//! 面板缩略图指纹。
//!
//! 32×32 灰度 + 去均值余弦。比 ORB 粗糙，但对"同一块面板、换了几个图标"
//! 这种变化更宽容，而且便宜。

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
