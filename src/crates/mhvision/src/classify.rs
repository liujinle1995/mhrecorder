//! 界面分类与面板定位。
//!
//! 先判断截图是哪个面板，再把整窗截图裁成面板本身；OCR 与字段提取都假设
//! 输入已经是面板。

use crate::prelude::*;
use crate::imgops::{rgba_to_bgr_mat, to_gray};
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
pub(crate) fn classify_from_lines(lines: &[OcrLine]) -> Option<ScreenKind> {
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
pub(crate) fn otsu_threshold(hist: &[u32; 256], total: u32) -> u8 {
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
pub(crate) fn morph(src: &[bool], w: u32, h: u32, dilate: bool) -> Vec<bool> {
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

/// Detect the long horizontal HP/MP gradient bars that characterise the
/// attribute panel: rows where most pixels are strongly saturated red or blue.
pub(crate) fn detect_wide_bars(bgr: &opencv::core::Mat) -> Result<bool> {    let mut hsv = opencv::core::Mat::default();
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
pub(crate) fn count_vertical_edges(bgr: &opencv::core::Mat) -> Result<u32> {
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
