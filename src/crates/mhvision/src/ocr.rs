//! OCR 引擎（PP-OCRv4 / ONNX Runtime）。
//!
//! 检测 + 识别 + CTC 贪心解码。和游戏无关的通用能力：换引擎只换这个文件。

use crate::prelude::*;
use crate::imgops::rgba_to_bgr_mat;
// ---------------------------------------------------------------------------
// OCR
// ---------------------------------------------------------------------------

/// The ONNX OCR engine (PP-OCRv4 det + rec via ONNX Runtime).
///
/// ONNX Runtime allows exactly one `Environment` per process; it is cached in
/// a `OnceLock` and shared by every engine instance.
pub struct OcrEngine {
    det: Box<ort::session::Session>,
    rec: Box<ort::session::Session>,
    keys: Vec<String>,
}

pub(crate) fn ort_environment() -> Result<&'static ort::environment::Environment> {
    static ENV: OnceLock<ort::environment::Environment> = OnceLock::new();
    if let Some(e) = ENV.get() {
        return Ok(e);
    }
    let built = ort::init()
        .with_name("mhvision")
        .build()
        .map_err(|e| VisionError::RuntimeUnavailable(e.to_string()))?;
    // ONNX Runtime's default severity is verbose and drowns our own logs.
    // Only real errors matter for a local, single-model pipeline.
    built.set_log_level(ort::logging::LogLevel::Error);
    Ok(ENV.get_or_init(|| built))
}

impl OcrEngine {
    /// Load the detection and recognition models from `models_dir`.
    ///
    /// Expected files: `ch_PP-OCRv4_det_infer.onnx`,
    /// `ch_PP-OCRv4_rec_infer.onnx`, `ppocr_keys_v1.txt`.
    pub fn new(models_dir: &Path) -> Result<Self> {
        let det_path = models_dir.join("ch_PP-OCRv4_det_infer.onnx");
        let rec_path = models_dir.join("ch_PP-OCRv4_rec_infer.onnx");
        let keys_path = models_dir.join("ppocr_keys_v1.txt");

        for p in [&det_path, &rec_path, &keys_path] {
            if !p.exists() {
                return Err(VisionError::ModelMissing(p.display().to_string()));
            }
        }

        let env = ort_environment()?;
        let det = ort::session::Session::builder(env)
            .and_then(|mut b| b.commit_from_file(det_path.to_string_lossy().as_ref()))
            .map_err(|e| VisionError::RuntimeUnavailable(e.to_string()))?;
        let rec = ort::session::Session::builder(env)
            .and_then(|mut b| b.commit_from_file(rec_path.to_string_lossy().as_ref()))
            .map_err(|e| VisionError::RuntimeUnavailable(e.to_string()))?;

        let keys_text = std::fs::read_to_string(&keys_path)
            .map_err(|e| VisionError::ModelMissing(format!("{}: {e}", keys_path.display())))?;
        // PaddleOCR's dict order: index 0 is the CTC blank, then one entry per
        // line, then a space at the end.
        let mut keys: Vec<String> = keys_text.lines().map(|l| l.to_string()).collect();
        keys.insert(0, String::new());
        keys.push(" ".to_string());

        Ok(Self { det: Box::new(det), rec: Box::new(rec), keys })
    }

    /// Recognize every text line in the image.
    pub fn recognize(&mut self, img: &image::RgbaImage) -> Result<Vec<OcrLine>> {
        let mat = rgba_to_bgr_mat(img)?;
        let boxes = self.detect_boxes(&mat, img.width() as i32, img.height() as i32)?;
        if boxes.is_empty() {
            return Ok(Vec::new());
        }

        let mut lines = Vec::with_capacity(boxes.len());
        for (x, y, w, h) in boxes {
            let region = crop_bgr(&mat, x, y, w, h)?;
            let (text, confidence) = self.recognize_region(&region)?;
            if !text.trim().is_empty() {
                lines.push(OcrLine {
                    text: text.trim().to_string(),
                    confidence,
                    rect: (x, y, w, h),
                });
            }
        }

        // Reading order: top-to-bottom, then left-to-right within a line band.
        lines.sort_by(|a, b| {
            let ay = a.rect.1;
            let by = b.rect.1;
            let row_tolerance = ((a.rect.3 + b.rect.3) / 4).max(8);
            if (ay - by).abs() <= row_tolerance {
                a.rect.0.cmp(&b.rect.0)
            } else {
                ay.cmp(&by)
            }
        });
        Ok(lines)
    }

    /// Run the detection model and turn the probability map into boxes.
    fn detect_boxes(
        &mut self,
        bgr: &opencv::core::Mat,
        orig_w: i32,
        orig_h: i32,
    ) -> Result<Vec<(i32, i32, i32, i32)>> {
        // PP-OCR det wants the long side a multiple of 32. We allow up to
        // 1280 px so small UI digits (stack counts, item levels) survive; a
        // tighter 960 cap lost most of them on 1280-wide captures.
        //
        // Never upscale: INTER_LINEAR upscaling of these panels smears the
        // 14 px labels enough to invent digits (法术防御 983 -> 1983). The
        // recognition model resizes each crop to height 48 on its own, which
        // already handles small text.
        let scale = (1280.0 / orig_w.max(orig_h).max(1) as f64).min(1.0);
        let (limit_w, limit_h) = (
            (((orig_w as f64 * scale) as i32 + 31) / 32 * 32).max(32),
            (((orig_h as f64 * scale) as i32 + 31) / 32 * 32).max(32),
        );
        let mut resized = opencv::core::Mat::default();
        opencv::imgproc::resize_def(
            bgr,
            &mut resized,
            opencv::core::Size::new(limit_w, limit_h),
        )
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;

        // Build the NCHW float tensor, normalised with PP-OCR's mean/std.
        let mut blob =
            ndarray::Array4::<f32>::zeros((1, 3, limit_h as usize, limit_w as usize));
        let bytes = resized.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
        for y in 0..limit_h as usize {
            for x in 0..limit_w as usize {
                let i = (y * limit_w as usize + x) * 3;
                if i + 2 >= bytes.len() {
                    continue;
                }
                // resized is BGR; the model wants RGB order.
                let r = bytes[i + 2] as f32 / 255.0;
                let g = bytes[i + 1] as f32 / 255.0;
                let b = bytes[i] as f32 / 255.0;
                blob[[0, 0, y, x]] = (r - 0.485) / 0.229;
                blob[[0, 1, y, x]] = (g - 0.456) / 0.224;
                blob[[0, 2, y, x]] = (b - 0.406) / 0.225;
            }
        }

        let tensor = ort::value::Tensor::from_array(blob)
            .map_err(|e| VisionError::Ocr(e.to_string()))?;
        let outputs = self
            .det
            .run(ort::inputs!["x" => tensor])
            .map_err(|e| VisionError::Ocr(format!("det inference: {e}")))?;

        let prob = outputs[0]
            .try_extract_array::<f32>()
            .map_err(|e| VisionError::Ocr(e.to_string()))?;
        let shape = prob.shape();
        if shape.len() < 4 {
            return Err(VisionError::Ocr(format!("unexpected det output shape {shape:?}")));
        }
        let (ph, pw) = (shape[2], shape[3]);

        let prob_map = prob
            .as_slice()
            .ok_or_else(|| VisionError::Ocr("det output not contiguous".into()))?;
        let scaled_w = orig_w as f64 / pw as f64;
        let scaled_h = orig_h as f64 / ph as f64;

        // Threshold, then find connected components row by row. A full
        // cv::connectedComponents call would be nicer, but this keeps the
        // dependency surface small and is fast enough on screenshots.
        let mut boxes = Vec::new();
        let mut visited = vec![false; ph * pw];
        for y0 in 0..ph {
            for x0 in 0..pw {
                let idx = y0 * pw + x0;
                if visited[idx] || prob_map[idx] < 0.22 {
                    continue;
                }
                // Iterative flood fill to collect one text region.
                let mut stack = vec![(x0, y0)];
                visited[idx] = true;
                let (mut min_x, mut max_x, mut min_y, mut max_y) = (x0, x0, y0, y0);
                let mut area = 0usize;
                while let Some((cx, cy)) = stack.pop() {
                    area += 1;
                    min_x = min_x.min(cx);
                    max_x = max_x.max(cx);
                    min_y = min_y.min(cy);
                    max_y = max_y.max(cy);
                    for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                        let nx = cx as i32 + dx;
                        let ny = cy as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= pw as i32 || ny >= ph as i32 {
                            continue;
                        }
                        let nidx = ny as usize * pw + nx as usize;
                        if visited[nidx] || prob_map[nidx] < 0.22 {
                            continue;
                        }
                        visited[nidx] = true;
                        stack.push((nx as usize, ny as usize));
                    }
                }
                if area < 4 {
                    continue;
                }
                // Unclip: expand the box so ascenders/descenders are included.
                let pad = (((max_y - min_y) as f64) * 0.4).max(3.0) as i32;
                let bx = ((min_x as f64 - pad as f64) * scaled_w).max(0.0) as i32;
                let by = ((min_y as f64 - pad as f64) * scaled_h).max(0.0) as i32;
                let bw = (((max_x - min_x + 1) as f64 + 2.0 * pad as f64) * scaled_w) as i32;
                let bh = (((max_y - min_y + 1) as f64 + 2.0 * pad as f64) * scaled_h) as i32;
                let bw = bw.min(orig_w - bx).max(1);
                let bh = bh.min(orig_h - by).max(1);
                if bw < 6 || bh < 5 {
                    continue;
                }
                boxes.push((bx, by, bw, bh));
            }
        }

        Ok(boxes)
    }

    /// Crop one detected region and run the recognition model on it.
    fn recognize_region(&mut self, bgr: &opencv::core::Mat) -> Result<(String, f64)> {
        // PP-OCR rec: fixed height 48, width proportional, capped.
        let h = 48;
        let ratio = bgr.cols() as f64 / bgr.rows().max(1) as f64;
        let w = ((h as f64 * ratio).ceil() as i32).clamp(16, 640);

        let mut resized = opencv::core::Mat::default();
        opencv::imgproc::resize_def(
            bgr,
            &mut resized,
            opencv::core::Size::new(w, h),
        )
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;

        let mut blob = ndarray::Array4::<f32>::zeros((1, 3, h as usize, w as usize));
        let bytes = resized.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 3;
                if i + 2 >= bytes.len() {
                    continue;
                }
                let r = bytes[i + 2] as f32 / 255.0;
                let g = bytes[i + 1] as f32 / 255.0;
                let b = bytes[i] as f32 / 255.0;
                blob[[0, 0, y, x]] = (r - 0.5) / 0.5;
                blob[[0, 1, y, x]] = (g - 0.5) / 0.5;
                blob[[0, 2, y, x]] = (b - 0.5) / 0.5;
            }
        }

        let tensor = ort::value::Tensor::from_array(blob)
            .map_err(|e| VisionError::Ocr(e.to_string()))?;
        let outputs = self
            .rec
            .run(ort::inputs!["x" => tensor])
            .map_err(|e| VisionError::Ocr(format!("rec inference: {e}")))?;

        let logits = outputs[0]
            .try_extract_array::<f32>()
            .map_err(|e| VisionError::Ocr(e.to_string()))?;
        let shape = logits.shape();
        if shape.len() != 3 {
            return Err(VisionError::Ocr(format!("unexpected rec output shape {shape:?}")));
        }
        let (steps, classes) = (shape[1], shape[2]);
        let data = logits
            .as_slice()
            .ok_or_else(|| VisionError::Ocr("rec output not contiguous".into()))?;

        Ok(ctc_greedy_decode(data, steps, classes, &self.keys))
    }
}

/// Crop a BGR region out of a Mat.
pub(crate) fn crop_bgr(
    bgr: &opencv::core::Mat,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<opencv::core::Mat> {
    let x = x.max(0).min(bgr.cols() - 1);
    let y = y.max(0).min(bgr.rows() - 1);
    let w = w.max(1).min(bgr.cols() - x);
    let h = h.max(1).min(bgr.rows() - y);
    let roi = opencv::core::Rect::new(x, y, w, h);
    let view = bgr.roi(roi).map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let mut out = opencv::core::Mat::default();
    view.copy_to(&mut out).map_err(|e| VisionError::OpenCv(e.to_string()))?;
    Ok(out)
}

/// CTC greedy decode used by PP-OCR's recogniser.
///
/// `data` is row-major `steps * classes`; argmax per step, collapse repeats,
/// then drop the blank class (index 0).
pub(crate) fn ctc_greedy_decode(
    data: &[f32],
    steps: usize,
    classes: usize,
    keys: &[String],
) -> (String, f64) {
    let mut text = String::new();
    let mut confidences = Vec::new();
    let mut prev = usize::MAX;

    for step in 0..steps {
        let base = step * classes;
        if base + classes > data.len() {
            break;
        }
        let mut best_idx = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        for c in 0..classes {
            let v = data[base + c];
            if v > best_val {
                best_val = v;
                best_idx = c;
            }
        }
        if best_idx != 0 && best_idx != prev {
            if let Some(key) = keys.get(best_idx) {
                text.push_str(key);
            }
            confidences.push(best_val as f64);
        }
        prev = best_idx;
    }

    let confidence = if confidences.is_empty() {
        0.0
    } else {
        confidences.iter().sum::<f64>() / confidences.len() as f64
    };
    (text, confidence)
}
