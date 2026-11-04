//! OCR 引擎（PP-OCRv4 / ONNX Runtime）。
//!
//! 检测 + 识别 + CTC 贪心解码。和游戏无关的通用能力：换引擎只换这个文件。
//!
//! ## 两个后处理实现并存
//!
//! 检测模型输出的是"每个像素属于文字的概率"，概率图怎么变成文本框有两套实现：
//!
//! - [`DetPostprocess::Legacy`]：固定阈值 + 四邻域漫游 + 按高度的矩形外扩。
//!   轻量、无依赖，对规整横排文字够用。
//! - [`DetPostprocess::Db`]：标准 DB 后处理（二值化 → 轮廓 → 最小外接矩形 →
//!   按面积/周长 unclip → 平均概率过滤）。斜体、贴得很近的中文、带描边的
//!   游戏字体更稳。
//!
//! 两套都保留不是历史包袱：换检测后处理会连带改变行数、行高与阅读顺序，
//! 而字段提取是按行做启发式的。默认值必须有真实截图上的对照数据撑着，
//! 不能凭"标准实现更强"就切。切换开关是 [`OcrConfig::det`]。
//!
//! ## 识别按宽度分桶批量推理
//!
//! 识别模型输入是 `[N, 3, 48, W]`，N 与 W 都是动态维度（paddle2onnx 导出的
//! `p2o.DynamicDimension.*`）。逐框推理时每个框一次 `run()`，多框截图里大部分
//! 时间花在调用开销上。这里把裁切图按"补齐后的宽度"分桶，同桶内凑一批一次
//! 推理，再按原索引还原。批次上限由 [`OcrConfig::batch_size`] 控制：批越大
//! 峰值内存越高（每张 3×48×W×4 字节），且太宽的同桶批次会白算填充区域。

use std::collections::BTreeMap;

use crate::imgops::rgba_to_bgr_mat;
use crate::prelude::*;
use opencv::core::{Mat, Scalar, Size, Vector};
use opencv::imgproc;

/// 识别模型固定输入高度。
const REC_HEIGHT: i32 = 48;
/// 识别输入宽度上限（与原实现一致，超长行按此截断后仍能读出主体）。
const REC_MAX_WIDTH: i32 = 640;
/// 识别输入宽度下限。
const REC_MIN_WIDTH: i32 = 16;
/// 分桶对齐粒度：宽度向上补齐到该值的整数倍，同桶内填充浪费 < 32 px。
const PAD_ALIGN: i32 = 32;

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 检测后处理实现。默认值由真实截图 A/B 结论决定，见本文件头部说明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetPostprocess {
    /// 四邻域漫游 + 矩形外扩（0.16 及之前的行为）。
    Legacy,
    /// 轮廓 + 最小外接矩形 + unclip（标准 DB 后处理）。
    Db,
}

impl DetPostprocess {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "legacy" | "old" | "0" => Some(Self::Legacy),
            "db" | "contour" | "1" => Some(Self::Db),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Db => "db",
        }
    }
}

/// OCR 运行时配置。
///
/// 默认值偏"低资源"：识别是交互路径（点一下等一两秒），不是批处理，占满所有
/// 核心换来的若干百毫秒不值得让整机变卡。
#[derive(Debug, Clone)]
pub struct OcrConfig {
    /// 检测后处理实现。
    pub det: DetPostprocess,
    /// 识别批大小；1 = 逐框推理（用于对照）。
    pub batch_size: usize,
    /// ONNX Runtime 算子内并行线程数；0 = 交给 ORT 默认。
    pub intra_threads: usize,
    /// 检测输入长边上限。超过则等比缩小，不放大。
    pub max_side: u32,
    /// 概率二值化阈值（DB 用）。
    pub bin_thresh: f32,
    /// 文本框平均概率下限（DB 用）。
    pub box_thresh: f32,
    /// unclip 扩张系数（DB 用）。
    pub unclip_ratio: f32,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            det: DetPostprocess::Db,
            // 批大小与线程数的默认值是量出来的，见 crate 文档：批 4 最优，
            // 线程 4 在本机（6 核 12 线程）比 ORT 默认的 12 线程快 24%。
            batch_size: 4,
            intra_threads: 4,
            max_side: 1280,
            bin_thresh: 0.30,
            box_thresh: 0.60,
            unclip_ratio: 1.5,
        }
    }
}

impl OcrConfig {
    /// 环境变量覆盖，给 A/B 实测和排障用（界面只暴露三档，不暴露细项）。
    ///
    /// | 变量 | 作用 |
    /// |---|---|
    /// | `MH_OCR_DET=legacy\|db` | 检测后处理 |
    /// | `MH_OCR_BATCH=1..16` | 识别批大小 |
    /// | `MH_OCR_THREADS=0..N` | ORT 算子内线程数 |
    /// | `MH_OCR_MAX_SIDE=…` | 检测输入长边上限 |
    /// | `MH_OCR_BOX_THRESH` / `MH_OCR_BIN_THRESH` / `MH_OCR_UNCLIP` | DB 阈值 |
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("MH_OCR_DET") {
            if let Some(d) = DetPostprocess::parse(&v) {
                cfg.det = d;
            }
        }
        if let Some(v) = env_u64("MH_OCR_BATCH") {
            cfg.batch_size = (v as usize).clamp(1, 16);
        }
        if let Some(v) = env_u64("MH_OCR_THREADS") {
            cfg.intra_threads = (v as usize).min(64);
        }
        if let Some(v) = env_u64("MH_OCR_MAX_SIDE") {
            cfg.max_side = (v as u32).clamp(256, 4096);
        }
        if let Some(v) = env_f32("MH_OCR_BIN_THRESH") {
            cfg.bin_thresh = v.clamp(0.05, 0.95);
        }
        if let Some(v) = env_f32("MH_OCR_BOX_THRESH") {
            cfg.box_thresh = v.clamp(0.05, 0.99);
        }
        if let Some(v) = env_f32("MH_OCR_UNCLIP") {
            cfg.unclip_ratio = v.clamp(0.0, 5.0);
        }
        cfg
    }

    /// 一行人类可读的摘要，进日志和 `version` 的返回。
    pub fn describe(&self) -> String {
        format!(
            "det={} batch={} threads={} maxSide={} binThresh={:.2} boxThresh={:.2} unclip={:.2}",
            self.det.as_str(),
            self.batch_size,
            if self.intra_threads == 0 {
                "auto".to_string()
            } else {
                self.intra_threads.to_string()
            },
            self.max_side,
            self.bin_thresh,
            self.box_thresh,
            self.unclip_ratio
        )
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.trim().parse::<u64>().ok())
}

fn env_f32(key: &str) -> Option<f32> {
    std::env::var(key).ok().and_then(|v| v.trim().parse::<f32>().ok())
}

/// 一次识别的分段耗时（毫秒）。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrTiming {
    /// 检测阶段：整图推理 + 后处理出框。
    pub det_ms: f64,
    /// 识别阶段：裁切 + 分批推理 + CTC 解码。
    pub rec_ms: f64,
    /// 检测出的文本框数量。
    pub boxes: usize,
}

// ---------------------------------------------------------------------------
// 引擎
// ---------------------------------------------------------------------------

/// The ONNX OCR engine (PP-OCRv4 det + rec via ONNX Runtime).
///
/// ONNX Runtime allows exactly one `Environment` per process; it is cached in
/// a `OnceLock` and shared by every engine instance.
pub struct OcrEngine {
    det: Box<ort::session::Session>,
    rec: Box<ort::session::Session>,
    keys: Vec<String>,
    cfg: OcrConfig,
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

/// Build one session, pinning the intra-op thread count.
///
/// ORT 默认会用满物理核心。对一个"点一下等一秒"的桌面工具来说，那会把整机
/// （尤其同时开着模拟器/编译任务时）挤到卡顿，收益也就几百毫秒。
fn build_session(
    env: &ort::environment::Environment,
    path: &Path,
    intra_threads: usize,
) -> Result<ort::session::Session> {
    let mut builder = ort::session::Session::builder(env)
        .map_err(|e| VisionError::RuntimeUnavailable(e.to_string()))?;
    if intra_threads > 0 {
        builder = builder
            .with_intra_threads(intra_threads)
            .map_err(|e| VisionError::RuntimeUnavailable(e.to_string()))?;
    }
    builder
        .commit_from_file(path.to_string_lossy().as_ref())
        .map_err(|e| VisionError::RuntimeUnavailable(e.to_string()))
}

impl OcrEngine {
    /// Load the models with the default (env-overridable) configuration.
    pub fn new(models_dir: &Path) -> Result<Self> {
        Self::with_config(models_dir, OcrConfig::from_env())
    }

    /// Load the detection and recognition models from `models_dir`.
    ///
    /// Expected files: `ch_PP-OCRv4_det_infer.onnx`,
    /// `ch_PP-OCRv4_rec_infer.onnx`, `ppocr_keys_v1.txt`.
    pub fn with_config(models_dir: &Path, cfg: OcrConfig) -> Result<Self> {
        let det_path = models_dir.join("ch_PP-OCRv4_det_infer.onnx");
        let rec_path = models_dir.join("ch_PP-OCRv4_rec_infer.onnx");
        let keys_path = models_dir.join("ppocr_keys_v1.txt");

        for p in [&det_path, &rec_path, &keys_path] {
            if !p.exists() {
                return Err(VisionError::ModelMissing(p.display().to_string()));
            }
        }

        let env = ort_environment()?;
        let det = build_session(env, &det_path, cfg.intra_threads)?;
        let rec = build_session(env, &rec_path, cfg.intra_threads)?;

        let keys_text = std::fs::read_to_string(&keys_path)
            .map_err(|e| VisionError::ModelMissing(format!("{}: {e}", keys_path.display())))?;
        // PaddleOCR's dict order: index 0 is the CTC blank, then one entry per
        // line, then a space at the end.
        let mut keys: Vec<String> = keys_text.lines().map(|l| l.to_string()).collect();
        keys.insert(0, String::new());
        keys.push(" ".to_string());

        tracing::info!("OCR engine loaded: {}", cfg.describe());
        Ok(Self { det: Box::new(det), rec: Box::new(rec), keys, cfg })
    }

    /// 当前生效的配置（`version` 会回报给界面与脚本）。
    pub fn config(&self) -> &OcrConfig {
        &self.cfg
    }

    /// Recognize every text line in the image.
    pub fn recognize(&mut self, img: &image::RgbaImage) -> Result<Vec<OcrLine>> {
        self.recognize_with_timing(img).map(|(lines, _)| lines)
    }

    /// 带分段计时的识别。
    ///
    /// "识别慢"是这类工具最常见的抱怨，但慢在检测（固定 1280 宽的整图推理）
    /// 还是识别（逐行 48 高的窄图）决定了该调哪个参数。把它做成返回值而不是
    /// 日志，测试脚本和基准命令才能给出可比较的数字。
    pub fn recognize_with_timing(
        &mut self,
        img: &image::RgbaImage,
    ) -> Result<(Vec<OcrLine>, OcrTiming)> {
        let started = std::time::Instant::now();
        let mat = rgba_to_bgr_mat(img)?;
        let boxes = self.detect_boxes(&mat, img.width() as i32, img.height() as i32)?;
        let det_ms = started.elapsed().as_secs_f64() * 1000.0;
        if boxes.is_empty() {
            return Ok((
                Vec::new(),
                OcrTiming { det_ms, rec_ms: 0.0, boxes: 0 },
            ));
        }

        let rec_started = std::time::Instant::now();
        let texts = self.recognize_boxes(&mat, &boxes)?;
        let rec_ms = rec_started.elapsed().as_secs_f64() * 1000.0;

        let mut lines = Vec::with_capacity(boxes.len());
        for ((x, y, w, h), (text, confidence)) in boxes.iter().zip(texts) {
            if !text.trim().is_empty() {
                lines.push(OcrLine {
                    text: text.trim().to_string(),
                    confidence,
                    rect: (*x, *y, *w, *h),
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
        Ok((
            lines,
            OcrTiming { det_ms, rec_ms, boxes: boxes.len() },
        ))
    }

    /// Run the detection model and turn the probability map into boxes.
    fn detect_boxes(
        &mut self,
        bgr: &Mat,
        orig_w: i32,
        orig_h: i32,
    ) -> Result<Vec<(i32, i32, i32, i32)>> {
        // PP-OCR det wants the long side a multiple of 32. We allow up to
        // `max_side` (1280 default) so small UI digits (stack counts, item
        // levels) survive; a tighter 960 cap lost most of them on 1280-wide
        // captures.
        //
        // Never upscale: INTER_LINEAR upscaling of these panels smears the
        // 14 px labels enough to invent digits (法术防御 983 -> 1983). The
        // recognition model resizes each crop to height 48 on its own, which
        // already handles small text.
        let max_side = self.cfg.max_side.max(256) as f64;
        let scale = (max_side / orig_w.max(orig_h).max(1) as f64).min(1.0);
        let (limit_w, limit_h) = (
            (((orig_w as f64 * scale) as i32 + 31) / 32 * 32).max(32),
            (((orig_h as f64 * scale) as i32 + 31) / 32 * 32).max(32),
        );
        let mut resized = Mat::default();
        imgproc::resize_def(bgr, &mut resized, Size::new(limit_w, limit_h))
            .map_err(|e| VisionError::OpenCv(e.to_string()))?;

        // Build the NCHW float tensor, normalised with PP-OCR's mean/std.
        let mut blob = ndarray::Array4::<f32>::zeros((1, 3, limit_h as usize, limit_w as usize));
        let bytes = resized.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
        let pixels = (limit_w as usize) * (limit_h as usize);
        for (i, px) in bytes.chunks_exact(3).take(pixels).enumerate() {
            let (y, x) = (i / limit_w as usize, i % limit_w as usize);
            // resized is BGR; the model wants RGB order.
            let r = px[2] as f32 / 255.0;
            let g = px[1] as f32 / 255.0;
            let b = px[0] as f32 / 255.0;
            blob[[0, 0, y, x]] = (r - 0.485) / 0.229;
            blob[[0, 1, y, x]] = (g - 0.456) / 0.224;
            blob[[0, 2, y, x]] = (b - 0.406) / 0.225;
        }

        let tensor =
            ort::value::Tensor::from_array(blob).map_err(|e| VisionError::Ocr(e.to_string()))?;
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

        Ok(match self.cfg.det {
            DetPostprocess::Legacy => legacy_postprocess(prob_map, pw, ph, orig_w, orig_h),
            DetPostprocess::Db => db_postprocess(prob_map, pw, ph, orig_w, orig_h, &self.cfg)?,
        })
    }

    /// Crop every box and read them in width-bucketed batches.
    fn recognize_boxes(
        &mut self,
        mat: &Mat,
        boxes: &[(i32, i32, i32, i32)],
    ) -> Result<Vec<(String, f64)>> {
        let h = REC_HEIGHT;

        // 1. 裁切并统一到高 48。宽度保留比例，太长的截到上限。
        let mut crops: Vec<(i32, Mat)> = Vec::with_capacity(boxes.len());
        for &(x, y, w, bh) in boxes {
            let region = crop_bgr(mat, x, y, w, bh)?;
            let ratio = region.cols() as f64 / region.rows().max(1) as f64;
            let rw = ((h as f64 * ratio).ceil() as i32).clamp(REC_MIN_WIDTH, REC_MAX_WIDTH);
            let mut resized = Mat::default();
            imgproc::resize_def(&region, &mut resized, Size::new(rw, h))
                .map_err(|e| VisionError::OpenCv(e.to_string()))?;
            crops.push((rw, resized));
        }

        // 2. 按"补齐后的宽度"分桶：同桶内填充最少，才能凑批而不白算。
        let mut buckets: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
        for (i, (rw, _)) in crops.iter().enumerate() {
            let bucket = (((rw + PAD_ALIGN - 1) / PAD_ALIGN) * PAD_ALIGN).max(PAD_ALIGN);
            buckets.entry(bucket).or_default().push(i);
        }

        let mut out = vec![(String::new(), 0.0); boxes.len()];
        let batch = self.cfg.batch_size.max(1);
        for (bucket, group) in buckets {
            for chunk in group.chunks(batch) {
                self.run_rec_batch(&crops, chunk, bucket, h, &mut out)?;
            }
        }
        Ok(out)
    }

    /// One `rec` inference for a chunk of crops padded to `bucket` width.
    fn run_rec_batch(
        &mut self,
        crops: &[(i32, Mat)],
        chunk: &[usize],
        bucket: i32,
        h: i32,
        out: &mut [(String, f64)],
    ) -> Result<()> {
        let n = chunk.len();
        let (hu, wu) = (h as usize, bucket as usize);
        let mut blob = ndarray::Array4::<f32>::zeros((n, 3, hu, wu));

        for (slot, &item) in chunk.iter().enumerate() {
            let (rw, ref src) = crops[item];
            let bytes = src.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
            let usable = (rw as usize).min(wu);
            for y in 0..hu {
                for x in 0..usable {
                    let i = (y * rw as usize + x) * 3;
                    if i + 2 >= bytes.len() {
                        continue;
                    }
                    let r = bytes[i + 2] as f32 / 255.0;
                    let g = bytes[i + 1] as f32 / 255.0;
                    let b = bytes[i] as f32 / 255.0;
                    blob[[slot, 0, y, x]] = (r - 0.5) / 0.5;
                    blob[[slot, 1, y, x]] = (g - 0.5) / 0.5;
                    blob[[slot, 2, y, x]] = (b - 0.5) / 0.5;
                }
                // 右侧补齐保持 0：与 PaddleOCR 批量推理的填充一致（归一化后的
                // 0 对应灰度 127.5，不是纯黑，不会把模型带偏）。
            }
        }

        let tensor =
            ort::value::Tensor::from_array(blob).map_err(|e| VisionError::Ocr(e.to_string()))?;
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
        let (got_n, steps, classes) = (shape[0], shape[1], shape[2]);
        if got_n != n {
            return Err(VisionError::Ocr(format!(
                "rec batch mismatch: asked {n}, got {got_n}"
            )));
        }
        let data = logits
            .as_slice()
            .ok_or_else(|| VisionError::Ocr("rec output not contiguous".into()))?;

        for (slot, &item) in chunk.iter().enumerate() {
            let base = slot * steps * classes;
            out[item] = ctc_greedy_decode(&data[base..], steps, classes, &self.keys);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 检测后处理：两套实现
// ---------------------------------------------------------------------------

/// 0.16 及之前的实现：固定阈值 + 四邻域漫游 + 按高度 40% 的矩形外扩。
///
/// 保留它是为了对照：它不是"错的"，只是对斜体/贴字/描边的鲁棒性没底。
fn legacy_postprocess(
    prob: &[f32],
    pw: usize,
    ph: usize,
    orig_w: i32,
    orig_h: i32,
) -> Vec<(i32, i32, i32, i32)> {
    const THRESH: f32 = 0.22;
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
            if visited[idx] || prob[idx] < THRESH {
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
                    if visited[nidx] || prob[nidx] < THRESH {
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
    boxes
}

/// 标准 DB 后处理：二值图 → 外轮廓 → 最小外接矩形 → 面积/周长 unclip。
///
/// 与 legacy 的关键差别：
/// - 用 `minAreaRect` 记录文字走向，按**旋转矩形**的外接范围出框，斜体/轻微
///   倾斜的字不会被矩形外扩算出一个偏大的框；
/// - 扩张量按 `area * unclip_ratio / perimeter` 计算，随字号自适应，而不是
///   固定按高度比例；
/// - 用框内平均概率过滤，低置信的噪点不再变成一行乱码。
fn db_postprocess(
    prob: &[f32],
    pw: usize,
    ph: usize,
    orig_w: i32,
    orig_h: i32,
    cfg: &OcrConfig,
) -> Result<Vec<(i32, i32, i32, i32)>> {
    let mut mask = Mat::new_rows_cols_with_default(
        ph as i32,
        pw as i32,
        opencv::core::CV_8UC1,
        Scalar::all(0.0),
    )
    .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    {
        let dst = mask.data_bytes_mut().map_err(|e| VisionError::OpenCv(e.to_string()))?;
        for (i, &v) in prob.iter().take(pw * ph).enumerate() {
            if v >= cfg.bin_thresh {
                dst[i] = 255;
            }
        }
    }

    let mut contours: Vector<Vector<opencv::core::Point>> = Vector::new();
    let mut hierarchy = Mat::default();
    imgproc::find_contours_with_hierarchy_def(
        &mask,
        &mut contours,
        &mut hierarchy,
        imgproc::RETR_EXTERNAL,
        imgproc::CHAIN_APPROX_SIMPLE,
    )
    .map_err(|e| VisionError::OpenCv(e.to_string()))?;

    let scaled_w = orig_w as f64 / pw as f64;
    let scaled_h = orig_h as f64 / ph as f64;
    let mut boxes = Vec::with_capacity(contours.len());

    for i in 0..contours.len() {
        let contour = contours.get(i).map_err(|e| VisionError::OpenCv(e.to_string()))?;
        let rect =
            imgproc::min_area_rect(&contour).map_err(|e| VisionError::OpenCv(e.to_string()))?;
        let (mut rw, mut rh) = (rect.size.width as f64, rect.size.height as f64);
        // 单像素噪点、拉开距离的标点经常只剩 2-3 px 的碎块。
        if rw < 3.0 || rh < 3.0 {
            continue;
        }

        let bounds =
            imgproc::bounding_rect(&contour).map_err(|e| VisionError::OpenCv(e.to_string()))?;
        let score = mean_prob(prob, pw, ph, bounds.x, bounds.y, bounds.width, bounds.height);
        if score < cfg.box_thresh as f64 {
            continue;
        }

        let area = imgproc::contour_area(&contour, false)
            .map_err(|e| VisionError::OpenCv(e.to_string()))?
            .abs();
        let perimeter = 2.0 * (rw + rh);
        let dist = if perimeter > 0.0 {
            area * cfg.unclip_ratio as f64 / perimeter
        } else {
            0.0
        };
        rw += 2.0 * dist;
        rh += 2.0 * dist;

        // 旋转矩形在轴对齐方向上的半宽/半高，直接给出外包框。
        let angle = (rect.angle as f64).to_radians();
        let (cos, sin) = (angle.cos().abs(), angle.sin().abs());
        let half_w = rw * 0.5 * cos + rh * 0.5 * sin;
        let half_h = rw * 0.5 * sin + rh * 0.5 * cos;
        let (cx, cy) = (rect.center.x as f64, rect.center.y as f64);

        let bx = ((cx - half_w) * scaled_w).max(0.0) as i32;
        let by = ((cy - half_h) * scaled_h).max(0.0) as i32;
        let bw = ((2.0 * half_w) * scaled_w) as i32;
        let bh = ((2.0 * half_h) * scaled_h) as i32;
        let bw = bw.min(orig_w - bx).max(1);
        let bh = bh.min(orig_h - by).max(1);
        if bw < 6 || bh < 5 {
            continue;
        }
        boxes.push((bx, by, bw, bh));
    }

    Ok(boxes)
}

/// 矩形区域内概率的平均值（DB 的 box score）。
fn mean_prob(prob: &[f32], pw: usize, ph: usize, x: i32, y: i32, w: i32, h: i32) -> f64 {
    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = ((x + w).max(0) as usize).min(pw);
    let y1 = ((y + h).max(0) as usize).min(ph);
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for row in y0..y1 {
        let base = row * pw;
        for col in x0..x1 {
            sum += prob[base + col] as f64;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

// ---------------------------------------------------------------------------
// 裁切与解码
// ---------------------------------------------------------------------------

/// Crop a BGR region out of a Mat.
pub(crate) fn crop_bgr(bgr: &Mat, x: i32, y: i32, w: i32, h: i32) -> Result<Mat> {
    let x = x.max(0).min(bgr.cols() - 1);
    let y = y.max(0).min(bgr.rows() - 1);
    let w = w.max(1).min(bgr.cols() - x);
    let h = h.max(1).min(bgr.rows() - y);
    let roi = opencv::core::Rect::new(x, y, w, h);
    let view = bgr.roi(roi).map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let mut out = Mat::default();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<String> {
        // 0 = blank, 1 = "你", 2 = "好"
        vec![String::new(), "你".into(), "好".into()]
    }

    #[test]
    fn ctc_collapses_repeats_and_drops_blanks() {
        let classes = 3;
        // 步序列: 你 你 blank 好
        let data = vec![
            0.0, 0.9, 0.1, //
            0.0, 0.8, 0.2, //
            0.9, 0.05, 0.05, //
            0.0, 0.1, 0.9,
        ];
        let (text, conf) = ctc_greedy_decode(&data, 4, classes, &keys());
        assert_eq!(text, "你好");
        assert!((conf - 0.9).abs() < 1e-6, "confidence averages the kept steps");
    }

    #[test]
    fn ctc_handles_short_output_rows() {
        // 输出行比 steps*classes 短时不能 panic（模型换版本时见过）。
        let data = vec![0.0, 0.9, 0.1];
        let (text, conf) = ctc_greedy_decode(&data, 4, 3, &keys());
        assert_eq!(text, "你");
        assert!(conf > 0.0);
    }

    #[test]
    fn det_postprocess_parses_both_spellings() {
        assert_eq!(DetPostprocess::parse("db"), Some(DetPostprocess::Db));
        assert_eq!(DetPostprocess::parse(" LEGACY "), Some(DetPostprocess::Legacy));
        assert_eq!(DetPostprocess::parse("nope"), None);
    }

    #[test]
    fn db_postprocess_finds_two_separated_regions() {
        // 40x20 的概率图，两块 10x6 的高概率矩形，中间留 6 px 间隔。
        let (pw, ph) = (40usize, 20usize);
        let mut prob = vec![0.01f32; pw * ph];
        for y in 4..10 {
            for x in 2..12 {
                prob[y * pw + x] = 0.95;
            }
            for x in 18..28 {
                prob[y * pw + x] = 0.95;
            }
        }
        let cfg = OcrConfig { det: DetPostprocess::Db, ..OcrConfig::default() };
        let boxes = db_postprocess(&prob, pw, ph, pw as i32 * 2, ph as i32 * 2, &cfg).unwrap();
        assert_eq!(boxes.len(), 2, "两个分离区域应各出一个框: {boxes:?}");

        // 坐标按 2 倍缩放回原图。
        let mut xs: Vec<i32> = boxes.iter().map(|b| b.0).collect();
        xs.sort();
        assert!(xs[0] < 24 && xs[1] >= 24, "框的横坐标应落在各自区域附近: {xs:?}");
    }

    #[test]
    fn db_postprocess_drops_low_score_blobs() {
        let (pw, ph) = (20usize, 10usize);
        // 大面积 0.35 的区域：过二值化，但平均概率低于 0.6 的框阈值。
        let mut prob = vec![0.0f32; pw * ph];
        for y in 2..8 {
            for x in 2..18 {
                prob[y * pw + x] = 0.35;
            }
        }
        let cfg = OcrConfig {
            det: DetPostprocess::Db,
            bin_thresh: 0.30,
            box_thresh: 0.60,
            ..OcrConfig::default()
        };
        let boxes = db_postprocess(&prob, pw, ph, pw as i32, ph as i32, &cfg).unwrap();
        assert!(boxes.is_empty(), "低置信区域应被平均概率过滤: {boxes:?}");
    }
}
