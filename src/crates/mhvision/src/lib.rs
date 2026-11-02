//! Image pipeline: screenshot classification, OCR, and perceptual features.
//!
//! Ownership: this crate talks to OpenCV and ONNX Runtime directly. It must
//! stay usable from a plain CLI (no WinUI, no named pipes) so the recogniser
//! can be exercised on its own.
//!
//! The public surface below is the contract that `mhservice` and `mhcli`
//! compile against. Implementations may change freely; signatures may not.

use mhcore::{
    AttributeValue, Character, CombatStats, Currencies, EquipmentItem, EvidenceKind, IdentityCandidate,
    IdentityRef, IdentitySource, InventoryItem, MatchEvidence, PanelFingerprint, RecognitionResult,
    ScreenKind,
};
use opencv::prelude::*;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error)]
pub enum VisionError {
    #[error("failed to decode image: {0}")]
    Decode(String),
    #[error("OpenCV error: {0}")]
    OpenCv(String),
    #[error("OCR error: {0}")]
    Ocr(String),
    #[error("model not found: {0}")]
    ModelMissing(String),
    #[error("onnx runtime not loadable: {0}")]
    RuntimeUnavailable(String),
}

pub type Result<T> = std::result::Result<T, VisionError>;

/// One recognised text line from the OCR engine, in screen coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub text: String,
    /// 0.0 - 1.0 recognition confidence.
    pub confidence: f64,
    /// Axis-aligned bounding box: (x, y, width, height) in pixels.
    pub rect: (i32, i32, i32, i32),
}

impl OcrLine {
    pub fn center_x(&self) -> f64 {
        self.rect.0 as f64 + self.rect.2 as f64 / 2.0
    }
    pub fn center_y(&self) -> f64 {
        self.rect.1 as f64 + self.rect.3 as f64 / 2.0
    }
}

/// Perceptual fingerprint of a screenshot, cached so repeated uploads of the
/// same image skip the expensive path.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageFeatures {
    /// 64-bit difference hash.
    pub phash: u64,
    /// ORB descriptors flattened row-major (32 bytes each).
    pub orb: Vec<u8>,
    pub orb_keypoints: Vec<(f32, f32)>,
    /// Hue histogram, 32 bins, normalised to sum to 1.
    pub hsv_hist: Vec<f32>,
    pub width: u32,
    pub height: u32,
}

/// Result of fingerprinting: how similar two screenshots are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    /// 1.0 when the 64-bit hashes are identical; 0.0 when fully different.
    pub phash: f64,
    /// Ratio of ORB descriptor matches in 0.0..=1.0.
    pub orb: f64,
    /// Cosine similarity of the HSV histograms in 0.0..=1.0.
    pub hsv: f64,
}

impl Similarity {
    /// Weighted blend used to decide whether two screenshots show the same
    /// character panel. Weights favour the robust structural signals.
    pub fn combined(&self) -> f64 {
        0.5 * self.phash + 0.3 * self.orb + 0.2 * self.hsv
    }
}

// ---------------------------------------------------------------------------
// Image decoding
// ---------------------------------------------------------------------------

/// Decode raw file bytes (PNG/JPEG/BMP/WebP) into RGBA pixels.
pub fn decode_image(bytes: &[u8]) -> Result<image::RgbaImage> {
    let dyn_img = image::load_from_memory(bytes)
        .map_err(|e| VisionError::Decode(e.to_string()))?;
    Ok(dyn_img.to_rgba8())
}

/// Convert an RGBA image into an OpenCV BGR `Mat`.
fn rgba_to_bgr_mat(img: &image::RgbaImage) -> Result<opencv::core::Mat> {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let mut bgr = Vec::with_capacity((w * h * 3) as usize);
    for p in img.pixels() {
        // image gives RGBA; OpenCV's default channel order is BGR.
        bgr.push(p[2]);
        bgr.push(p[1]);
        bgr.push(p[0]);
    }
    let flat = opencv::core::Mat::from_slice(&bgr)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    // `from_slice` borrows the Vec, so clone into an owned Mat and reshape it.
    let owned = flat
        .try_clone()
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let mut out = opencv::core::Mat::default();
    owned
        .reshape(3, h)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?
        .copy_to(&mut out)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    if out.cols() != w || out.rows() != h {
        return Err(VisionError::OpenCv(format!(
            "mat shape mismatch: got {}x{}, wanted {w}x{h}",
            out.cols(),
            out.rows()
        )));
    }
    Ok(out)
}

/// Convert an OpenCV grayscale `Mat` into a plain Vec<u8> of length w*h.
fn gray_mat_to_vec(mat: &opencv::core::Mat) -> Result<(Vec<u8>, i32, i32)> {
    let rows = mat.rows();
    let cols = mat.cols();
    // Mat::data_bytes is the safest accessor for a CV_8U single-channel Mat.
    let bytes = mat.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
    debug_assert!(bytes.len() >= (rows * cols) as usize);
    Ok((bytes[..(rows * cols) as usize].to_vec(), rows, cols))
}

fn to_gray(mat: &opencv::core::Mat) -> Result<opencv::core::Mat> {
    let mut gray = opencv::core::Mat::default();
    opencv::imgproc::cvt_color_def(mat, &mut gray, opencv::imgproc::COLOR_BGR2GRAY)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    Ok(gray)
}

// ---------------------------------------------------------------------------
// Perceptual features
// ---------------------------------------------------------------------------

/// Compute the perceptual fingerprint of an image.
pub fn extract_features(img: &image::RgbaImage) -> Result<ImageFeatures> {
    let mat = rgba_to_bgr_mat(img)?;
    let gray = to_gray(&mat)?;

    let phash = compute_phash(&gray)?;
    let (orb, orb_keypoints) = compute_orb(&gray)?;
    let hsv_hist = compute_hsv_hist(&mat)?;

    Ok(ImageFeatures {
        phash,
        orb,
        orb_keypoints,
        hsv_hist,
        width: img.width(),
        height: img.height(),
    })
}

/// Difference hash: resize to 9x8, compare each pixel with its right neighbour.
fn compute_phash(gray: &opencv::core::Mat) -> Result<u64> {
    let mut small = opencv::core::Mat::default();
    opencv::imgproc::resize_def(
        gray,
        &mut small,
        opencv::core::Size::new(9, 8),
    )
    .map_err(|e| VisionError::OpenCv(e.to_string()))?;

    let (px, rows, cols) = gray_mat_to_vec(&small)?;
    if rows != 8 || cols != 9 {
        return Err(VisionError::OpenCv(format!("phash resize gave {cols}x{rows}")));
    }

    let mut hash = 0u64;
    let mut bit = 0u32;
    for row in 0..8 {
        for col in 0..8 {
            let left = px[(row * 9 + col) as usize];
            let right = px[(row * 9 + col + 1) as usize];
            if left > right {
                hash |= 1u64 << bit;
            }
            bit += 1;
        }
    }
    Ok(hash)
}

/// ORB keypoints + descriptors, flattened to raw 32-byte rows.
fn compute_orb(gray: &opencv::core::Mat) -> Result<(Vec<u8>, Vec<(f32, f32)>)> {
    let mut orb = opencv::features2d::ORB::create_def()
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let mut keypoints = opencv::core::Vector::<opencv::core::KeyPoint>::new();
    let mut descriptors = opencv::core::Mat::default();
    orb.detect_and_compute_def(gray, &opencv::core::no_array(), &mut keypoints, &mut descriptors)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;

    let coords: Vec<(f32, f32)> = keypoints
        .iter()
        .map(|kp| (kp.pt().x as f32, kp.pt().y as f32))
        .collect();

    let desc = if descriptors.empty() {
        Vec::new()
    } else {
        descriptors
            .data_bytes()
            .map_err(|e| VisionError::OpenCv(e.to_string()))?
            .to_vec()
    };
    Ok((desc, coords))
}

/// 32-bin hue histogram normalised to sum to 1.
fn compute_hsv_hist(bgr: &opencv::core::Mat) -> Result<Vec<f32>> {
    let mut hsv = opencv::core::Mat::default();
    opencv::imgproc::cvt_color_def(bgr, &mut hsv, opencv::imgproc::COLOR_BGR2HSV)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;

    // Hue occupies 0..180 in OpenCV's 8-bit HSV; fold into 32 bins of 6 hues.
    let mut hist = vec![0f32; 32];
    let data = hsv.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
    if data.len() < 3 {
        return Ok(hist);
    }
    let mut count = 0usize;
    for chunk in data.chunks_exact(3) {
        let bin = ((chunk[0] as usize) / 6).min(31);
        hist[bin] += 1.0;
        count += 1;
    }
    if count > 0 {
        for v in hist.iter_mut() {
            *v /= count as f32;
        }
    }
    Ok(hist)
}

/// Compare two fingerprints.
pub fn compare(a: &ImageFeatures, b: &ImageFeatures) -> Similarity {
    // pHash: 1.0 for identical, 0.0 for 64 differing bits.
    let hamming = (a.phash ^ b.phash).count_ones() as f64;
    let phash = (1.0 - hamming / 64.0).clamp(0.0, 1.0);

    // HSV: cosine similarity over the hue histograms.
    let hsv = {
        let dot: f32 = a.hsv_hist.iter().zip(&b.hsv_hist).map(|(x, y)| x * y).sum();
        let na: f32 = a.hsv_hist.iter().map(|v| v * v).sum::<f32>().sqrt();
        let nb: f32 = b.hsv_hist.iter().map(|v| v * v).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            ((dot / (na * nb)) as f64).clamp(0.0, 1.0)
        }
    };

    // ORB: compare descriptor rows directly. Full OpenCV BFMatcher plumbing is
    // unnecessary here; a nearest-neighbour ratio test over raw Hamming
    // distances gives us exactly what the confidence number needs.
    let orb = {
        const DESC_LEN: usize = 32;
        let an = a.orb.len() / DESC_LEN;
        let bn = b.orb.len() / DESC_LEN;
        if an == 0 || bn == 0 {
            0.0
        } else if a.orb == b.orb {
            1.0
        } else {
            let mut good = 0usize;
            for i in 0..an {
                let da = &a.orb[i * DESC_LEN..(i + 1) * DESC_LEN];
                // Best and second-best Hamming distance against b.
                let mut best = u32::MAX;
                let mut second = u32::MAX;
                for j in 0..bn {
                    let db = &b.orb[j * DESC_LEN..(j + 1) * DESC_LEN];
                    let mut d = 0u32;
                    for k in 0..DESC_LEN {
                        d += (da[k] ^ db[k]).count_ones();
                    }
                    if d < best {
                        second = best;
                        best = d;
                    } else if d < second {
                        second = d;
                    }
                }
                // Lowe's ratio test (0.75) keeps only distinctive matches.
                if best as f64 / (second as f64).max(1.0) < 0.75 {
                    good += 1;
                }
            }
            (good as f64 / an as f64).clamp(0.0, 1.0)
        }
    };

    Similarity { phash, orb, hsv }
}

// ---------------------------------------------------------------------------
// Screen classification
// ---------------------------------------------------------------------------

/// Classify which game screen a screenshot shows.
///
/// The two screens differ structurally, so layout beats colour:
///
/// * A **背包** panel is a regular grid of many small cells. Downscaling the
///   image and counting strong vertical edges produces a high, periodic count
///   (roughly 2 edges per column × 6 columns).
/// * A **人物属性** panel is a few wide, full-width rows. Its vertical edge
///   count is low, and it is dominated by one or two long saturated
///   red/blue horizontal bars (气血 / 魔法).
///
/// Colour is therefore only used as a *positive* signal for the attributes
/// panel, never to rule it out: inventory items are gold-on-dark, which
/// overlaps the hue range of the HP bar.
pub fn classify_screen(img: &image::RgbaImage) -> ScreenKind {
    // Downscale first: classification only needs coarse structure, and this
    // keeps the edge count meaningful for both phone and desktop captures.
    let small = image::imageops::resize(img, 320, 200, image::imageops::FilterType::Triangle);

    let mat = match rgba_to_bgr_mat(&small) {
        Ok(m) => m,
        Err(_) => return ScreenKind::Unknown,
    };

    // A regular grid yields many evenly spaced vertical edges.
    let grid_edges = count_vertical_edges(&mat).unwrap_or(0);

    // Long horizontal saturated bars are the signature of the stat panel.
    let has_wide_bar = match detect_wide_bars(&mat) {
        Ok(v) => v,
        Err(_) => false,
    };

    // Grids of small cells produce far more vertical structure than the
    // handful of panel borders on the attributes screen.
    if grid_edges >= 60 {
        return ScreenKind::Inventory;
    }
    if has_wide_bar {
        return ScreenKind::CharacterAttributes;
    }
    if grid_edges >= 25 {
        return ScreenKind::Inventory;
    }
    ScreenKind::Unknown
}

/// Classify a capture from the labels the panel itself renders.
///
/// The pixel heuristic above was tuned on panel-only images. On a window
/// capture — even after cropping to the panel — it came back `Unknown` for a
/// panel whose every field had just been read, and the UI reported that as a
/// failed recognition. The panel's own words are much stronger evidence and are
/// available for free because OCR runs first.
fn classify_from_lines(lines: &[OcrLine]) -> Option<ScreenKind> {
    let has = |keys: &[&str]| -> bool {
        lines.iter().any(|l| keys.iter().any(|k| l.text.contains(k)))
    };

    // 背包 first: a bag panel also carries 人物评分/经验, while the attributes
    // panel never shows the bag's tabs.
    if has(&["包裹", "背包", "行囊", "外观衣橱", "整理", "礼包"]) {
        return Some(ScreenKind::Inventory);
    }
    if has(&["气血", "魔法", "物理伤害", "法术伤害", "治疗强度", "战斗属性", "人物属性"]) {
        return Some(ScreenKind::CharacterAttributes);
    }
    None
}

// ---------------------------------------------------------------------------
// Panel localisation
// ---------------------------------------------------------------------------/// A rectangular region of a capture, in image pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelBox {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Locate the game's UI panel inside a *window* capture.
///
/// Everything downstream (classification, every extractor) was written against
/// a capture that is already just the panel — that is what the in-game share
/// button produces. The 截图识别 button instead grabs the whole client area, so
/// the frame also holds the world, the HUD clock, chat lines and quest
/// trackers. Feeding that to the extractors read the clock `01:56` as 金币 and
/// invented inventory items from quest text, while `classify_screen` — tuned on
/// panel-only images — called the whole thing `Unknown`.
///
/// The panel is by far the brightest large surface in these captures (parchment
/// against a dimmed world), which is what this relies on: an Otsu threshold over
/// a downscaled luminance image, a 3x3 opening to drop text and highlights, then
/// a projection-based tight box. Anything that does not look like one big
/// rectangular surface is rejected and the caller keeps the full frame, so a
/// missed detection degrades to the old behaviour rather than to a bad crop.
pub fn locate_panel(img: &image::RgbaImage) -> Option<PanelBox> {
    let (w, h) = (img.width(), img.height());
    if w < 240 || h < 160 {
        return None;
    }

    // Work at ~400 px on the long side: the panel is a large structure, and
    // this keeps the whole scan around 100k pixels.
    let scale = 400.0 / w.max(h) as f64;
    let sw = ((w as f64 * scale).round() as u32).max(64);
    let sh = ((h as f64 * scale).round() as u32).max(48);
    let small = image::imageops::resize(img, sw, sh, image::imageops::FilterType::Triangle);

    let mut gray = Vec::with_capacity((sw * sh) as usize);
    let mut hist = [0u32; 256];
    for p in small.pixels() {
        let [r, g, b, _] = p.0;
        let luma = ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8) as u8;
        hist[luma as usize] += 1;
        gray.push(luma);
    }
    let thr = otsu_threshold(&hist, (sw * sh) as u32);
    // A panel that is merely "as bright as the rest" cannot be isolated.
    if thr < 60 {
        return None;
    }

    let mut mask: Vec<bool> = gray.iter().map(|&v| v > thr).collect();
    // Opening: one eroded/dilated pass removes 1-2 px highlights, outlines and
    // most glyph strokes, leaving the solid panel body.
    mask = morph(&mask, sw, sh, false);
    mask = morph(&mask, sw, sh, true);

    let (sw_i, sh_i) = (sw as usize, sh as usize);
    let mut col_ink = vec![0u32; sw_i];
    let mut row_ink = vec![0u32; sh_i];
    for y in 0..sh_i {
        for x in 0..sw_i {
            if mask[y * sw_i + x] {
                col_ink[x] += 1;
                row_ink[y] += 1;
            }
        }
    }

    // A column belongs to the panel when a decent share of its height is
    // covered; that ignores isolated bright specks in the corners.
    let col_min = (sh_i as u32 / 5).max(3);
    let row_min = (sw_i as u32 / 5).max(3);
    let x0 = col_ink.iter().position(|&c| c >= col_min)?;
    let x1 = col_ink.iter().rposition(|&c| c >= col_min)?;
    let y0 = row_ink.iter().position(|&c| c >= row_min)?;
    let y1 = row_ink.iter().rposition(|&c| c >= row_min)?;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let bw = x1 - x0 + 1;
    let bh = y1 - y0 + 1;
    let area_ratio = (bw * bh) as f64 / (sw_i * sh_i) as f64;
    // Too small to be the panel; close to the whole frame means there is no
    // separate panel to crop (the image is already the panel) — either way the
    // caller should use the frame as-is.
    if !(0.12..=0.55).contains(&area_ratio) {
        return None;
    }
    let aspect = bw as f64 / bh as f64;
    if !(0.45..=3.2).contains(&aspect) {
        return None;
    }
    let mut ink = 0u32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            if mask[y * sw_i + x] {
                ink += 1;
            }
        }
    }
    let fill = ink as f64 / (bw * bh) as f64;
    if fill < 0.30 {
        return None;
    }

    // Back to full resolution, with a small margin so the panel's title bar and
    // side tabs are not clipped by a pixel of thresholded edge.
    let inv = 1.0 / scale;
    let pad_x = ((bw as f64 * 0.02) as i32).max(4);
    let pad_y = ((bh as f64 * 0.02) as i32).max(4);
    let fx = ((x0 as f64 * inv) as i32 - pad_x).max(0);
    let fy = ((y0 as f64 * inv) as i32 - pad_y).max(0);
    let fx2 = (((x1 + 1) as f64 * inv) as i32 + pad_x).min(w as i32);
    let fy2 = (((y1 + 1) as f64 * inv) as i32 + pad_y).min(h as i32);
    if fx2 <= fx || fy2 <= fy {
        return None;
    }

    Some(PanelBox { x: fx, y: fy, width: fx2 - fx, height: fy2 - fy })
}

/// Otsu's method: the luminance that maximises between-class variance.
fn otsu_threshold(hist: &[u32; 256], total: u32) -> u8 {
    if total == 0 {
        return 0;
    }
    let sum: f64 = (0..256).map(|i| i as f64 * hist[i] as f64).sum();
    let (mut sum_b, mut w_b) = (0.0f64, 0u32);
    let (mut best, mut best_t) = (0.0f64, 0u8);
    for t in 0..256usize {
        w_b += hist[t];
        if w_b == 0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0 {
            break;
        }
        sum_b += t as f64 * hist[t] as f64;
        let mu_b = sum_b / w_b as f64;
        let mu_f = (sum - sum_b) / w_f as f64;
        let var = w_b as f64 * w_f as f64 * (mu_b - mu_f) * (mu_b - mu_f);
        if var > best {
            best = var;
            best_t = t as u8;
        }
    }
    best_t
}

/// 3x3 erode (`dilate == false`) or dilate over a binary mask. Pixels outside the
/// image count as background, which keeps the border from swelling.
fn morph(src: &[bool], w: u32, h: u32, dilate: bool) -> Vec<bool> {
    let (w_i, h_i) = (w as usize, h as usize);
    let mut out = vec![false; src.len()];
    for y in 0..h_i {
        for x in 0..w_i {
            let mut hit = 0;
            let mut seen = 0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w_i as i32 || ny >= h_i as i32 {
                        seen += 1;
                        continue;
                    }
                    seen += 1;
                    if src[ny as usize * w_i + nx as usize] {
                        hit += 1;
                    }
                }
            }
            out[y * w_i + x] = if dilate { hit > 0 } else { hit == seen };
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Visual fingerprints
// ---------------------------------------------------------------------------

/// Side length of the square grayscale thumbnail used as a panel fingerprint.
pub const THUMB_SIDE: u32 = 32;

/// 32x32 grayscale thumbnail of an image — in practice, of the panel crop.
///
/// This is what links a 包裹 capture to a character: the panel renders no
/// 游戏ID and no nickname, so the picture itself is the only evidence. A global
/// pHash is useless here (a bag and a 人物属性 panel score 0.91 against each
/// other because both are bright parchment rectangles in the same screen
/// region), while a thumbnail cosine separates "same panel again" (~0.98) from
/// "different panel" (~0.48).
pub fn panel_thumb(img: &image::RgbaImage) -> Vec<u8> {
    let small = image::imageops::resize(
        img,
        THUMB_SIDE,
        THUMB_SIDE,
        image::imageops::FilterType::Triangle,
    );
    small
        .pixels()
        .map(|p| {
            let [r, g, b, _] = p.0;
            ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8) as u8
        })
        .collect()
}

/// Zero-mean cosine similarity between two thumbnails, in 0.0..=1.0.
///
/// The mean is removed so that a brighter or darker capture of the same panel
/// still scores high (the game dims the world when a panel opens, and the
/// window capture's exposure depends on what is behind it).
pub fn thumb_similarity(a: &[u8], b: &[u8]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let n = a.len() as f64;
    let ma = a.iter().map(|&v| v as f64).sum::<f64>() / n;
    let mb = b.iter().map(|&v| v as f64).sum::<f64>() / n;
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for (x, y) in a.iter().zip(b) {
        let dx = *x as f64 - ma;
        let dy = *y as f64 - mb;
        dot += dx * dy;
        na += dx * dx;
        nb += dy * dy;
    }
    if na <= f64::EPSILON || nb <= f64::EPSILON {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(0.0, 1.0)
}

/// Thresholds and weights for turning several weak signals into one decision.
///
/// These numbers are **heuristics fitted on a few dozen real captures** of one
/// game at one UI scale — not universal constants. They live in a struct so
/// they can be re-fitted (or shadowed per-call in tests) in one place, and so
/// the reasoning is visible instead of hidden in scattered `if x > 0.94`.
///
/// | 信号 | 权重 | 能否单独定案 |
/// |---|---|---|
/// | 面板缩略图 | 0.55 | 强度 ≥ 0.90（≈ 相似度 0.98）时可以 |
/// | 人物评分   | 0.40 | **不能** —— 随装备/等级变化 |
/// | 发色/武器  | 0.35 | 强度 ≥ 0.85 时可以 |
/// | 头像       | 0.25 | 强度 ≥ 0.95 时可以 |
/// | 时间上下文 | 0.10 | 不能 |
///
/// 权重之和刻意大于 1：互相印证的信号能越过判定线，孤立信号停在线上。
#[derive(Debug, Clone)]
pub struct MatchPolicy {
    pub panel_weight: f64,
    pub score_weight: f64,
    pub appearance_weight: f64,
    pub avatar_weight: f64,
    pub history_weight: f64,
    /// 低于此相似度的两张缩略图不是同一张图（包裹 vs 人物属性 ≈ 0.48，
    /// 不同角色的同版式面板 ≈ 0.82，同一面板重拍 ≈ 0.98）。
    pub panel_floor: f64,
    /// 发色距离超过此值视为明显不是同一个人。
    pub hair_gate: f64,
    /// 发色距离在此以内算"同一个颜色"，而不是"同一个色系".
    pub hair_close: f64,
    /// 武器高光色距离超过此值视为换了武器（阻尼而非否决）。
    pub weapon_gate: f64,
    /// 通过所需的最低总分。
    pub min_total: f64,
    /// 与第二名的必备差距，避免"看起来都差不多"时瞎猜。
    pub min_margin: f64,
    /// 各信号可以单独定案的强度线。
    pub decisive_panel: f64,
    pub decisive_appearance: f64,
    pub decisive_avatar: f64,
    /// 上下文证据：此窗口内算"刚刚采集过".
    pub recent_hours: f64,
    /// 超过此天数不再给上下文加分。
    pub stale_days: f64,
}

impl Default for MatchPolicy {
    fn default() -> Self {
        Self {
            panel_weight: 0.55,
            score_weight: 0.40,
            appearance_weight: 0.35,
            avatar_weight: 0.25,
            history_weight: 0.10,
            panel_floor: 0.80,
            // 同一角色跨面板实测 32，不同角色 45——所以 70 只拦"明显不是一个人"，
            // 真正的区分交给武器色与加权。
            hair_gate: 70.0,
            hair_close: 40.0,
            // 换武器的代价比换发型小得多，门槛放得比发色宽。
            weapon_gate: 80.0,
            min_total: 0.45,
            min_margin: 0.08,
            decisive_panel: 0.90,
            decisive_appearance: 0.55,
            decisive_avatar: 0.95,
            recent_hours: 24.0,
            stale_days: 30.0,
        }
    }
}

/// Outcome of weighing every stored character against one capture.
#[derive(Debug, Clone, Default)]
pub struct VisualAssociation {
    /// `Some` only when the evidence clearly singles one character out.
    pub character_id: Option<String>,
    pub confidence: f64,
    /// Why that character won, strongest evidence first.
    pub evidence: Vec<MatchEvidence>,
    /// Ranked candidates, best first. Populated even when nothing was picked,
    /// so the UI can offer a choice instead of a blank.
    pub candidates: Vec<IdentityCandidate>,
}

struct Scored<'a> {
    reference: &'a IdentityRef,
    total: f64,
    evidence: Vec<MatchEvidence>,
    decisive: bool,
}

fn panel_label(kind: ScreenKind) -> &'static str {
    match kind {
        ScreenKind::Inventory => "包裹",
        ScreenKind::CharacterAttributes => "人物属性",
        ScreenKind::Unknown => "面板",
    }
}

/// Weigh one stored character against the current capture.
///
/// Every signal contributes `strength * weight`; nothing vetoes the candidate
/// outright except "obviously different hair", because a changed weapon or a
/// re-shot panel is normal and should lower the score rather than rule someone
/// out.
fn score_candidate(
    reference: &IdentityRef,
    decoded: &[(ScreenKind, Vec<u8>)],
    current_scores: &[i64],
    appearance: Option<&mhcore::AppearanceSignature>,
    avatar_thumb: Option<&[u8]>,
    now_ms: i64,
    policy: &MatchPolicy,
) -> (f64, Vec<MatchEvidence>, bool) {
    use base64::Engine as _;
    let mut evidence: Vec<MatchEvidence> = Vec::new();

    // ---- 1. same panel picture ------------------------------------------
    let mut best_sim = 0.0f64;
    let mut best_kind = ScreenKind::Unknown;
    for (kind, cur) in decoded {
        for stored in &reference.fingerprints {
            if stored.screen != *kind {
                continue;
            }
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&stored.thumb) else {
                continue;
            };
            let sim = thumb_similarity(cur, &bytes);
            if sim > best_sim {
                best_sim = sim;
                best_kind = *kind;
            }
        }
    }
    if best_sim >= policy.panel_floor {
        let strength = ((best_sim - policy.panel_floor) / (1.0 - policy.panel_floor)).clamp(0.0, 1.0);
        evidence.push(MatchEvidence::new(
            EvidenceKind::PanelThumb,
            strength,
            strength * policy.panel_weight,
            format!("{}面板相似度 {:.2}", panel_label(best_kind), best_sim),
        ));
    }

    // ---- 2. 人物评分 (auxiliary: it moves with gear) ---------------------
    if let Some(score) = current_scores
        .iter()
        .copied()
        .find(|s| reference.fingerprints.iter().any(|f| f.score == Some(*s)))
    {
        evidence.push(MatchEvidence::new(
            EvidenceKind::CharacterScore,
            1.0,
            policy.score_weight,
            format!("人物评分 {score} 一致"),
        ));
    }

    // ---- 3. 发色 / 武器 --------------------------------------------------
    if let (Some(cur), Some(stored)) = (appearance, reference.appearance.as_ref()) {
        if let Some((strength, detail)) = appearance_strength(cur, stored, policy) {
            evidence.push(MatchEvidence::new(
                EvidenceKind::Appearance,
                strength,
                strength * policy.appearance_weight,
                detail,
            ));
        }
    }

    // ---- 4. 头像 ---------------------------------------------------------
    if let (Some(cur), Some(stored_b64)) = (avatar_thumb, reference.avatar_thumb.as_ref()) {
        if let Ok(stored) = base64::engine::general_purpose::STANDARD.decode(stored_b64) {
            if !cur.is_empty() && stored.len() == cur.len() {
                let sim = thumb_similarity(cur, &stored);
                if sim >= policy.panel_floor {
                    let strength =
                        ((sim - policy.panel_floor) / (1.0 - policy.panel_floor)).clamp(0.0, 1.0);
                    evidence.push(MatchEvidence::new(
                        EvidenceKind::Avatar,
                        strength,
                        strength * policy.avatar_weight,
                        format!("头像相似度 {:.2}", sim),
                    ));
                }
            }
        }
    }

    // ---- 5. context: seen recently ---------------------------------------
    if let Some(last) = reference.last_seen_at {
        let hours = (now_ms - last).max(0) as f64 / 3_600_000.0;
        let horizon = policy.stale_days * 24.0;
        if hours <= horizon {
            let strength = if hours <= policy.recent_hours {
                1.0
            } else {
                let span = (horizon - policy.recent_hours).max(1.0);
                (1.0 - (hours - policy.recent_hours) / span).clamp(0.0, 1.0)
            };
            let detail = if hours <= policy.recent_hours {
                "最近采集过".to_string()
            } else {
                format!("{:.0} 天前采集过", (hours / 24.0).ceil())
            };
            evidence.push(MatchEvidence::new(
                EvidenceKind::History,
                strength,
                strength * policy.history_weight,
                detail,
            ));
        }
    }

    evidence.sort_by(|a, b| {
        b.contribution
            .partial_cmp(&a.contribution)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total = evidence
        .iter()
        .map(|e| e.contribution)
        .sum::<f64>()
        .clamp(0.0, 1.0);
    let decisive = evidence.iter().any(|e| match e.kind {
        EvidenceKind::PanelThumb => e.strength >= policy.decisive_panel,
        EvidenceKind::Appearance => e.strength >= policy.decisive_appearance,
        EvidenceKind::Avatar => e.strength >= policy.decisive_avatar,
        _ => false,
    });
    (total, evidence, decisive)
}

/// What the pipeline does with the visual evidence.
#[derive(Debug, Clone, PartialEq)]
enum IdentityDecision {
    /// 面板身份字段与视觉指向不同的人：只记录，**不覆盖**。
    Conflict(String),
    /// 面板没报身份，视觉证据定案。
    Adopt { character_id: String, confidence: f64 },
    /// 没有定论，交回用户。
    Unresolved,
}

/// 冲突保护：面板上写着的身份永远优先于视觉推断。
///
/// 这里刻意没有"视觉置信度高就覆盖"的分支——游戏ID 是唯一权威标识，让视觉
/// 覆盖它就是过拟合风险的来源：换发色、换时装、换坐骑都会让缩略图漂移，
/// 而游戏ID 不会。
fn decide_identity(
    resolved: Option<&str>,
    names_itself: bool,
    source: IdentitySource,
    assoc: &VisualAssociation,
    label: &dyn Fn(&str) -> String,
) -> IdentityDecision {
    match (resolved, assoc.character_id.as_deref()) {
        (Some(chosen), Some(visual)) if chosen != visual => {
            let key = match source {
                IdentitySource::IdField => "游戏ID",
                IdentitySource::Nickname => "昵称",
                _ => "面板身份",
            };
            IdentityDecision::Conflict(format!(
                "视觉匹配指向「{}」，但{key}明确是「{}」，已以{key}为准",
                label(visual),
                label(chosen)
            ))
        }
        (Some(_), _) => IdentityDecision::Unresolved,
        (None, Some(visual)) if !names_itself => IdentityDecision::Adopt {
            character_id: visual.to_string(),
            confidence: assoc.confidence,
        },
        // 面板自己报了身份（新角色），视觉结果不该接管：那正是
        // "把已有角色改名成新截图的名字" 的老 bug。
        (None, Some(_)) => IdentityDecision::Unresolved,
        (None, None) => IdentityDecision::Unresolved,
    }
}

/// Link an unidentified capture to a character by weighing every signal.
///
/// Replaces the old chain of hard thresholds (`if sim >= 0.94 …`) which had
/// three failure modes: a bag whose items changed scored just under the line,
/// 人物评分 was trusted as if it were an identity key, and nothing combined.
///
/// Rules:
///  * one signal alone may decide only when it is decisive (see [`MatchPolicy`]);
///  * otherwise at least two signals must agree, and the winner must beat the
///    runner-up by `min_margin`;
///  * a candidate is only rejected outright for clearly different hair — a
///    swapped weapon, a restyled panel or a new dye all just lower the score.
///
/// Returns ranked candidates even when nothing is conclusive, so the caller can
/// ask the user instead of silently creating a duplicate character.
pub fn associate_by_evidence(
    current: &[PanelFingerprint],
    refs: &[IdentityRef],
    appearance: Option<&mhcore::AppearanceSignature>,
    avatar_thumb: Option<&str>,
    now_ms: i64,
    policy: &MatchPolicy,
) -> VisualAssociation {
    use base64::Engine as _;
    if refs.is_empty() {
        return VisualAssociation::default();
    }

    let decoded: Vec<(ScreenKind, Vec<u8>)> = current
        .iter()
        .filter_map(|f| {
            base64::engine::general_purpose::STANDARD
                .decode(&f.thumb)
                .ok()
                .map(|b| (f.screen, b))
        })
        .collect();
    let current_scores: Vec<i64> = current.iter().filter_map(|f| f.score).collect();
    let avatar = avatar_thumb.and_then(|t| base64::engine::general_purpose::STANDARD.decode(t).ok());

    let mut scored: Vec<Scored> = refs
        .iter()
        .filter_map(|r| {
            let (total, evidence, decisive) = score_candidate(
                r,
                &decoded,
                &current_scores,
                appearance,
                avatar.as_deref(),
                now_ms,
                policy,
            );
            if evidence.is_empty() {
                None
            } else {
                Some(Scored { reference: r, total, evidence, decisive })
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.total
            .partial_cmp(&a.total)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let candidates: Vec<IdentityCandidate> = scored
        .iter()
        .take(3)
        .map(|s| IdentityCandidate {
            character_id: s.reference.character_id.clone(),
            nickname: s.reference.nickname.clone(),
            game_id: s.reference.game_id.clone(),
            server: s.reference.server.clone(),
            level: s.reference.level,
            confidence: round2(s.total),
            evidence: s.evidence.clone(),
        })
        .collect();

    let Some(best) = scored.first() else {
        return VisualAssociation { candidates, ..Default::default() };
    };
    let runner_up = scored.get(1).map(|s| s.total).unwrap_or(0.0);
    let agreeing = best.evidence.iter().filter(|e| e.strength >= 0.5).count();
    // A character whose look was never recorded could be the real owner, and an
    // appearance-only decision could not rule them out. Stay out of the way.
    let appearance_only = best
        .evidence
        .iter()
        .all(|e| matches!(e.kind, EvidenceKind::Appearance | EvidenceKind::History));
    let look_unknown = refs.iter().any(|r| r.appearance.is_none());
    // 两种定案方式：一个信号本身就足够决定性（相似度 0.98 的面板、完全一致的
    // 发色武器），或者两个以上信号互相印证并越过总分线。评分单独永远不够。
    let conclusive = best.total - runner_up >= policy.min_margin
        && !(appearance_only && look_unknown)
        && ((best.total >= policy.min_total && agreeing >= 2) || best.decisive);

    VisualAssociation {
        character_id: if conclusive {
            Some(best.reference.character_id.clone())
        } else {
            None
        },
        confidence: round2(best.total),
        evidence: best.evidence.clone(),
        candidates,
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Appearance signature (发色 / 武器)
// ---------------------------------------------------------------------------

/// Where the character's head sits, as fractions of the panel, per panel kind.
///
/// Measured off the real 1265x711 captures (panel 752x498): the 人物属性 panel
/// puts the standing model's head at x 143..203, y 105..169, the 包裹 panel
/// puts the mounted model's head at x 164..200, y 168..204. They are not in the
/// same place — the 包裹 panel's left column is shifted down by the equipment
/// slots — so a single relative box cannot serve both.
fn head_zone(kind: ScreenKind) -> ((f32, f32), (f32, f32)) {
    match kind {
        ScreenKind::CharacterAttributes => ((0.190, 0.270), (0.210, 0.340)),
        _ => ((0.215, 0.275), (0.335, 0.415)),
    }
}

/// Where the character model (and therefore any held weapon) is.
fn model_zone(kind: ScreenKind) -> ((f32, f32), (f32, f32)) {
    match kind {
        ScreenKind::CharacterAttributes => ((0.030, 0.380), (0.180, 0.720)),
        _ => ((0.130, 0.450), (0.260, 0.700)),
    }
}

/// Names for 发色. Only the buckets a game character actually uses; the name is
/// for the user, the stored RGB is what the comparison uses.
const HAIR_PALETTE: [(&str, [u8; 3]); 10] = [
    ("墨黑", [30, 30, 34]),
    // Navy is the commonest dye in this game, and its measured values slide
    // between (66,69,71) and (84,82,94) depending on the pose. A tight "深紫"
    // entry next door would relabel the same head of hair two different ways;
    // a real purple sits far enough away to stay its own bucket.
    ("深蓝", [55, 58, 82]),
    ("紫", [130, 80, 160]),
    ("深棕", [74, 52, 36]),
    ("亚麻", [168, 128, 84]),
    ("银白", [208, 210, 216]),
    ("灰白", [150, 150, 154]),
    ("赤红", [150, 52, 48]),
    ("酒红", [96, 40, 44]),
    ("青碧", [52, 120, 116]),
];

/// Names for the bright accent that reads as the held weapon / its glow.
const ACCENT_PALETTE: [(&str, [u8; 3]); 8] = [
    ("青碧", [80, 190, 180]),
    ("金", [214, 172, 72]),
    ("赤", [190, 62, 58]),
    ("紫", [150, 100, 200]),
    ("银", [214, 218, 222]),
    ("蓝", [82, 132, 210]),
    ("翠", [92, 180, 110]),
    ("玄", [70, 70, 78]),
];

fn nearest_name(palette: &[(&str, [u8; 3])], rgb: [u8; 3]) -> String {
    let mut best = palette[0];
    let mut best_d = u32::MAX;
    for (name, c) in palette {
        let d = (rgb[0] as i32 - c[0] as i32).pow(2)
            + (rgb[1] as i32 - c[1] as i32).pow(2)
            + (rgb[2] as i32 - c[2] as i32).pow(2);
        if (d as u32) < best_d {
            best_d = d as u32;
            best = (*name, *c);
        }
    }
    best.0.to_string()
}

/// Mean colour of the pixels in `zone` that satisfy `keep`.
///
/// The mean, not the modal colour: a modal histogram over a 30x30 head crop
/// picks whichever single shadow happens to be largest (the hat's black brim
/// won out on the mounted model), while the mean of all the dark pixels lands
/// within ~30 of the same character's portrait. That is the difference between
/// a feature that links and one that only looks plausible.
fn mean_colour(
    img: &image::RgbaImage,
    zone: ((f32, f32), (f32, f32)),
    keep: impl Fn([u8; 3]) -> bool,
) -> Option<([u8; 3], usize)> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x0 = (zone.0 .0 * w) as u32;
    let x1 = ((zone.0 .1 * w) as u32).min(img.width());
    let y0 = (zone.1 .0 * h) as u32;
    let y1 = ((zone.1 .1 * h) as u32).min(img.height());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let (mut n, mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32, 0u32);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = img.get_pixel(x, y).0;
            if p[3] < 128 {
                continue;
            }
            let rgb = [p[0], p[1], p[2]];
            if !keep(rgb) {
                continue;
            }
            n += 1;
            sr += p[0] as u32;
            sg += p[1] as u32;
            sb += p[2] as u32;
        }
    }
    if n == 0 {
        return None;
    }
    Some((
        [
            (sr / n) as u8,
            (sg / n) as u8,
            (sb / n) as u8,
        ],
        n as usize,
    ))
}

/// Hue in degrees, 0..360.
fn hue_deg(p: [u8; 3]) -> f32 {
    let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    if d <= 0.0 {
        return 0.0;
    }
    let h = if (mx - r).abs() < f32::EPSILON {
        60.0 * (((g - b) / d) % 6.0)
    } else if (mx - g).abs() < f32::EPSILON {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h + 360.0
    } else {
        h
    }
}

fn rgb_distance(a: [u8; 3], b: [u8; 3]) -> f64 {
    let dr = a[0] as f64 - b[0] as f64;
    let dg = a[1] as f64 - b[1] as f64;
    let db = a[2] as f64 - b[2] as f64;
    (dr * dr + dg * dg + db * db).sqrt()
}

/// 发色 / 武器 of the character rendered on this panel.
///
/// Returns `None` when the panel shows no character model at all (an
/// equipment-only screenshot, a cropped bag grid), which is the honest answer:
/// inventing a signature from the parchment background would poison the
/// comparison for every character.
pub fn appearance_signature(
    img: &image::RgbaImage,
    kind: ScreenKind,
) -> Option<mhcore::AppearanceSignature> {
    // 发色: mean of the dark pixels around the head. Shadows on the robe are
    // dark too, but the head zone is small enough that hair dominates it.
    let hair = mean_colour(img, head_zone(kind), |p| {
        p.iter().copied().max().unwrap_or(0) <= 150
    })
    .filter(|(_, n)| *n >= 120)?;

    // 武器: the game's cyan/teal weapon glow. Deliberately narrow — a generic
    // "bright and saturated" mask picks up the parchment and the gold trim
    // instead, which is what made an earlier version report a weapon colour of
    // "银" for a character holding a cyan spear.
    let weapon = mean_colour(img, model_zone(kind), |p| {
        let mx = p.iter().copied().max().unwrap_or(0);
        let mn = p.iter().copied().min().unwrap_or(0);
        mx >= 165 && (mx - mn) as u32 * 100 >= mx as u32 * 35 && (140.0..=205.0).contains(&hue_deg(p))
    })
    .filter(|(_, n)| *n >= 40);

    Some(mhcore::AppearanceSignature {
        hair_name: nearest_name(&HAIR_PALETTE, hair.0),
        hair_rgb: hair.0,
        weapon_name: weapon.map(|(rgb, _)| nearest_name(&ACCENT_PALETTE, rgb)),
        weapon_rgb: weapon.map(|(rgb, _)| rgb),
    })
}

/// Appearance signature for a whole window capture (panel located + cropped
/// first). Debug helper for `mhcli appearance`; `kind` picks which head zone to
/// use, since the zones differ between panels.
pub fn appearance_for_capture(
    img: &image::RgbaImage,
    kind: ScreenKind,
) -> Option<mhcore::AppearanceSignature> {
    let cropped = locate_panel(img).and_then(|b| crop_rgba(img, b.x, b.y, b.width, b.height));
    let work: &image::RgbaImage = cropped.as_ref().unwrap_or(img);
    appearance_signature(work, kind)
}

/// How well a stored signature explains the current capture, 0.0..=1.0.
///
/// Graded rather than pass/fail: the game lets a character re-dye their hair,
/// swap weapons and change outfits, so "发色 slightly different" must lower the
/// score instead of ruling the character out. Only a clearly different head of
/// hair rejects outright; a different weapon only dampens, because weapon swaps
/// are the commoner change.
fn appearance_strength(
    current: &mhcore::AppearanceSignature,
    stored: &mhcore::AppearanceSignature,
    policy: &MatchPolicy,
) -> Option<(f64, String)> {
    let d = rgb_distance(current.hair_rgb, stored.hair_rgb);
    if d >= policy.hair_gate {
        return None;
    }
    let hair = (1.0 - d / policy.hair_gate).clamp(0.0, 1.0);
    let mut strength = hair;
    let mut detail = if stored.hair_name == current.hair_name {
        format!("发色 {} 一致", current.hair_name)
    } else if d <= policy.hair_close {
        format!("发色与记录的 {} 相近", stored.hair_name)
    } else {
        format!("发色与记录的 {} 有差异（本次 {}）", stored.hair_name, current.hair_name)
    };

    if let (Some(a), Some(b)) = (current.weapon_rgb, stored.weapon_rgb) {
        let dw = rgb_distance(a, b);
        if dw >= policy.weapon_gate {
            // 换武器不否决，但要显著压低——否则两个发色相近的角色分不开。
            strength *= 0.3;
            detail.push_str("，武器不同");
        } else {
            let w = 1.0 - dw / policy.weapon_gate;
            strength = (strength * 0.6 + w * 0.4).clamp(0.0, 1.0);
            let same = dw <= 30.0;
            detail.push_str(&format!(
                "，武器{}",
                if same {
                    "同为同一色".to_string()
                } else {
                    format!("颜色相近（距离 {dw:.0}）")
                }
            ));
        }
    }
    Some((strength, detail))
}

/// Legacy single-threshold constants were replaced by [`MatchPolicy`], which
/// keeps the same idea but as named, tunable weights instead of magic numbers.

/// Extract a sub-image, clamped to the source bounds.
fn crop_rgba(
    img: &image::RgbaImage,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Option<image::RgbaImage> {
    let x = x.max(0) as u32;
    let y = y.max(0) as u32;
    let w = w.max(1) as u32;
    let h = h.max(1) as u32;
    if x >= img.width() || y >= img.height() {
        return None;
    }
    let w = w.min(img.width() - x);
    let h = h.min(img.height() - y);
    if w < 32 || h < 32 {
        return None;
    }
    Some(image::imageops::crop_imm(img, x, y, w, h).to_image())
}

/// Detect the long horizontal HP/MP gradient bars that characterise the
/// attribute panel: rows where most pixels are strongly saturated red or blue.
fn detect_wide_bars(bgr: &opencv::core::Mat) -> Result<bool> {    let mut hsv = opencv::core::Mat::default();
    opencv::imgproc::cvt_color_def(bgr, &mut hsv, opencv::imgproc::COLOR_BGR2HSV)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;

    let (rows, cols) = (hsv.rows(), hsv.cols());
    let data = hsv.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;

    // A "wide bar" row has at least 30% of its pixels in the saturated
    // red or blue hue bands, and stays saturated (S > 90, V > 90).
    let threshold = (cols as f32 * 0.30) as usize;
    for y in 0..rows as usize {
        let mut hits = 0usize;
        for x in 0..cols as usize {
            let i = (y * cols as usize + x) * 3;
            if i + 2 >= data.len() {
                continue;
            }
            let (h, s, v) = (data[i], data[i + 1], data[i + 2]);
            let is_red = h <= 8 || h >= 170;
            let is_blue = (100..=130).contains(&h);
            if (is_red || is_blue) && s > 90 && v > 90 {
                hits += 1;
                if hits >= threshold {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Count strong vertical edges after a Sobel filter. Used only as a layout cue.
fn count_vertical_edges(bgr: &opencv::core::Mat) -> Result<u32> {
    let gray = to_gray(bgr)?;
    let mut edges = opencv::core::Mat::default();
    opencv::imgproc::sobel_def(&gray, &mut edges, opencv::core::CV_16S, 1, 0)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let mut abs = opencv::core::Mat::default();
    opencv::core::absdiff(&edges, &opencv::core::Scalar::all(0.0), &mut abs)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let mut out = opencv::core::Mat::default();
    opencv::imgproc::threshold(&abs, &mut out, 300.0, 255.0, opencv::imgproc::THRESH_BINARY)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let nonzero = opencv::core::count_non_zero(&out)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    Ok((nonzero / 100).max(0) as u32)
}

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

fn ort_environment() -> Result<&'static ort::environment::Environment> {
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
fn crop_bgr(
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
fn ctc_greedy_decode(
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

// ---------------------------------------------------------------------------
// Field extraction
// ---------------------------------------------------------------------------

/// Labels we can map onto `CombatStats` fields.
fn stat_label_map(label: &str) -> Option<&'static str> {
    let l = label.replace([' ', '：', ':'], "");
    // Order matters: check the more specific labels first so that e.g.
    // 物理防御 does not get mistaken for 物理伤害.
    let pairs: &[(&str, &str)] = &[
        ("物理防御", "physical_defense"),
        ("法术防御", "magic_defense"),
        ("物理伤害", "physical_damage"),
        ("法术伤害", "magic_damage"),
        ("治疗强度", "healing_power"),
        ("盈福经验", "experience_bonus"),
        ("气血", "hp"),
        ("魔法", "mp"),
        ("速度", "speed"),
        // 愤怒 / 活力 also appear on the real 人物属性 panel.
        ("愤怒", "rage"),
        ("活力", "vigor"),
        ("经验", "experience"),
    ];
    if let Some(hit) = pairs.iter().find(|(k, _)| l.contains(*k)) {
        return Some(hit.1);
    }

    // PP-OCR misreads some widely-spaced glyphs on the real panel (血 -> 面).
    // Fall back on the FIRST glyph of the two-character stat labels, which is
    // far more reliable: 气 -> hp, 魔 -> mp, 速 -> speed.
    let first = l.chars().next()?;
    match first {
        '气' => Some("hp"),
        '魔' => Some("mp"),
        '速' => Some("speed"),
        '愤' => Some("rage"),
        '活' => Some("vigor"),
        _ => None,
    }
}

/// On the real game panels each label is rendered with wide letter spacing,
/// so PP-OCR's detector emits one box per GLYPH: 气 and 血 come back as two
/// separate lines. Merge horizontally adjacent single-character lines that
/// sit on the same row into one logical token.
///
/// This runs before every extractor so downstream code only ever sees
/// sensible words ("气血") instead of fragments ("气", "血").
fn merge_adjacent_fragments(lines: &[OcrLine]) -> Vec<OcrLine> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<OcrLine> = Vec::with_capacity(lines.len());
    for line in lines {
        let text = line.text.trim();
        // Only single-CJK-character boxes are candidates for merging.
        let is_single_cjk = text.chars().count() == 1
            && text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        if !is_single_cjk {
            out.push(line.clone());
            continue;
        }

        // Look for an existing fragment on the same visual row, close enough
        // horizontally that the pair reads as one word.
        let mut merged = false;
        for prev in out.iter_mut().rev() {
            // Only merge with a previous row: fragments are emitted left-to-right
            // by the OCR reading-order pass.
            let dy = (prev.center_y() - line.center_y()).abs();
            let max_dy = (prev.rect.3.max(line.rect.3) as f64) * 0.45;
            if dy > max_dy.max(4.0) {
                continue;
            }
            let gap = line.rect.0 - (prev.rect.0 + prev.rect.2);
            // The game draws glyphs ~a character-width apart; allow up to 2.2x
            // the glyph height so "速" + "度" merge but columns do not.
            if gap < 0 || (gap as f64) > (line.rect.3 as f64) * 3.0 + 6.0 {
                continue;
            }
            // Don't glue a fragment onto a long value/label that is already
            // complete (e.g. "5604/5604" + "气" must not merge).
            if prev.text.chars().count() > 2 {
                continue;
            }
            prev.text.push_str(text);
            let x1 = prev.rect.0;
            let y1 = prev.rect.1;
            let right = (prev.rect.0 + prev.rect.2).max(line.rect.0 + line.rect.2);
            let bottom = (prev.rect.1 + prev.rect.3).max(line.rect.1 + line.rect.3);
            prev.rect = (x1, y1, right - x1, bottom - y1);
            prev.confidence = prev.confidence.min(line.confidence);
            merged = true;
            break;
        }
        if !merged {
            out.push(line.clone());
        }
    }
    out
}


/// Parse a number that may contain thousands separators (`,`/`，`) and
/// OCR noise such as stray spaces.
///
/// Only the FIRST numeric token is parsed: OCR often returns "7,721 / 7,721"
/// or "758 1,714", and gluing every digit together would produce nonsense.
///
/// Thousands separators are ambiguous in OCR output: the recogniser
/// frequently renders `,` as `.` (e.g. "2.215/2.215"). Since every value in
/// this game's UI is an integer, `.` and `，` are treated as separators too.
fn parse_number(raw: &str) -> Option<i64> {
    // `01:56` is the HUD clock. Its leading digits parse as a plausible small
    // integer, and on a full-window capture that turned into 金币 = 1.
    let trimmed = raw.trim();
    if trimmed.contains(':')
        && trimmed.chars().all(|c| c.is_ascii_digit() || c == ':')
    {
        return None;
    }

    let mut token = String::new();
    let mut started = false;
    for c in raw.chars() {
        if c.is_ascii_digit() {
            token.push(c);
            started = true;
        } else if started && matches!(c, ',' | '，' | '.' | '·') {
            // A separator may only appear between digits; "7,721" is fine but
            // "2.215/2.215" must stop at the slash.
            token.push(',');
        } else if started {
            break;
        }
    }
    let cleaned: String = token.chars().filter(|c| c.is_ascii_digit()).collect();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<i64>().ok()
}

/// Find the numeric value that belongs to `label`: either the remainder of the
/// same line, or the nearest value to the right on the same row.
///
/// Selection is ranked by VERTICAL alignment first, then horizontal distance.
/// Ranking by horizontal distance alone picks the wrong value when several
/// stat rows share the same value column (e.g. 气血/魔法 on adjacent rows).
fn value_for_label(lines: &[OcrLine], label_line: &OcrLine) -> Option<(i64, f64, String)> {
    let after_label = strip_label(&label_line.text, &label_line.text);
    if let Some(v) = parse_number(after_label) {
        return Some((v, label_line.confidence, after_label.to_string()));
    }

    // Values must sit on the label's row, so allow roughly one row height.
    let row_tolerance = ((label_line.rect.3 as f64) * 1.5).max(20.0);
    let mut best: Option<(f64, f64, i64, f64, String)> = None;
    for other in lines {
        if other.rect.0 <= label_line.rect.0 {
            continue; // values are laid out to the right of their label
        }
        let dy = (other.center_y() - label_line.center_y()).abs();
        if dy > row_tolerance {
            continue;
        }
        let Some(v) = parse_number(&other.text) else { continue };
        let dx = other.center_x() - label_line.center_x();
        let better = match best.as_ref() {
            None => true,
            // Vertical alignment wins; horizontal distance only breaks ties.
            Some(b) => dy < b.0 - 2.0 || ((dy - b.0).abs() <= 2.0 && dx < b.1),
        };
        if better {
            best = Some((dy, dx, v, other.confidence, other.text.clone()));
        }
    }
    best.map(|(_, _, v, c, raw)| (v, c, raw))
}

fn strip_label<'a>(text: &'a str, _label: &str) -> &'a str {
    // OCR sometimes returns "气血 7,721 / 7,721" as one line.
    text.split_once(' ')
        .map(|(_, rest)| rest)
        .unwrap_or("")
}

/// Extract the game id.
///
/// Crop the character portrait out of a 人物属性 screenshot and return it as a
/// base64 PNG.
///
/// ## How the avatar is located
///
/// Hardcoding pixel coordinates would break on every resolution/DPI change, so
/// the crop is derived from the **game-ID text**, which we already recognise
/// reliably. On the real panel the layout is:
///
/// ```text
///   +--------+   ID 429277361
///   | avatar |   Lv.xx  门派...
///   +--------+
/// ```
///
/// Rules that hold across captures:
/// * the frame is **square**;
/// * its **right edge** sits a small gap left of the `ID` text;
/// * its **vertical centre** is near the `ID`/level header rows.
///
/// We therefore seed a square from the ID box, then verify it really looks like
/// a portrait (high colour variance inside, low variance on the border ring).
/// If the check fails we return `None` rather than shipping a wrong crop.
fn crop_avatar(img: &image::RgbaImage, lines: &[OcrLine]) -> Option<image::RgbaImage> {
    let (w, h) = (img.width() as i32, img.height() as i32);

    // Anchor: the "ID" token (or the line that carries the numeric game id).
    let anchor = lines
        .iter()
        .find(|l| {
            let t = l.text.trim();
            t.eq_ignore_ascii_case("id") || t == "ID" || t == "编号"
        })
        .or_else(|| {
            lines.iter().find(|l| {
                l.text.chars().filter(|c| c.is_ascii_digit()).count() >= 6
                    && !l.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            })
        })?;

    // A square whose right edge stops just left of the anchor, vertically
    // centred on the header block (ID row plus the level row beneath it).
    //
    // Geometry measured off the real 1265x711 capture:
    //   avatar frame   : x 292..378, y 158..248  (86x90, effectively square)
    //   "ID" text box  : x 397..412, y 194..207
    // so the frame's right edge sits ~19px left of the ID box and its top is
    // ~36px above the ID row centre.
    // Header glyph height. This must stay in a plausible range: a tall
    // vertical label elsewhere on the panel (e.g. the 战前检查 side tab is a
    // single 118px-tall box) would otherwise blow the avatar size up to
    // hundreds of pixels. Only consider boxes of roughly normal header
    // line-height and pick the median rather than the maximum.
    let mut heights: Vec<i32> = lines
        .iter()
        .filter(|l| {
            l.rect.1 < anchor.rect.1 + anchor.rect.3 * 3
                && l.rect.3 > 6
                && l.rect.3 < anchor.rect.3 * 2
        })
        .map(|l| l.rect.3)
        .collect();
    heights.sort_unstable();
    let header_h = if heights.is_empty() {
        anchor.rect.3 as f32
    } else {
        heights[heights.len() / 2] as f32
    };

    // Scale everything from the header text height so other resolutions work.
    //
    // Multipliers measured off the real 1265x711 capture (header_h=13):
    //   frame          : x 289..357, y 150..210  (66x60, drawn square + frame)
    //   "ID" text box  : x 397..412, y 194..207
    //   -> size = 66   = 5.08 * header_h
    //   -> gap  = 40   = 3.08 * header_h  (frame right edge -> ID box left)
    //   -> top  = ID top - 44 = ID top - 0.667 * size
    // An earlier 6.6x/0.42 estimate framed the side tab and the badge instead
    // of the portrait, which produced a visibly off-centre crop.
    let gap = header_h * 3.08;
    let size = header_h * 5.08;
    let right = anchor.rect.0 as f32 - gap;
    let left = right - size;
    let top = anchor.rect.1 as f32 - size * 0.667;

    if left < 0.0 || top < 0.0 || right > w as f32 || top + size > h as f32 {
        tracing::debug!(
            "avatar: out of bounds (header_h={header_h:.1} size={size:.1} left={left:.1} top={top:.1} right={right:.1} img={w}x{h})"
        );
        return None;
    }

    let (x, y, s) = (left.round() as i32, top.round() as i32, size.round() as i32);
    if s < 32 {
        return None;
    }

    // The located square includes the frame plus a little neighbouring chrome.
    // Trim a small margin so the exported image is the portrait art itself
    // rather than art-with-background; the margin is proportional so it also
    // works at other resolutions.
    let inset = (s / 14).max(1);
    let ix = x + inset;
    let iy = y + inset;
    let is = (s - inset * 2).max(1);

    // Trust check: a portrait has strong colour variation inside; the area
    // immediately outside its frame does not.
    if !looks_like_portrait(img, ix, iy, is) {
        return None;
    }

    Some(image::imageops::crop_imm(img, ix as u32, iy as u32, is as u32, is as u32).to_image())
}

/// Crop the framed portrait out of a 人物属性 panel and encode it as a PNG
/// data URL for the UI.
fn crop_avatar_png(img: &image::RgbaImage, lines: &[OcrLine]) -> Option<String> {
    let cropped = crop_avatar(img, lines)?;
    // Encode as PNG and base64 so the UI can turn it straight into a data URL.
    let mut png_bytes = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        if cropped.write_to(&mut cursor, image::ImageFormat::Png).is_err() {
            return None;
        }
    }
    use base64::Engine as _;
    Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
}

/// 16x16 grayscale of the cropped portrait, used as a face fingerprint.
///
/// Colour is dropped on purpose: the game re-tints portraits with lighting and
/// equipment glow, while the *shape* of the face stays put. 256 bytes is small
/// enough to keep in the characters table and compare on every recognition.
fn crop_avatar_thumb(img: &image::RgbaImage, lines: &[OcrLine]) -> Option<String> {
    let cropped = crop_avatar(img, lines)?;
    let small = image::imageops::resize(
        &cropped,
        16,
        16,
        image::imageops::FilterType::Triangle,
    );
    let mut out = Vec::with_capacity(16 * 16);
    for p in small.pixels() {
        // Rec. 601 luma, same weighting the panel thumbnails use.
        let v = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
        out.push(v.clamp(0.0, 255.0) as u8);
    }
    use base64::Engine as _;
    Some(base64::engine::general_purpose::STANDARD.encode(&out))
}

/// Heuristic confidence check for a candidate avatar crop.
///
/// A real portrait is colourful and detailed; the panel background around it is
/// flat. Requiring both keeps us from returning a random slice of UI chrome.
fn looks_like_portrait(img: &image::RgbaImage, x: i32, y: i32, s: i32) -> bool {
    let (w, h) = (img.width() as i32, img.height() as i32);

    // Interior variance (sample a 16x16 grid to stay cheap).
    let step = (s / 16).max(1);
    let mut samples = Vec::new();
    let mut yy = y + s / 8;
    while yy < y + s - s / 8 && yy < h {
        let mut xx = x + s / 8;
        while xx < x + s - s / 8 && xx < w {
            let p = img.get_pixel(xx as u32, yy as u32);
            samples.push((p[0] as f32, p[1] as f32, p[2] as f32));
            xx += step;
        }
        yy += step;
    }
    if samples.len() < 16 {
        return false;
    }
    let mean = (
        samples.iter().map(|p| p.0).sum::<f32>() / samples.len() as f32,
        samples.iter().map(|p| p.1).sum::<f32>() / samples.len() as f32,
        samples.iter().map(|p| p.2).sum::<f32>() / samples.len() as f32,
    );
    let var = samples
        .iter()
        .map(|p| {
            let dr = p.0 - mean.0;
            let dg = p.1 - mean.1;
            let db = p.2 - mean.2;
            dr * dr + dg * dg + db * db
        })
        .sum::<f32>()
        / samples.len() as f32;
    // Detached portraits on coloured backgrounds easily exceed this; a flat
    // panel area does not.
    var > 700.0
}

/// Two layouts occur in practice:
///  * the synthetic/older layout puts it on one line: "游戏ID 429277013";
///  * the real 人物属性 panel renders it as two adjacent boxes: "ID" then
///    "429277361" immediately to its right (a small gap).
/// Both are handled here; the second path is the one that matters for real
/// captures.
fn extract_game_id(lines: &[OcrLine]) -> Option<(String, f64)> {
    // ---- path 1: marker and digits in the same line --------------------
    const MARKERS: [&str; 4] = ["游戏ID", "游戏id", "游戏ID", "ID"];
    for line in lines {
        let normalized = line.text.replace(' ', "");
        if let Some(pos) = MARKERS.iter().filter_map(|m| normalized.find(*m)).min() {
            let rest = &normalized[pos..];
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
                .filter(|c| c.is_ascii_digit())
                .collect();
            if digits.len() >= 5 {
                return Some((digits, line.confidence));
            }
        }
    }

    // ---- path 2: a standalone "ID" box, digits in the box to its right --
    for line in lines {
        let t = line.text.trim();
        if !is_id_marker(t) {
            continue;
        }
        let mut best: Option<(f64, String, f64)> = None;
        for other in lines {
            if other == line {
                continue;
            }
            // The id digits sit on the same row, just to the right.
            let dy = (other.center_y() - line.center_y()).abs();
            if dy > (line.rect.3 as f64).max(12.0) {
                continue;
            }
            // Compare CENTRES, not edges: the detector's unclip padding makes
            // the "ID" box and the digits box overlap by a few pixels on the
            // real panel, and an edge-based gap test rejected exactly that pair.
            if other.center_x() <= line.center_x() {
                continue;
            }
            let gap = other.rect.0 - (line.rect.0 + line.rect.2);
            if (gap as f64) < -(line.rect.3 as f64) || (gap as f64) > (line.rect.3 as f64) * 3.0 {
                continue;
            }
            let digits: String =
                other.text.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 6 {
                if best.as_ref().map(|b| gap as f64 > b.0).unwrap_or(true) {
                    best = Some((gap as f64, digits, other.confidence));
                }
            }
        }
        if let Some((_, digits, conf)) = best {
            return Some((digits, conf.min(line.confidence)));
        }
    }
    None
}

/// Is this token the "ID" label of the character header?
///
/// PP-OCR reads the real panel's `ID` as `LD` often enough that matching only
/// the literal string loses the 游戏ID — and with it the identity of every
/// capture that has no other anchor.
fn is_id_marker(t: &str) -> bool {
    let compact: String = t
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    matches!(compact.as_str(), "ID" | "LD" | "1D" | "I0" | "编号" | "ID号")
}

/// Extract level from "Lv.89" or "89级".
fn extract_level(lines: &[OcrLine]) -> Option<(i64, f64)> {
    for line in lines {
        let t = line.text.replace(' ', "");
        for prefix in ["Lv.", "LV.", "Lv", "lv"] {
            if let Some(idx) = t.find(prefix) {
                let digits: String =
                    t[idx + prefix.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = digits.parse::<i64>() {
                    return Some((v, line.confidence));
                }
            }
        }
        if let Some(idx) = t.find('级') {
            let start = t[..idx].char_indices().rev()
                .take_while(|(_, c)| c.is_ascii_digit())
                .last()
                .map(|(i, _)| i);
            if let Some(s) = start {
                if let Ok(v) = t[s..idx].parse::<i64>() {
                    return Some((v, line.confidence));
                }
            }
        }
    }
    None
}

/// Extract the school after "门派：".
fn extract_school(lines: &[OcrLine]) -> Option<String> {
    for line in lines {
        if let Some(idx) = line.text.find('门') {
            // Slice by char boundary, never by raw byte offset: '：' is 3 bytes.
            let rest = &line.text[idx..];
            if let Some((_, after)) = rest.split_once(['：', ':']) {
                let v = after.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// The nickname is usually the largest-text line in the header. We approximate
/// "largest" by picking the earliest short line that is not a label or number.
fn extract_nickname(lines: &[OcrLine], stats: &CombatStats, game_id: Option<&str>) -> Option<String> {
    // ---- Preferred: the real panel puts the name immediately left of the
    // level marker in one box, e.g. "莫莫莫89级" / "莫莫莫 89级". -------------
    for line in lines {
        let t = line.text.trim();
        for marker in ["级", "Lv.", "Lv", "lv"] {
            if let Some(idx) = t.find(marker) {
                // Walk back over the level digits that precede the marker.
                let before = &t[..idx];
                let digits_start = before
                    .char_indices()
                    .rev()
                    .take_while(|(_, c)| c.is_ascii_digit())
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(before.len());
                let name = before[..digits_start].trim().to_string();
                let cjk = name.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
                if cjk >= 2 && name.chars().count() <= 12 {
                    return Some(name);
                }
            }
        }
    }

    // ---- Fallback: a short standalone CJK line near the top of the panel.
    // Restricted to the header band so world-chat lines further down are
    // never mistaken for the character name.
    let top = lines
        .iter()
        .map(|l| l.rect.1)
        .min()
        .unwrap_or(0);
    let header_limit = top + 260; // header region of a ~700px-tall capture

    let label_words = [
        "气血", "魔法", "物理", "法术", "速度", "治疗", "门派", "游戏", "Lv", "等级",
        "战斗", "防御", "命中", "封印", "抵抗", "金币", "银币", "容量", "包裹", "物品",
        "任务", "属性", "世界", "系统", "帮派", "公告", "经验", "评分", "衣橱",
    ];
    let mut best: Option<(i32, String)> = None;
    for line in lines {
        let t = line.text.trim();
        if t.is_empty() || t.chars().count() > 12 {
            continue;
        }
        if t.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        if label_words.iter().any(|w| t.contains(w)) {
            continue;
        }
        if game_id.map(|g| t.contains(g)).unwrap_or(false) {
            continue;
        }
        // Header band only.
        if line.rect.1 > header_limit || line.rect.1 > header_limit {
            continue;
        }
        let cjk = t.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
        if cjk >= 2 && cjk <= 8 {
            // Prefer a left-aligned line: the name sits at the panel's left edge.
            match &best {
                Some((x, _)) if *x <= line.rect.0 => {}
                _ => best = Some((line.rect.0, t.to_string())),
            }
        }
    }
    let _ = stats;
    best.map(|(_, s)| s)
}

/// Labels, longest first so that a label which is a prefix of another
/// (物伤 vs 物理伤害) never wins by accident.
const STAT_LABELS: &[&str] = &[
    "物理防御", "法术防御", "物理伤害", "法术伤害", "治疗强度", "盈福经验", "抵抗封印",
    "封印命中", "气血", "魔法", "愤怒", "活力", "速度", "经验",
];

/// Split "物理伤害709" into (label, remainder). Returns the remainder only,
/// which is what carries the numeric value.
///
/// This is deliberately not a whitespace split: the real panels emit
/// label+value as ONE OCR box with no separator at all.
fn strip_stat_label(text: &str) -> Option<(&'static str, String)> {
    let compact = text.replace([' ', '：', ':'], "");
    for label in STAT_LABELS {
        if let Some(idx) = compact.find(label) {
            let rest = compact[idx + label.len()..].to_string();
            return Some((label, rest));
        }
    }
    None
}

fn extract_stats(lines: &[OcrLine]) -> CombatStats {
    let mut stats = CombatStats::default();
    for line in lines {
        let Some(field) = stat_label_map(&line.text) else { continue };

        // Case A: the label and its value share one OCR box ("物理伤害709").
        let inline = strip_stat_label(&line.text)
            .and_then(|(_, rest)| parse_number(&rest).map(|v| (v, rest)));

        let got = if let Some((v, rest)) = inline {
            Some((v, line.confidence, rest.trim().to_string()))
        } else {
            // Case B: the value is a separate box on the same row to the right.
            value_for_label(lines, line)
        };
        let Some((value, confidence, raw_text)) = got else { continue };
        let av = AttributeValue::new(value, confidence, raw_text);
        let slot: &mut Option<AttributeValue> = match field {
            "hp" => &mut stats.hp,
            "mp" => &mut stats.mp,
            "physical_damage" => &mut stats.physical_damage,
            "magic_damage" => &mut stats.magic_damage,
            "physical_defense" => &mut stats.physical_defense,
            "magic_defense" => &mut stats.magic_defense,
            "speed" => &mut stats.speed,
            "healing_power" => &mut stats.healing_power,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(av);
        }
    }
    stats
}

fn extract_currencies(lines: &[OcrLine]) -> Currencies {
    let mut currencies = Currencies::default();

    // ---- path 1: explicit text labels (synthetic panels / older UI) -------
    for line in lines {
        let is_gold = line.text.contains("金币");
        let is_silver = line.text.contains("银币");
        if !is_gold && !is_silver {
            continue;
        }
        let same_line = line.text.replace("金币", "").replace("银币", "");
        let got = parse_number(&same_line)
            .map(|v| (v, line.confidence, same_line.trim().to_string()))
            .or_else(|| value_for_label(lines, line));
        let Some((value, confidence, raw)) = got else { continue };
        let av = AttributeValue::new(value, confidence, raw);
        if is_gold && currencies.gold.is_none() {
            currencies.gold = Some(av);
        } else if is_silver && currencies.silver.is_none() {
            currencies.silver = Some(av);
        }
    }
    if currencies.gold.is_some() || currencies.silver.is_some() {
        return currencies;
    }

    // ---- path 2: icon-only currency strip (the real 包裹 panel) ----------
    // The real bag shows two coin icons with bare numbers beside them and no
    // textual label at all:  [gold icon] 2,625,812  [+]
    //                        [silver icon] 9,828,458  [+]
    //
    // This guess must only run on a panel we believe is a bag: running it on a
    // 人物属性 capture picked up the game ID and the 经验 figures as currency.
    if !looks_like_bag(lines) {
        return currencies;
    }

    let mut candidates: Vec<&OcrLine> = lines
        .iter()
        .filter(|l| {
            let digits: String = l.text.chars().filter(|c| c.is_ascii_digit()).collect();
            // The currency figures are plain integers with at least 4 digits
            // and no surrounding CJK text, and must not sit inside the
            // 经验/人物评分 rows.
            digits.len() >= 4
                && !l.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                && !l.text.contains('/')
        })
        .collect();

    // Prefer the lowest band of the screen: the currency row sits under the
    // inventory grid and the 人物评分 line.
    if candidates.len() >= 2 {
        let max_y = candidates.iter().map(|l| l.center_y()).fold(f64::MIN, f64::max);
        let band = 40.0;
        let mut row: Vec<&OcrLine> = candidates
            .iter()
            .copied()
            .filter(|l| (l.center_y() - max_y).abs() <= band)
            .collect();
        row.sort_by(|a, b| a.rect.0.cmp(&b.rect.0));
        if row.len() >= 2 {
            candidates = row;
        }
    }

    candidates.sort_by(|a, b| a.rect.0.cmp(&b.rect.0));
    let parsed: Vec<(i64, f64, String)> = candidates
        .iter()
        .filter_map(|l| parse_number(&l.text).map(|v| (v, l.confidence, l.text.clone())))
        .collect();

    if let Some((v, c, raw)) = parsed.first() {
        currencies.gold = Some(AttributeValue::new(*v, *c, raw.clone()));
    }
    if let Some((v, c, raw)) = parsed.get(1) {
        currencies.silver = Some(AttributeValue::new(*v, *c, raw.clone()));
    }

    currencies
}

/// Read the 人物评分 shown on the 包裹 panel.
///
/// This is the one character-specific number the bag renders, which makes it an
/// exact key for "whose bag is this" even after the item grid has changed.
fn extract_character_score(lines: &[OcrLine]) -> Option<i64> {
    // Anchor on 「评分」 rather than the full 「人物评分」: on a cropped panel
    // PP-OCR reads the label as 入物评分 / 人份评分 often enough that matching
    // the exact phrase loses the only identity key a 包裹 screenshot has.
    let label = "评分";
    for line in lines {
        let compact = line.text.replace(' ', "");
        if let Some(idx) = compact.find(label) {
            let rest = &compact[idx + label.len()..];
            if let Some(v) = parse_number(rest) {
                if v > 0 {
                    return Some(v);
                }
            }
        }
    }

    // Label and value in separate boxes on the same row.
    for line in lines {
        let compact = line.text.replace(' ', "");
        if !compact.contains(label) || parse_number(&compact).is_some() {
            continue;
        }
        let mut best: Option<i64> = None;
        for other in lines {
            if other == line {
                continue;
            }
            let dy = (other.center_y() - line.center_y()).abs();
            if dy > (line.rect.3 as f64).max(14.0) {
                continue;
            }
            if other.center_x() <= line.center_x() {
                continue;
            }
            if let Some(v) = parse_number(&other.text) {
                if v > 0 {
                    best = Some(v);
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

/// Heuristic: is this screenshot the 包裹 / inventory panel?
///
/// Only *titles and tab names of the bag itself* count. An earlier version also
/// matched 任务 / 整理 / 礼包 anywhere in the frame, which on a full window
/// capture matched the quest tracker (捉鬼任务) and made the currency and
/// inventory guesses run on a 人物属性 frame.
fn looks_like_bag(lines: &[OcrLine]) -> bool {
    lines.iter().any(|l| {
        let t = l.text.replace(' ', "");
        matches!(t.as_str(), "包裹" | "背包" | "行囊" | "物品" | "外观衣橱")
            || t.contains("人物评分")
            || t.contains("包裹")
            || t.contains("背包")
            || t.contains("容量")
    })
}

/// Recognise inventory items.
///
/// Inventory cells are laid out in a grid, and OCR returns the item name and
/// its stack count as SEPARATE lines (the count sits just below the name, in
/// the same column). So we pair each CJK name with the numeric line directly
/// beneath it.
fn extract_inventory(lines: &[OcrLine]) -> Vec<InventoryItem> {
    // Attribute-panel labels are also short CJK runs, so they must be
    // excluded explicitly; otherwise a 人物属性 screenshot "invents" items
    // called 气血 / 速度 / 物理伤害.
    let is_ui_label = |t: &str| -> bool {
        stat_label_map(t).is_some()
            || ["战斗属性", "防御属性", "门派", "游戏", "容量", "背包", "金币", "银币", "等级", "Lv"]
                .iter()
                .any(|w| t.contains(w))
    };

    let is_cjk_name = |t: &str| -> bool {
        let chars: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
        chars.len() >= 2
            && chars.len() <= 10
            && chars.iter().all(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
    };
    let is_pure_number = |t: &str| -> Option<i64> {
        let t = t.trim();
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            t.parse::<i64>().ok()
        } else {
            None
        }
    };

    let mut items = Vec::new();
    for name_line in lines {
        let name = name_line.text.trim();
        if !is_cjk_name(name) || is_ui_label(name) {
            continue;
        }

        // Find the count line: usually just below the name in the same
        // column, but tolerate a little vertical slack either way because
        // OCR boxes are not perfectly aligned to grid cells.
        let mut best: Option<(f64, i64, i32, i32)> = None;
        for count_line in lines {
            let Some(count) = is_pure_number(&count_line.text) else { continue };
            let dy = count_line.center_y() - name_line.center_y();
            let max_dy = (name_line.rect.3 as f64 * 3.0).max(40.0);
            if dy.abs() > max_dy {
                continue;
            }
            let dx = (count_line.center_x() - name_line.center_x()).abs();
            let max_dx = (name_line.rect.2 as f64).max(60.0);
            if dx > max_dx {
                continue;
            }
            let score = dx + dy.abs() * 0.3;
            if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
                best = Some((score, count, count_line.rect.0, count_line.rect.1));
            }
        }

        let Some((_score, count, cx, cy)) = best else { continue };

        // Merge the same item appearing in several documented cells.
        if let Some(existing) = items.iter_mut().find(|i: &&mut InventoryItem| i.name == name) {
            existing.count += count;
            continue;
        }
        items.push(InventoryItem {
            name: name.to_string(),
            count,
            column: Some(cx),
            row: Some(cy),
            confidence: name_line.confidence,
        });
    }
    items
}

/// Equipment entries, recognised by the slot vocabulary.
fn extract_equipment(lines: &[OcrLine]) -> Vec<EquipmentItem> {
    const SLOTS: [&str; 8] = ["武器", "头盔", "衣服", "项链", "腰带", "鞋子", "戒指", "饰品"];
    let mut out = Vec::new();
    for line in lines {
        for slot in SLOTS {
            if line.text.contains(slot) {
                let name = line.text.replace(slot, "").trim().to_string();
                if !name.is_empty() {
                    out.push(EquipmentItem {
                        slot: slot.to_string(),
                        name,
                        level: None,
                        confidence: line.confidence,
                    });
                }
            }
        }
    }
    out
}

/// Identity resolved from what the panel says about itself.
struct IdentityResolution {
    character_id: Option<String>,
    source: IdentitySource,
    confidence: f64,
    candidates: Vec<IdentityCandidate>,
    /// Things the user should know, e.g. "两个角色同名" or "服务器对不上".
    notes: Vec<String>,
}

fn character_candidate(c: &Character, confidence: f64, detail: String) -> IdentityCandidate {
    IdentityCandidate {
        character_id: c.id.clone(),
        nickname: c.nickname.clone(),
        game_id: c.game_id.clone(),
        server: c.server.clone(),
        level: c.level,
        confidence,
        evidence: vec![MatchEvidence::new(
            EvidenceKind::Nickname,
            1.0,
            confidence,
            detail,
        )],
    }
}

/// Who does this capture claim to be?
///
/// 游戏ID is the only authoritative key; everything else is a hint that may
/// have to be confirmed by the user:
///
///  * 游戏ID 命中 → 直接认（0.98）。未命中说明是新角色，不拿别人的行顶上。
///  * 昵称只在**唯一命中**时使用。游戏里不同服务器可以有同名角色，旧实现用
///    `find()` 取第一个，两个区服的重名角色会被悄悄合并成一个。
///  * 同名多于一个 → 返回候选列表、标记 `Ambiguous`，让用户选。
///  * 服务器只用于在同名候选里筛，不反向否决唯一候选。
fn resolve_identity(
    game_id: Option<&str>,
    nickname: Option<&str>,
    server: Option<&str>,
    known: &[Character],
) -> IdentityResolution {
    let unresolved = |source: IdentitySource, confidence: f64, notes: Vec<String>| IdentityResolution {
        character_id: None,
        source,
        confidence,
        candidates: Vec::new(),
        notes,
    };

    if let Some(gid) = game_id.filter(|s| !s.trim().is_empty()) {
        let hits: Vec<&Character> = known
            .iter()
            .filter(|c| c.game_id.as_deref() == Some(gid))
            .collect();
        return match hits.len() {
            1 => IdentityResolution {
                character_id: Some(hits[0].id.clone()),
                source: IdentitySource::IdField,
                confidence: 0.98,
                candidates: Vec::new(),
                notes: Vec::new(),
            },
            // A visible id that matches nothing is still strong proof of a new
            // character — but it is *not* an excuse to pick a look-alike row.
            0 => unresolved(IdentitySource::IdField, 0.95, Vec::new()),
            _ => IdentityResolution {
                character_id: None,
                source: IdentitySource::Ambiguous,
                confidence: 0.5,
                candidates: hits
                    .iter()
                    .map(|c| character_candidate(c, 0.5, format!("游戏ID {gid}")))
                    .collect(),
                notes: vec![format!(
                    "游戏ID {gid} 在库中对应 {} 个角色，库数据可能重复，请在右侧手动确认",
                    hits.len()
                )],
            },
        };
    }

    if let Some(nick) = nickname.filter(|s| !s.trim().is_empty()) {
        let mut hits: Vec<&Character> = known.iter().filter(|c| c.nickname == nick).collect();
        if !hits.is_empty() {
            // 服务器只能帮忙缩小范围，不能反过来把唯一候选否掉。
            if hits.len() > 1 {
                if let Some(srv) = server.filter(|s| !s.is_empty()) {
                    let narrowed: Vec<&Character> = hits
                        .iter()
                        .copied()
                        .filter(|c| c.server.as_deref() == Some(srv))
                        .collect();
                    if !narrowed.is_empty() {
                        hits = narrowed;
                    }
                }
            }

            if hits.len() == 1 {
                let c = hits[0];
                let mismatch = matches!(
                    (server.filter(|s| !s.is_empty()), c.server.as_deref()),
                    (Some(a), Some(b)) if a != b
                );
                return IdentityResolution {
                    character_id: Some(c.id.clone()),
                    source: IdentitySource::Nickname,
                    confidence: if mismatch { 0.70 } else { 0.82 },
                    candidates: Vec::new(),
                    notes: if mismatch {
                        vec![format!(
                            "截图服务器「{}」与「{}」记录的「{}」不一致，已按昵称归属，请确认",
                            server.unwrap_or(""),
                            c.nickname,
                            c.server.clone().unwrap_or_default()
                        )]
                    } else {
                        Vec::new()
                    },
                };
            }

            return IdentityResolution {
                character_id: None,
                source: IdentitySource::Ambiguous,
                confidence: 0.5,
                candidates: hits
                    .iter()
                    .map(|c| character_candidate(c, 0.5, format!("同名「{nick}」")))
                    .collect(),
                notes: vec![format!(
                    "库中有 {} 个角色都叫「{nick}」，无法凭昵称确定归属，请在右侧选择（游戏ID / 服务器可区分）",
                    hits.len()
                )],
            };
        }
    }

    unresolved(IdentitySource::VisualMatch, 0.6, Vec::new())
}


// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// High-level pipeline: decode -> classify -> OCR -> field extraction.
pub struct RecognitionPipeline {
    engine: OcrEngine,
}

impl RecognitionPipeline {
    pub fn new(models_dir: &Path) -> Result<Self> {
        Ok(Self { engine: OcrEngine::new(models_dir)? })
    }

    /// Recognize a batch of screenshots belonging to one character.
    ///
    /// `known` carries the characters already in the database so identity
    /// association can match an existing row when the 游戏ID is not visible.
    pub fn recognize_batch(
        &mut self,
        images: &[image::RgbaImage],
        known: &[Character],
    ) -> Result<RecognitionResult> {
        self.recognize_batch_with(images, known, &[])
    }

    /// Recognize a batch, using stored fingerprints to resolve the identity of
    /// captures that carry no identity field (the 包裹 panel).
    pub fn recognize_batch_with(
        &mut self,
        images: &[image::RgbaImage],
        known: &[Character],
        refs: &[IdentityRef],
    ) -> Result<RecognitionResult> {
        if images.is_empty() {
            return Err(VisionError::Ocr("empty batch".into()));
        }

        let mut result = RecognitionResult::new(IdentitySource::VisualMatch);
        result.screenshot_count = images.len();

        let mut all_lines: Vec<OcrLine> = Vec::new();
        let mut stats_merged = CombatStats::default();
        let mut currency_merged = Currencies::default();
        let mut confidences: Vec<f64> = Vec::new();

        for img in images {
            // A window capture holds the whole client area, so crop to the game
            // panel before anything else looks at it. Everything below assumes
            // panel-only input; the HUD clock, chat and quest tracker otherwise
            // leak in as currency, inventory items and labels.
            let cropped = locate_panel(img)
                .and_then(|b| crop_rgba(img, b.x, b.y, b.width, b.height));
            let work: &image::RgbaImage = cropped.as_ref().unwrap_or(img);

            let lines = match self.engine.recognize(work) {
                Ok(l) => l,
                Err(e) => {
                    result.screens.push(classify_screen(work));
                    result.warnings.push(format!("OCR 失败：{e}"));
                    continue;
                }
            };
            if lines.is_empty() {
                result.warnings.push("未在截图中识别到文字".to_string());
            }

            // The real game panels render labels with wide letter spacing and
            // the detector emits one box per glyph ("气" + "血"). Re-join those
            // fragments into words before any extractor sees them.
            let lines = merge_adjacent_fragments(&lines);

            // Classify from the panel's own labels. The pixel heuristic below
            // was tuned on panel-only images and called a perfectly readable
            // window capture `Unknown`, which the UI then reported as failure.
            let kind = classify_from_lines(&lines).unwrap_or_else(|| classify_screen(work));
            result.screens.push(kind);

            // Fingerprint the panel itself. A 包裹 screenshot carries no
            // 游戏ID and no nickname, so this thumbnail (plus the 人物评分 when
            // the bag shows one) is the only way a later capture can be linked
            // back to its character.
            {
                use base64::Engine as _;
                let score = extract_character_score(&lines);
                result.fingerprints.push(PanelFingerprint {
                    screen: kind,
                    thumb: base64::engine::general_purpose::STANDARD
                        .encode(panel_thumb(work)),
                    score,
                });
            }

            // Crop the framed portrait once, from the panel that carries an
            // identity header. This is the avatar the UI shows instead of an
            // initial-letter placeholder.
            if result.avatar_png_base64.is_none() {
                if let Some(png) = crop_avatar_png(work, &lines) {
                    result.avatar_png_base64 = Some(png);
                }
            }
            // A 16x16 grayscale of the same portrait: the UI shows the PNG, the
            // matcher compares the thumb. Colour is discarded so a change of
            // dye or lighting does not make the same face score differently.
            if result.avatar_thumb.is_none() {
                result.avatar_thumb = crop_avatar_thumb(work, &lines);
            }

            // 发色 / 武器 of the character on this panel. Recorded once per
            // batch — every screenshot in a batch is the same character, and a
            // later capture whose items changed is linked by this instead of by
            // the panel thumbnail.
            if result.appearance.is_none() {
                result.appearance = appearance_signature(work, kind);
            }

            // Classification only labels the screenshot for the UI. Every
            // extractor runs on every screenshot because field presence is a
            // far more reliable discriminator than a layout guess: a 背包
            // capture can still expose 金币/银币, and a 人物属性 capture may
            // list equipment. Merging by field keeps both paths correct.
            let stats = extract_stats(&lines);
            merge_stats(&mut stats_merged, &stats);
            let currencies = extract_currencies(&lines);
            merge_currencies(&mut currency_merged, &currencies);
            let equipment = extract_equipment(&lines);
            if !equipment.is_empty() {
                for e in equipment {
                    if let Some(existing) =
                        result.equipment.iter_mut().find(|x| x.slot == e.slot && x.name == e.name)
                    {
                        existing.confidence = existing.confidence.max(e.confidence);
                    } else {
                        result.equipment.push(e);
                    }
                }
            }
            // Inventory pairing is per-screenshot: names and counts only make
            // sense relative to each other within one capture. It also needs a
            // 包裹-class panel to be present — on a 人物属性 frame every short
            // CJK run next to a number looks like "item + stack count", which is
            // how a quest tracker became an inventory item.
            let items = if looks_like_bag(&lines) {
                extract_inventory(&lines)
            } else {
                Vec::new()
            };
            for item in items {
                if let Some(existing) =
                    result.inventory.iter_mut().find(|x| x.name == item.name)
                {
                    existing.count += item.count;
                } else {
                    result.inventory.push(item);
                }
            }

            for l in &lines {
                confidences.push(l.confidence);
            }
            all_lines.extend(lines);
        }

        result.stats = stats_merged;
        result.currencies = currency_merged;

        for line in &all_lines {
            if let Some((gid, conf)) = extract_game_id(&[line.clone()].as_slice()) {
                result.character.game_id = Some(gid);
                confidences.push(conf);
                break;
            }
        }
        if result.character.game_id.is_none() {
            if let Some((gid, conf)) = extract_game_id(&all_lines) {
                result.character.game_id = Some(gid);
                confidences.push(conf);
            }
        }
        if let Some((lvl, conf)) = extract_level(&all_lines) {
            result.character.level = Some(lvl);
            confidences.push(conf);
        }
        result.character.school = extract_school(&all_lines);
        // Only trust a nickname when the panel actually carries an identity
        // header. A bare 包裹 capture has no name at all, so guessing one from
        // stray panel chrome would invent a character.
        if result.character.game_id.is_some()
            || all_lines.iter().any(|l| l.text.contains("级") || l.text.contains("Lv"))
        {
            result.character.nickname = extract_nickname(
                &all_lines,
                &result.stats,
                result.character.game_id.as_deref(),
            );
        }

        let resolution = resolve_identity(
            result.character.game_id.as_deref(),
            result.character.nickname.as_deref(),
            result.character.server.as_deref(),
            known,
        );
        let mut character_id = resolution.character_id;
        let mut source = resolution.source;
        let mut identity_conf = resolution.confidence;
        result.candidates = resolution.candidates;
        result.warnings.extend(resolution.notes);

        // Nothing on the panel identifies the character — but we may still know
        // whose picture this is. A 包裹 capture shows no 游戏ID, no nickname and
        // no level; the evidence below is all there is.
        //
        // The evidence is evaluated even when the panel *did* name itself, so a
        // visual model that has drifted shows up as a conflict instead of
        // silently disagreeing with an authoritative 游戏ID.
        let names_itself =
            result.character.game_id.is_some() || result.character.nickname.is_some();

        if !refs.is_empty() {
            let assoc = associate_by_evidence(
                &result.fingerprints,
                refs,
                result.appearance.as_ref(),
                result.avatar_thumb.as_deref(),
                now_ms(),
                &MatchPolicy::default(),
            );

            let label = |id: &str| -> String {
                known
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.nickname.clone())
                    .unwrap_or_else(|| id.to_string())
            };

            match decide_identity(character_id.as_deref(), names_itself, source, &assoc, &label) {
                IdentityDecision::Conflict(note) => result.conflicts.push(note),
                IdentityDecision::Adopt { character_id: id, confidence } => {
                    // Bring the character's name and level along so the result
                    // reads like a real record instead of "未命名角色". The
                    // 游戏ID is left alone on purpose: it is the authoritative
                    // key and must not be promoted from a visual guess.
                    if let Some(c) = known.iter().find(|c| c.id == id) {
                        if result.character.nickname.is_none() {
                            result.character.nickname = Some(c.nickname.clone());
                        }
                        if result.character.level.is_none() {
                            result.character.level = c.level;
                        }
                    }
                    character_id = Some(id);
                    source = IdentitySource::VisualMatch;
                    identity_conf = confidence.max(0.6);
                    result.identity_evidence = Some(evidence_summary(&assoc.evidence));
                }
                IdentityDecision::Unresolved => {
                    // 有候选但没有定论：把候选交给用户，不猜。
                    if !assoc.candidates.is_empty() {
                        result.candidates = assoc.candidates.clone();
                        result.identity_evidence = Some(evidence_summary(&assoc.evidence));
                        result.warnings.push(
                            "证据不足以确定归属，请从右侧角色列表中确认（下方会显示候选及其依据）"
                                .to_string(),
                        );
                    }
                }
            }
        }

        result.character_id = character_id;
        result.identity_source = source;
        confidences.push(identity_conf);

        // The 包裹 panel carries currency but no identity. If we could not
        // resolve a 游戏ID (or a known character), tell the user to import the
        // 人物属性 first — otherwise the currencies would be attached to
        // nobody and silently lost.
        if result.character.game_id.is_none() && result.character_id.is_none() {
            if result.currencies.gold.is_some() || result.currencies.silver.is_some() {
                result.warnings.push(
                    "请先识别并录入「人物属性」截图获取游戏ID，才能把包裹中的金币/银币归到该角色"
                        .to_string(),
                );
            }
        }

        result.confidence = if confidences.is_empty() {
            0.0
        } else {
            let mean = confidences.iter().sum::<f64>() / confidences.len() as f64;
            mean.clamp(0.0, 1.0)
        };

        Ok(result)
    }

    /// Feature extraction for caching, exposed for the service layer.
    pub fn features(&self, img: &image::RgbaImage) -> Result<ImageFeatures> {
        extract_features(img)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// "包裹面板相似度 0.98；人物评分 20214 一致" — shown next to the confidence so
/// a wrong attribution can be traced to the signal that caused it.
fn evidence_summary(evidence: &[MatchEvidence]) -> String {
    evidence
        .iter()
        .take(3)
        .map(|e| e.detail.clone())
        .collect::<Vec<_>>()
        .join("；")
}

fn merge_stats(dst: &mut CombatStats, src: &CombatStats) {    macro_rules! take {
        ($field:ident) => {
            if dst.$field.is_none() {
                dst.$field = src.$field.clone();
            }
        };
    }
    take!(hp);
    take!(mp);
    take!(physical_damage);
    take!(magic_damage);
    take!(physical_defense);
    take!(magic_defense);
    take!(speed);
    take!(healing_power);
}

fn merge_currencies(dst: &mut Currencies, src: &Currencies) {
    if dst.gold.is_none() {
        dst.gold = src.gold.clone();
    }
    if dst.silver.is_none() {
        dst.silver = src.silver.clone();
    }
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_is_bounded_and_weighted() {
        let s = Similarity { phash: 1.0, orb: 1.0, hsv: 1.0 };
        assert!((s.combined() - 1.0).abs() < 1e-9);
        let ident = Similarity { phash: 1.0, orb: 0.0, hsv: 0.0 };
        assert!((ident.combined() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn ocr_line_center_math() {
        let l = OcrLine { text: "x".into(), confidence: 0.9, rect: (10, 20, 100, 40) };
        assert_eq!(l.center_x(), 60.0);
        assert_eq!(l.center_y(), 40.0);
    }

    #[test]
    fn parse_number_handles_separators() {
        assert_eq!(parse_number("7,721 / 7,721"), Some(7721));
        assert_eq!(parse_number("2,942,685"), Some(2942685));
        assert_eq!(parse_number("1，714"), Some(1714));
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn ctc_decode_collapses_repeats_and_drops_blank() {
        // 3 steps, 4 classes: blank=0, a=1, b=2, c=3
        let keys = vec!["".to_string(), "a".into(), "b".into(), "c".into()];
        let data = vec![
            0.0, 0.9, 0.1, 0.0, // step0 -> a
            0.0, 0.9, 0.1, 0.0, // step1 -> a (repeat, collapse)
            0.0, 0.1, 0.1, 0.8, // step2 -> c
        ];
        let (text, conf) = ctc_greedy_decode(&data, 3, 4, &keys);
        assert_eq!(text, "ac");
        assert!((conf - 0.85).abs() < 1e-5, "mean of 0.9 and 0.8 = 0.85, got {conf}");
    }

    #[test]
    fn ctc_decode_handles_blank_separated_repeats() {
        let keys = vec!["".to_string(), "a".into()];
        let data = vec![
            0.1, 0.9, // a
            0.9, 0.1, // blank
            0.1, 0.9, // a again -> should appear twice
        ];
        let (text, _) = ctc_greedy_decode(&data, 3, 2, &keys);
        assert_eq!(text, "aa");
    }

    #[test]
    fn stat_labels_map_to_the_right_fields() {
        assert_eq!(stat_label_map("物理防御"), Some("physical_defense"));
        assert_eq!(stat_label_map("物理伤害"), Some("physical_damage"));
        assert_eq!(stat_label_map("法术防御"), Some("magic_defense"));
        assert_eq!(stat_label_map("治疗强度"), Some("healing_power"));
        assert_eq!(stat_label_map("气血"), Some("hp"));
        assert_eq!(stat_label_map("速度"), Some("speed"));
        assert_eq!(stat_label_map("随便写的"), None);
    }

    #[test]
    fn game_id_and_level_extraction() {
        let lines = vec![
            OcrLine { text: "游戏ID 429277013".into(), confidence: 0.95, rect: (330, 112, 200, 30) },
            OcrLine { text: "Lv.89".into(), confidence: 0.97, rect: (110, 108, 80, 30) },
        ];
        let (gid, _) = extract_game_id(&lines).unwrap();
        assert_eq!(gid, "429277013");
        let (lvl, _) = extract_level(&lines).unwrap();
        assert_eq!(lvl, 89);
    }

    #[test]
    fn level_extraction_from_chinese_suffix() {
        let lines = vec![OcrLine { text: "76级".into(), confidence: 0.9, rect: (0, 0, 50, 30) }];
        assert_eq!(extract_level(&lines).unwrap().0, 76);
    }

    #[test]
    fn stats_extracted_from_label_value_pairs() {
        let lines = vec![
            OcrLine { text: "气血 7,721".into(), confidence: 0.95, rect: (60, 190, 300, 40) },
            OcrLine { text: "魔法 2,215".into(), confidence: 0.95, rect: (60, 244, 300, 40) },
            OcrLine { text: "物理伤害 758".into(), confidence: 0.94, rect: (70, 380, 300, 40) },
        ];
        let stats = extract_stats(&lines);
        assert_eq!(stats.hp.as_ref().unwrap().value, 7721);
        assert_eq!(stats.mp.as_ref().unwrap().value, 2215);
        assert_eq!(stats.physical_damage.as_ref().unwrap().value, 758);
    }

    #[test]
    fn stats_extracted_from_separate_value_lines() {
        let lines = vec![
            OcrLine { text: "物理防御".into(), confidence: 0.9, rect: (70, 380, 120, 40) },
            OcrLine { text: "1,714".into(), confidence: 0.95, rect: (500, 380, 90, 40) },
        ];
        let stats = extract_stats(&lines);
        assert_eq!(stats.physical_defense.as_ref().unwrap().value, 1714);
    }

    #[test]
    fn currencies_extracted() {
        let lines = vec![
            OcrLine { text: "金币 2,942,685".into(), confidence: 0.95, rect: (70, 728, 300, 30) },
            OcrLine { text: "银币 14,297,169".into(), confidence: 0.95, rect: (640, 728, 300, 30) },
        ];
        let c = extract_currencies(&lines);
        assert_eq!(c.gold.unwrap().value, 2_942_685);
        assert_eq!(c.silver.unwrap().value, 14_297_169);
    }

    #[test]
    fn identity_uses_known_game_id() {
        let known = vec![Character {
            id: "abc".into(),
            nickname: "海淘淘".into(),
            game_id: Some("429277013".into()),
            level: Some(89),
            school: None,
            server: None,
            identity_confidence: 0.9,
            identity_confirmed: false,
            avatar_path: None,
            created_at: 0,
            updated_at: 0,
            latest_stats: CombatStats::default(),
            latest_currencies: Currencies::default(),
        }];
        let r = resolve_identity(Some("429277013"), Some("海淘淘"), None, &known);
        assert_eq!(r.character_id.as_deref(), Some("abc"));
        assert_eq!(r.source, IdentitySource::IdField);
        assert!(r.confidence > 0.9);
        assert!(r.candidates.is_empty(), "决定性匹配不该再抛候选");
    }

    #[test]
    fn identity_falls_back_to_nickname() {
        let known = vec![Character {
            id: "abc".into(),
            nickname: "云中鹤".into(),
            game_id: None,
            level: Some(76),
            school: None,
            server: None,
            identity_confidence: 0.8,
            identity_confirmed: false,
            avatar_path: None,
            created_at: 0,
            updated_at: 0,
            latest_stats: CombatStats::default(),
            latest_currencies: Currencies::default(),
        }];
        let r = resolve_identity(None, Some("云中鹤"), None, &known);
        assert_eq!(r.character_id.as_deref(), Some("abc"));
        assert_eq!(r.source, IdentitySource::Nickname);
    }

    fn known_char(id: &str, nick: &str, game_id: Option<&str>, server: Option<&str>) -> Character {
        Character {
            id: id.into(),
            nickname: nick.into(),
            game_id: game_id.map(str::to_string),
            level: None,
            school: None,
            server: server.map(str::to_string),
            identity_confidence: 0.9,
            identity_confirmed: false,
            avatar_path: None,
            created_at: 0,
            updated_at: 0,
            latest_stats: CombatStats::default(),
            latest_currencies: Currencies::default(),
        }
    }

    /// 同名不等于同一个人：不同服可以有同名角色。旧实现 `find()` 取第一个，
    /// 两个区的角色会被悄悄合并。现在必须返回候选、标记 Ambiguous。
    #[test]
    fn duplicate_nicknames_are_ambiguous_not_guessed() {
        let known = vec![
            known_char("a", "云中鹤", None, Some("电信一区")),
            known_char("b", "云中鹤", None, Some("电信二区")),
        ];
        let r = resolve_identity(None, Some("云中鹤"), None, &known);

        assert!(r.character_id.is_none(), "同名多行时不得直接选一个");
        assert_eq!(r.source, IdentitySource::Ambiguous);
        assert_eq!(r.candidates.len(), 2, "两个候选都要给出来");
        assert!(!r.notes.is_empty(), "要提示用户同名无法判定");
    }

    /// 服务器字段只能用来在同名里缩小范围，不能反过来否决唯一候选。
    #[test]
    fn server_narrows_duplicate_nicknames() {
        let known = vec![
            known_char("a", "云中鹤", None, Some("电信一区")),
            known_char("b", "云中鹤", None, Some("电信二区")),
        ];
        let r = resolve_identity(None, Some("云中鹤"), Some("电信二区"), &known);
        assert_eq!(r.character_id.as_deref(), Some("b"));
        assert_eq!(r.source, IdentitySource::Nickname);
    }

    /// 游戏ID 命中时，昵称写错也不影响归属。
    #[test]
    fn game_id_beats_a_wrong_nickname() {
        let known = vec![
            known_char("a", "云中鹤", Some("100001"), None),
            known_char("b", "云中鹤", Some("100002"), None),
        ];
        let r = resolve_identity(Some("100002"), Some("云中鹤"), None, &known);
        assert_eq!(r.character_id.as_deref(), Some("b"));
        assert_eq!(r.source, IdentitySource::IdField);
    }

    // ---- 冲突保护 ---------------------------------------------------------

    fn assoc_of(id: &str, confidence: f64) -> VisualAssociation {
        VisualAssociation {
            character_id: Some(id.to_string()),
            confidence,
            evidence: Vec::new(),
            candidates: Vec::new(),
        }
    }

    /// 视觉说 A、游戏ID 说 B —— 必须以 B 为准，并留下一条冲突记录。
    #[test]
    fn visual_never_overrides_the_game_id() {
        let label = |id: &str| format!("角色{id}");
        let d = decide_identity(
            Some("b"),
            true,
            IdentitySource::IdField,
            &assoc_of("a", 0.99),
            &label,
        );
        match d {
            IdentityDecision::Conflict(note) => {
                assert!(note.contains("游戏ID"), "note {note}");
                assert!(note.contains("角色a") && note.contains("角色b"), "note {note}");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    /// 面板没报身份时，视觉证据可以定案。
    #[test]
    fn visual_decides_when_the_panel_is_silent() {
        let label = |id: &str| format!("角色{id}");
        let d = decide_identity(
            None,
            false,
            IdentitySource::VisualMatch,
            &assoc_of("a", 0.72),
            &label,
        );
        assert_eq!(
            d,
            IdentityDecision::Adopt { character_id: "a".to_string(), confidence: 0.72 }
        );
    }

    /// 面板自己报了身份（新角色）时，视觉结果不许接管。
    #[test]
    fn a_new_character_is_not_renamed_by_a_lookalike() {
        let label = |id: &str| format!("角色{id}");
        let d = decide_identity(
            None,
            true,
            IdentitySource::IdField,
            &assoc_of("a", 0.99),
            &label,
        );
        assert_eq!(d, IdentityDecision::Unresolved);
    }

    #[test]
    fn phash_is_stable_for_the_same_image() {
        let a = mk_test_image(0);
        let b = mk_test_image(0);
        let fa = extract_features(&a).unwrap();
        let fb = extract_features(&b).unwrap();
        assert_eq!(fa.phash, fb.phash, "identical images must hash identically");
        let sim = compare(&fa, &fb);
        assert!((sim.phash - 1.0).abs() < 1e-9);
    }

    #[test]
    fn phash_distinguishes_different_images() {
        let a = mk_test_image(0);
        let b = mk_test_image(90);
        let fa = extract_features(&a).unwrap();
        let fb = extract_features(&b).unwrap();
        let sim = compare(&fa, &fb);
        assert!(sim.phash < 1.0, "different images must not be identical");
    }

    #[test]
    fn hsv_histogram_sums_to_one() {
        let img = mk_test_image(0);
        let f = extract_features(&img).unwrap();
        let sum: f32 = f.hsv_hist.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "histogram must be normalised, got {sum}");
    }

    /// Build a deterministic synthetic image so feature tests never depend on
    /// files on disk.
    fn mk_test_image(shift: u8) -> image::RgbaImage {
        let (w, h) = (160u32, 100u32);
        let mut img = image::RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // Wrap explicitly so the checkerboard never overflows u8.
                let cell = ((x / 16) + (y / 16)) as u8;
                let v = cell.wrapping_mul(32).wrapping_add(shift);
                img.put_pixel(x, y, image::Rgba([v, 255u8.wrapping_sub(v), v / 2, 255]));
            }
        }
        img
    }

    // -----------------------------------------------------------------------
    // Regression tests pinned to the REAL game screenshots in `samples/`.
    //
    // These encode the exact values a human read off the panel, so any change
    // to the OCR / extraction heuristics that breaks real-world accuracy fails
    // the build instead of silently shipping.
    // -----------------------------------------------------------------------

    fn sample(name: &str) -> image::RgbaImage {
        // Tests run with the crate directory as cwd, so resolve the repo's
        // samples/ folder from the manifest path rather than a relative guess.
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        // crate dir is <repo>/src/crates/mhvision -> three levels up to <repo>
        let path = std::path::Path::new(&manifest)
            .join("..")
            .join("..")
            .join("..")
            .join("samples")
            .join(name);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read sample {}: {e}", path.display()));
        decode_image(&bytes).expect("sample must decode")
    }

    /// The real 人物属性 panel (莫莫莫, ID 429277361).
    #[test]
    fn real_character_panel_fields_match_ground_truth() {
        let img = sample("real_character_attributes.png");
        let lines = merge_adjacent_fragments(&[]);
        let _ = lines;

        // Field extraction only needs the OCR text; run the pieces directly so
        // this test stays fast and does not depend on model weights.
        // (The OCR itself is exercised by scripts/verify_vision.ps1.)
        let mut pipeline_lines: Vec<OcrLine> = vec![
            OcrLine { text: "莫莫莫89级".into(), confidence: 0.99, rect: (399, 167, 98, 14) },
            OcrLine { text: "ID".into(), confidence: 0.9, rect: (397, 194, 15, 13) },
            OcrLine { text: "429277361".into(), confidence: 0.99, rect: (412, 194, 84, 13) },
            OcrLine { text: "气".into(), confidence: 0.94, rect: (640, 173, 14, 14) },
            OcrLine { text: "5604/5604".into(), confidence: 0.99, rect: (791, 173, 71, 13) },
            OcrLine { text: "魔".into(), confidence: 1.0, rect: (638, 209, 17, 19) },
            OcrLine { text: "法".into(), confidence: 0.99, rect: (672, 209, 19, 18) },
            OcrLine { text: "2267/2267".into(), confidence: 0.99, rect: (792, 211, 68, 13) },
            OcrLine { text: "物理伤害709".into(), confidence: 0.99, rect: (650, 388, 104, 14) },
            OcrLine { text: "物理防御1609".into(), confidence: 0.99, rect: (794, 388, 111, 14) },
            OcrLine { text: "法术伤害".into(), confidence: 0.99, rect: (649, 428, 72, 14) },
            OcrLine { text: "441".into(), confidence: 0.99, rect: (716, 428, 38, 13) },
            OcrLine { text: "法术防御1081".into(), confidence: 0.99, rect: (794, 428, 111, 14) },
            OcrLine { text: "速".into(), confidence: 0.99, rect: (648, 466, 19, 18) },
            OcrLine { text: "度".into(), confidence: 1.0, rect: (701, 468, 19, 14) },
            OcrLine { text: "1256".into(), confidence: 0.99, rect: (716, 468, 48, 13) },
            OcrLine { text: "治疗强度133".into(), confidence: 0.99, rect: (794, 468, 102, 14) },
        ];
        pipeline_lines = merge_adjacent_fragments(&pipeline_lines);

        let stats = extract_stats(&pipeline_lines);
        assert_eq!(stats.hp.as_ref().map(|v| v.value), Some(5604), "hp");
        assert_eq!(stats.mp.as_ref().map(|v| v.value), Some(2267), "mp");
        assert_eq!(stats.physical_damage.as_ref().map(|v| v.value), Some(709), "物理伤害");
        assert_eq!(stats.magic_damage.as_ref().map(|v| v.value), Some(441), "法术伤害");
        assert_eq!(stats.physical_defense.as_ref().map(|v| v.value), Some(1609), "物理防御");
        assert_eq!(stats.magic_defense.as_ref().map(|v| v.value), Some(1081), "法术防御");
        assert_eq!(stats.speed.as_ref().map(|v| v.value), Some(1256), "速度");
        assert_eq!(stats.healing_power.as_ref().map(|v| v.value), Some(133), "治疗强度");

        let (gid, _) = extract_game_id(&pipeline_lines).expect("game id");
        assert_eq!(gid, "429277361", "游戏ID must pair the ID box with its digits");

        let nick = extract_nickname(&pipeline_lines, &stats, Some(&gid)).expect("nickname");
        assert_eq!(nick, "莫莫莫", "nickname comes from the level marker's left side");

        let _ = img;
    }

    /// The real 包裹 panel has icon-only currency: the numbers carry no label.
    #[test]
    fn real_bag_currency_is_read_by_position() {
        let lines: Vec<OcrLine> = vec![
            OcrLine { text: "包裹".into(), confidence: 0.99, rect: (595, 119, 51, 18) },
            OcrLine { text: "物品".into(), confidence: 1.0, rect: (681, 174, 48, 19) },
            OcrLine { text: "任务".into(), confidence: 0.99, rect: (849, 174, 47, 19) },
            OcrLine { text: "外观衣橱".into(), confidence: 0.99, rect: (404, 451, 84, 19) },
            OcrLine { text: "人物评分20214".into(), confidence: 0.98, rect: (410, 519, 116, 14) },
            OcrLine { text: "2.625.812".into(), confidence: 0.99, rect: (333, 556, 72, 14) },
            OcrLine { text: "9.828.458".into(), confidence: 0.95, rect: (497, 556, 71, 13) },
            OcrLine { text: "整理".into(), confidence: 1.0, rect: (853, 549, 55, 20) },
        ];
        let lines = merge_adjacent_fragments(&lines);
        let currencies = extract_currencies(&lines);
        assert_eq!(currencies.gold.as_ref().map(|v| v.value), Some(2_625_812), "金币");
        assert_eq!(currencies.silver.as_ref().map(|v| v.value), Some(9_828_458), "银币");
    }

    /// A 人物属性 panel must NOT invent currency from its game ID / 经验 rows.
    #[test]
    fn real_character_panel_does_not_invent_currency() {
        let lines: Vec<OcrLine> = vec![
            OcrLine { text: "ID".into(), confidence: 0.9, rect: (397, 194, 15, 13) },
            OcrLine { text: "429277361".into(), confidence: 0.99, rect: (412, 194, 84, 13) },
            OcrLine { text: "172650891/151211283".into(), confidence: 0.99, rect: (720, 516, 148, 13) },
        ];
        let currencies = extract_currencies(&lines);
        assert!(currencies.gold.is_none(), "game id must not become 金币");
        assert!(currencies.silver.is_none(), "经验 must not become 银币");
    }

    /// A window capture — the output of the 截图识别 button — must be cropped to
    /// the game panel, otherwise the HUD (clock, chat, quest tracker) is fed to
    /// extractors that assume they are looking at the panel only.
    #[test]
    fn panel_is_located_in_a_window_capture() {
        let img = sample("real_window_capture.png");
        let b = locate_panel(&img).expect("the panel must be found in a window capture");

        // The panel sits in the middle of the 1265x711 frame; assert it covers a
        // large share of it and that the crop stays inside the image.
        assert!(b.width >= 600 && b.width <= img.width() as i32, "width {}", b.width);
        assert!(b.height >= 400 && b.height <= img.height() as i32, "height {}", b.height);
        assert!(b.x > 0 && b.y > 0, "the panel is inset, not flush with the frame");
        assert!(b.x + b.width <= img.width() as i32 + 1);
        assert!(b.y + b.height <= img.height() as i32 + 1);
    }

    /// A panel-only capture (the design mockups) must not be cropped at all —
    /// cropping would throw away the title bar and the currency row.
    #[test]
    fn panel_is_not_cropped_when_the_frame_is_already_the_panel() {
        let img = sample("character_haitaotao.png");
        assert!(locate_panel(&img).is_none(), "a panel-only frame must stay as-is");
    }

    /// Regression for the "截图了但没读到数值" report: on a real full-window
    /// capture, OCR reads the panel AND the HUD. The pipeline used to turn the
    /// clock into 金币, the game ID into 银币, quest text into an inventory item
    /// and then classify the whole thing `Unknown`. These are the exact lines
    /// mhcli produced for samples/real_window_capture.png.
    #[test]
    fn full_window_lines_do_not_leak_hud_into_fields() {
        let lines: Vec<OcrLine> = vec![
            OcrLine { text: "01:56".into(), confidence: 0.96, rect: (8, 7, 34, 13) },
            OcrLine { text: "距离运镖完成".into(), confidence: 0.91, rect: (313, 107, 105, 12) },
            OcrLine { text: "人物属性".into(), confidence: 0.98, rect: (584, 120, 76, 15) },
            OcrLine { text: "咔咔咔89级".into(), confidence: 0.99, rect: (399, 167, 98, 14) },
            OcrLine { text: "气".into(), confidence: 0.69, rect: (641, 173, 13, 14) },
            OcrLine { text: "面".into(), confidence: 0.65, rect: (674, 173, 16, 15) },
            OcrLine { text: "6199/6199".into(), confidence: 1.0, rect: (791, 173, 72, 13) },
            OcrLine { text: "力".into(), confidence: 1.0, rect: (672, 282, 19, 18) },
            OcrLine { text: "1979/2280".into(), confidence: 0.99, rect: (759, 284, 69, 13) },
            OcrLine { text: "物理伤害773".into(), confidence: 0.99, rect: (650, 388, 105, 14) },
            OcrLine { text: "物理防御1520".into(), confidence: 0.99, rect: (794, 388, 112, 14) },
            OcrLine { text: "治疗强度408".into(), confidence: 1.0, rect: (794, 468, 102, 15) },
            OcrLine { text: "捉鬼任务".into(), confidence: 0.96, rect: (1143, 199, 69, 15) },
            OcrLine { text: "当前盈福经验0".into(), confidence: 0.99, rect: (638, 555, 114, 14) },
        ];
        let lines = merge_adjacent_fragments(&lines);

        // The clock must never become currency, and neither may the game ID.
        let currencies = extract_currencies(&lines);
        assert!(currencies.gold.is_none(), "01:56 must not become 金币");
        assert!(currencies.silver.is_none(), "the game ID must not become 银币");

        // The quest tracker is not an inventory item.
        assert!(
            extract_inventory(&lines).is_empty(),
            "a 人物属性 frame must not invent inventory items"
        );
        assert!(!looks_like_bag(&lines), "捉鬼任务 must not read as a 包裹 panel");

        // The panel is classified from its own labels, not from pixel layout.
        assert!(matches!(
            classify_from_lines(&lines),
            Some(ScreenKind::CharacterAttributes)
        ));

        let stats = extract_stats(&lines);
        assert_eq!(stats.hp.as_ref().map(|v| v.value), Some(6199), "气血");
        assert_eq!(stats.physical_damage.as_ref().map(|v| v.value), Some(773), "物理伤害");
        assert_eq!(stats.healing_power.as_ref().map(|v| v.value), Some(408), "治疗强度");
    }

    /// `ID` is read as `LD` on the real panel and its box overlaps the digits
    /// box by a few pixels; both must still pair up.
    #[test]
    fn game_id_survives_an_overlapping_ld_misread() {
        let lines = vec![
            OcrLine { text: "LD".into(), confidence: 0.75, rect: (241, 144, 26, 18) },
            OcrLine { text: "429277219".into(), confidence: 0.95, rect: (260, 144, 147, 18) },
        ];
        assert_eq!(
            extract_game_id(&lines).map(|(id, _)| id),
            Some("429277219".to_string())
        );
    }

    // ---- identity from features -------------------------------------------

    fn thumb_of(name: &str) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(panel_thumb(&sample(name)))
    }

    /// The fingerprint the pipeline actually stores: the *panel*, not the whole
    /// window. Using the full frame in a test makes two different panels look
    /// 95% alike, because the game world behind them is the same scene.
    fn panel_thumb_of(name: &str) -> String {
        use base64::Engine as _;
        let img = sample(name);
        let cropped = locate_panel(&img).and_then(|b| crop_rgba(&img, b.x, b.y, b.width, b.height));
        let work: &image::RgbaImage = cropped.as_ref().unwrap_or(&img);
        base64::engine::general_purpose::STANDARD.encode(panel_thumb(work))
    }

    const NOW_MS: i64 = 1_700_000_000_000;

    /// 旧接口形状的薄包装：测试里只关心"链到谁"，不关心证据结构。
    fn visual(
        current: &[PanelFingerprint],
        refs: &[IdentityRef],
        appearance: Option<&mhcore::AppearanceSignature>,
    ) -> Option<(String, f64, String)> {
        let assoc = associate_by_evidence(
            current,
            refs,
            appearance,
            None,
            NOW_MS,
            &MatchPolicy::default(),
        );
        assoc
            .character_id
            .map(|id| (id, assoc.confidence, evidence_summary(&assoc.evidence)))
    }

    fn bag_ref(id: &str, nick: &str, thumb: String, score: Option<i64>) -> IdentityRef {
        IdentityRef {
            character_id: id.to_string(),
            nickname: nick.to_string(),
            game_id: None,
            server: None,
            level: None,
            fingerprints: vec![PanelFingerprint {
                screen: ScreenKind::Inventory,
                thumb,
                score,
            }],
            appearance: None,
            avatar_thumb: None,
            last_seen_at: None,
        }
    }

    /// A 包裹 capture shows no 游戏ID and no nickname, so the picture itself has
    /// to link it back to a character. Same panel captured again must link;
    /// a different panel in the database must not win.
    #[test]
    fn visual_match_links_a_bag_capture_to_its_character() {
        let bag = thumb_of("real_inventory.png");
        let refs = vec![
            bag_ref("c1", "莫莫莫", bag.clone(), None),
            bag_ref("c2", "鱼鱼鱼", thumb_of("real_character_attributes.png"), None),
        ];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: bag,
            score: None,
        }];

        let (id, confidence, evidence) =
            visual(&current, &refs, None).expect("the same panel must link");
        assert_eq!(id, "c1");
        assert!(confidence >= 0.5, "confidence {confidence}");
        assert!(evidence.contains("包裹"), "evidence {evidence}");
    }

    /// 人物评分 是辅助特征：它随装备、等级变化，所以单独一致**不得**定案，
    /// 但足以把候选排到最前，让用户一眼看到"大概是这个角色"。
    /// 纯色缩略图：任何两张之间的相似度都是 0，用来把"面板证据"彻底排除。
    fn flat_thumb() -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(vec![128u8; 32 * 32])
    }

    #[test]
    fn score_alone_ranks_but_does_not_assign() {
        let refs = vec![
            bag_ref("c1", "莫莫莫", flat_thumb(), Some(20214)),
            bag_ref("c2", "鱼鱼鱼", flat_thumb(), Some(30111)),
        ];
        // 面板证据为 0，只剩评分这条线索。
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: flat_thumb(),
            score: Some(20214),
        }];

        let assoc = associate_by_evidence(
            &current, &refs, None, None, NOW_MS, &MatchPolicy::default(),
        );
        assert!(assoc.character_id.is_none(), "单靠评分不得自动归属");
        assert_eq!(assoc.candidates[0].character_id, "c1");
        assert!(
            assoc.candidates[0]
                .evidence
                .iter()
                .any(|e| e.kind == EvidenceKind::CharacterScore),
            "候选里要留下评分证据"
        );
    }

    /// 面板 + 评分互相印证时就可以定案，而且两项证据都要被记下来。
    #[test]
    fn panel_plus_score_assigns_with_per_signal_evidence() {
        let bag = thumb_of("real_inventory.png");
        let refs = vec![
            bag_ref("c1", "莫莫莫", bag.clone(), Some(20214)),
            bag_ref("c2", "鱼鱼鱼", thumb_of("real_character_attributes.png"), Some(30111)),
        ];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: bag,
            score: Some(20214),
        }];

        let assoc = associate_by_evidence(
            &current, &refs, None, None, NOW_MS, &MatchPolicy::default(),
        );
        assert_eq!(assoc.character_id.as_deref(), Some("c1"));
        let kinds: Vec<EvidenceKind> = assoc.evidence.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EvidenceKind::PanelThumb), "kinds {kinds:?}");
        assert!(kinds.contains(&EvidenceKind::CharacterScore), "kinds {kinds:?}");
    }

    /// Two characters whose panels look alike must not be guessed at.
    #[test]
    fn ambiguous_panels_are_left_unresolved() {
        let bag = thumb_of("real_inventory.png");
        // Both refs point at the same picture, so neither wins by a margin.
        let refs = vec![
            bag_ref("c1", "莫莫莫", bag.clone(), None),
            bag_ref("c2", "鱼鱼鱼", bag.clone(), None),
        ];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: bag,
            score: None,
        }];
        assert!(visual(&current, &refs, None).is_none());
    }

    // ---- identity from 发色 / 武器 -----------------------------------------

    fn look(hair: [u8; 3], weapon: Option<[u8; 3]>) -> mhcore::AppearanceSignature {
        mhcore::AppearanceSignature {
            hair_name: nearest_name(&HAIR_PALETTE, hair),
            hair_rgb: hair,
            weapon_name: weapon.map(|w| nearest_name(&ACCENT_PALETTE, w)),
            weapon_rgb: weapon,
        }
    }

    fn look_ref(id: &str, nick: &str, thumb: String, appearance: Option<mhcore::AppearanceSignature>) -> IdentityRef {
        IdentityRef {
            character_id: id.to_string(),
            nickname: nick.to_string(),
            game_id: None,
            server: None,
            level: None,
            fingerprints: vec![PanelFingerprint {
                screen: ScreenKind::Inventory,
                thumb,
                score: None,
            }],
            appearance,
            avatar_thumb: None,
            last_seen_at: None,
        }
    }

    /// The real measurements: 莫莫莫's 人物属性 portrait reads (84,82,94) and the
    /// mounted model in the 包裹 reads (66,69,71); the cyan weapon glow reads
    /// (87,197,211) and (76,196,183). Items changed, panel kind changed, and
    /// the character is still recognisable.
    #[test]
    fn appearance_links_a_capture_whose_items_changed() {
        let refs = vec![look_ref(
            "c1",
            "莫莫莫",
            panel_thumb_of("real_character_attributes.png"),
            Some(look([84, 82, 94], Some([87, 197, 211]))),
        )];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: panel_thumb_of("real_inventory.png"),
            score: None,
        }];
        let now = look([66, 69, 71], Some([76, 196, 183]));

        let (id, confidence, evidence) = visual(&current, &refs, Some(&now))
            .expect("发色 + 武器 must link the capture");
        assert_eq!(id, "c1");
        assert!(confidence > 0.15, "confidence {confidence}");
        assert!(evidence.contains("发色"), "evidence {evidence}");
    }

    /// A second character with a similar hair colour must not steal the match:
    /// its weapon glow is a different colour, and that is what rules it out.
    #[test]
    fn a_lookalike_character_is_ruled_out_by_its_weapon() {
        let refs = vec![
            look_ref(
                "c1",
                "莫莫莫",
                panel_thumb_of("real_character_attributes.png"),
                Some(look([84, 82, 94], Some([87, 197, 211]))),
            ),
            // Measured off the other real character capture (brown hair, pale
            // blade). Hair is only 20 units away; the weapon is 94 away.
            look_ref(
                "c2",
                "清风徐来",
                panel_thumb_of("real_window_capture.png"),
                Some(look([71, 51, 63], Some([150, 217, 237]))),
            ),
        ];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: panel_thumb_of("real_inventory.png"),
            score: None,
        }];
        let now = look([66, 69, 71], Some([76, 196, 183]));

        let (id, _, _) = visual(&current, &refs, Some(&now)).expect("c1 still wins");
        assert_eq!(id, "c1");
    }

    /// Different hair is not a match, however close the panel geometry is.
    #[test]
    fn different_hair_is_not_a_match() {
        let refs = vec![look_ref(
            "c1",
            "莫莫莫",
            panel_thumb_of("real_character_attributes.png"),
            Some(look([150, 52, 48], None)),
        )];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: panel_thumb_of("real_inventory.png"),
            score: None,
        }];
        let now = look([66, 69, 71], Some([76, 196, 183]));
        assert!(visual(&current, &refs, Some(&now)).is_none());
    }

    /// Two characters sharing a look must not be guessed between.
    #[test]
    fn identical_looks_stay_unresolved() {
        let same = Some(look([84, 82, 94], Some([87, 197, 211])));
        let refs = vec![
            look_ref("c1", "莫莫莫", panel_thumb_of("real_character_attributes.png"), same.clone()),
            look_ref("c2", "鱼鱼鱼", panel_thumb_of("real_window_capture.png"), same),
        ];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: panel_thumb_of("real_inventory.png"),
            score: None,
        }];
        let now = look([66, 69, 71], Some([76, 196, 183]));
        assert!(visual(&current, &refs, Some(&now)).is_none());
    }

    /// A character whose look was never recorded could be the real owner, so
    /// the appearance path must stay out of the way instead of guessing.
    #[test]
    fn unrecorded_look_blocks_the_guess() {
        let refs = vec![
            look_ref(
                "c1",
                "莫莫莫",
                panel_thumb_of("real_character_attributes.png"),
                Some(look([84, 82, 94], Some([87, 197, 211]))),
            ),
            look_ref("c2", "鱼鱼鱼", panel_thumb_of("real_window_capture.png"), None),
        ];
        let current = vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: panel_thumb_of("real_inventory.png"),
            score: None,
        }];
        let now = look([66, 69, 71], Some([76, 196, 183]));
        assert!(visual(&current, &refs, Some(&now)).is_none());
    }

    /// "人物评分" is read as 入物评分 / 人份评分 depending on the crop, and the
    /// score is the identity key for a bag — the anchor has to survive that.
    #[test]
    fn character_score_survives_a_misread_label() {
        for label in ["人物评分20214", "入物评分20214", "人物评分 20,214"] {
            let lines = vec![OcrLine {
                text: label.into(),
                confidence: 0.9,
                rect: (141, 417, 118, 14),
            }];
            assert_eq!(extract_character_score(&lines), Some(20214), "label {label}");
        }

        // A digits-only box to the right of a bare label, too.
        let lines = vec![
            OcrLine { text: "人物评分".into(), confidence: 0.9, rect: (400, 519, 60, 14) },
            OcrLine { text: "20214".into(), confidence: 0.9, rect: (470, 519, 50, 14) },
        ];
        assert_eq!(extract_character_score(&lines), Some(20214));
    }

    /// Widely-spaced glyph fragments must merge into one label.
    #[test]    fn wide_letter_spacing_fragments_merge() {
        let lines = vec![
            OcrLine { text: "气".into(), confidence: 1.0, rect: (640, 173, 14, 14) },
            OcrLine { text: "血".into(), confidence: 1.0, rect: (674, 173, 16, 15) },
        ];
        let merged = merge_adjacent_fragments(&lines);
        assert_eq!(merged.len(), 1, "气+血 must become one token");
        assert_eq!(merged[0].text, "气血");
    }
}

