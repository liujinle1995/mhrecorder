//! 感知特征：dHash、ORB（双向 + RANSAC）、HSV 直方图。
//!
//! 两张截图"像不像"的全部量化与阈值都在这里。

use crate::prelude::*;
use crate::imgops::{gray_mat_to_vec, rgba_to_bgr_mat, to_gray};
/// Perceptual fingerprint of a screenshot, cached so repeated uploads of the
/// same image skip the expensive path.
///
/// Naming note: the 64-bit hash is a **dHash** (difference hash), not the DCT
/// pHash its predecessor was called. The algorithm was always "resize to 9x8,
/// compare each pixel with its right neighbour" — that is dHash. Keeping the
/// wrong name invited "just tweak the pHash threshold" reasoning about a number
/// that is not a pHash at all.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageFeatures {
    /// 64-bit difference hash.
    pub dhash: u64,
    /// ORB descriptors flattened row-major (32 bytes each).
    pub orb: Vec<u8>,
    pub orb_keypoints: Vec<(f32, f32)>,
    /// HSV histogram: 32 hue bins (saturation- and value-weighted) followed by
    /// 8 saturation bins and 8 value bins, each block normalised to sum to 1.
    pub hsv_hist: Vec<f32>,
    pub width: u32,
    pub height: u32,
}

/// Bin layout of [`ImageFeatures::hsv_hist`].
pub const HUE_BINS: usize = 32;
pub const SAT_BINS: usize = 8;
pub const VAL_BINS: usize = 8;

/// Result of fingerprinting: how similar two screenshots are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    /// 1.0 when the 64-bit dHash values are identical; 0.0 when fully different.
    pub dhash: f64,
    /// Geometrically verified ORB agreement in 0.0..=1.0.
    pub orb: f64,
    /// Cosine similarity of the HSV histograms in 0.0..=1.0.
    pub hsv: f64,
    /// How many keypoint pairs survived RANSAC. Useful when explaining a score:
    /// 0.9 from 3 inliers is luck, 0.9 from 60 inliers is the same picture.
    pub orb_inliers: usize,
}

impl Similarity {
    /// Weighted blend used to decide whether two screenshots show the same
    /// character panel. Weights favour the robust structural signals.
    pub fn combined(&self) -> f64 {
        0.5 * self.dhash + 0.3 * self.orb + 0.2 * self.hsv
    }
}

// ---------------------------------------------------------------------------
// Perceptual features
// ---------------------------------------------------------------------------

/// Compute the perceptual fingerprint of an image.
pub fn extract_features(img: &image::RgbaImage) -> Result<ImageFeatures> {
    let mat = rgba_to_bgr_mat(img)?;
    let gray = to_gray(&mat)?;

    let dhash = compute_dhash(&gray)?;
    let (orb, orb_keypoints) = compute_orb(&gray)?;
    let hsv_hist = compute_hsv_hist(&mat)?;

    Ok(ImageFeatures {
        dhash,
        orb,
        orb_keypoints,
        hsv_hist,
        width: img.width(),
        height: img.height(),
    })
}

/// Difference hash: resize to 9x8, compare each pixel with its right neighbour.
///
/// This is **dHash**, not the DCT-based pHash: there is no cosine transform
/// anywhere in it. It was misnamed for a long time, which made every discussion
/// about "tightening the pHash threshold" reason about the wrong number.
pub(crate) fn compute_dhash(gray: &opencv::core::Mat) -> Result<u64> {
    let mut small = opencv::core::Mat::default();
    opencv::imgproc::resize_def(
        gray,
        &mut small,
        opencv::core::Size::new(9, 8),
    )
    .map_err(|e| VisionError::OpenCv(e.to_string()))?;

    let (px, rows, cols) = gray_mat_to_vec(&small)?;
    if rows != 8 || cols != 9 {
        return Err(VisionError::OpenCv(format!("dhash resize gave {cols}x{rows}")));
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
pub(crate) fn compute_orb(gray: &opencv::core::Mat) -> Result<(Vec<u8>, Vec<(f32, f32)>)> {
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

/// HSV histogram that actually uses S and V, not just hue.
///
/// The previous version binned hue alone, so a dark navy robe and a bright blue
/// banner landed in the same bucket with the same weight — the only thing that
/// told them apart was brightness, which was thrown away. Now:
/// * hue is counted **weighted by saturation × value**, so grey and black
///   pixels (where hue is meaningless) barely vote;
/// * saturation and value get their own 8-bin blocks, so "same hue, different
///   vividness" is still a measurable difference.
pub(crate) fn compute_hsv_hist(bgr: &opencv::core::Mat) -> Result<Vec<f32>> {
    let mut hsv = opencv::core::Mat::default();
    opencv::imgproc::cvt_color_def(bgr, &mut hsv, opencv::imgproc::COLOR_BGR2HSV)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;

    let mut hist = vec![0f32; HUE_BINS + SAT_BINS + VAL_BINS];
    let data = hsv.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
    if data.len() < 3 {
        return Ok(hist);
    }
    for chunk in data.chunks_exact(3) {
        // OpenCV 8-bit HSV: H in 0..180, S and V in 0..255.
        let (h, s, v) = (chunk[0] as f32, chunk[1] as f32 / 255.0, chunk[2] as f32 / 255.0);
        let hue_bin = ((chunk[0] as usize) / 6).min(HUE_BINS - 1);
        hist[hue_bin] += s * v;
        let sat_bin = ((chunk[1] as usize) * SAT_BINS / 256).min(SAT_BINS - 1);
        hist[HUE_BINS + sat_bin] += 1.0;
        let val_bin = ((chunk[2] as usize) * VAL_BINS / 256).min(VAL_BINS - 1);
        hist[HUE_BINS + SAT_BINS + val_bin] += 1.0;
        let _ = h;
    }

    // 三块各自归一化：否则像素数多的块会压过另外两块。
    for (start, len) in [(0, HUE_BINS), (HUE_BINS, SAT_BINS), (HUE_BINS + SAT_BINS, VAL_BINS)] {
        let sum: f32 = hist[start..start + len].iter().sum();
        if sum > 0.0 {
            for v in &mut hist[start..start + len] {
                *v /= sum;
            }
        }
    }
    Ok(hist)
}

/// Cosine similarity between two equal-length slices, clamped to 0..=1.
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|v| v * v).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        ((dot / (na * nb)) as f64).clamp(0.0, 1.0)
    }
}

/// Compare two fingerprints.
pub fn compare(a: &ImageFeatures, b: &ImageFeatures) -> Similarity {
    // dHash: 1.0 for identical, 0.0 for 64 differing bits.
    let hamming = (a.dhash ^ b.dhash).count_ones() as f64;
    let dhash = (1.0 - hamming / 64.0).clamp(0.0, 1.0);

    // HSV: hue 0.6, saturation 0.2, value 0.2. Hue is still the most telling,
    // but it no longer decides alone.
    let hsv = {
        let hue = cosine(&a.hsv_hist[..HUE_BINS], &b.hsv_hist[..HUE_BINS]);
        let sat = cosine(
            &a.hsv_hist[HUE_BINS..HUE_BINS + SAT_BINS],
            &b.hsv_hist[HUE_BINS..HUE_BINS + SAT_BINS],
        );
        let val = cosine(&a.hsv_hist[HUE_BINS + SAT_BINS..], &b.hsv_hist[HUE_BINS + SAT_BINS..]);
        (0.6 * hue + 0.2 * sat + 0.2 * val).clamp(0.0, 1.0)
    };

    let (orb, orb_inliers) = orb_similarity(a, b);

    Similarity { dhash, orb, hsv, orb_inliers }
}

// ---------------------------------------------------------------------------
// ORB matching
// ---------------------------------------------------------------------------

/// ORB descriptor length in bytes (256 bits).
pub(crate) const DESC_LEN: usize = 32;

/// Lowe's ratio test threshold, applied in both directions.
pub(crate) const RATIO: f32 = 0.8;

/// A pair of descriptors is only considered if the distance is below this many
/// bits. Without an absolute cutoff a "best match" among 10 000 unrelated
/// descriptors can still be poor.
pub(crate) const MAX_DESC_DIST: u32 = 72;

/// 双向比值匹配 + RANSAC 几何验证。
///
/// 只有描述子最近邻一致**不够**：相似纹理（同一套 UI 边框、同一个图标网格）
/// 会让两张不同的截图也产生一堆"最近邻"，分数虚高。两处修正：
///
/// 1. **双向** —— i 认 j 为最近，j 也要认 i 为最近，单向的偶然匹配被丢掉；
/// 2. **几何** —— 用 RANSAC 拟合一个相似变换（缩放 + 旋转 + 平移），只统计
///    落在模型上的点。对不上的匹配（纹理巧合）不贡献分数。
///
/// 分数同时要求"比例高"和"数量够"：3 个 inlier 全对也只是碰巧。
pub(crate) fn orb_similarity(a: &ImageFeatures, b: &ImageFeatures) -> (f64, usize) {
    let an = a.orb.len() / DESC_LEN;
    let bn = b.orb.len() / DESC_LEN;
    if an == 0 || bn == 0 {
        return (0.0, 0);
    }
    if a.orb == b.orb {
        return (1.0, an);
    }

    // 距离矩阵只算一次，正反两个方向的最近邻都从它来。
    let mut dist = vec![0u32; an * bn];
    for i in 0..an {
        let da = &a.orb[i * DESC_LEN..(i + 1) * DESC_LEN];
        for j in 0..bn {
            let db = &b.orb[j * DESC_LEN..(j + 1) * DESC_LEN];
            let mut d = 0u32;
            for k in 0..DESC_LEN {
                d += (da[k] ^ db[k]).count_ones();
            }
            dist[i * bn + j] = d;
        }
    }

    // 正向：每个 a 的最近邻与次近邻。
    let mut fwd_best = vec![usize::MAX; an];
    let mut fwd_ok = vec![false; an];
    for i in 0..an {
        let row = &dist[i * bn..(i + 1) * bn];
        let (mut best, mut second) = (u32::MAX, u32::MAX);
        let (mut best_j, mut best_second_j) = (usize::MAX, usize::MAX);
        for (j, &d) in row.iter().enumerate() {
            if d < best {
                second = best;
                best_second_j = best_j;
                best = d;
                best_j = j;
            } else if d < second {
                second = d;
                best_second_j = j;
            }
        }
        let _ = best_second_j;
        if best_j == usize::MAX {
            continue;
        }
        let ratio_ok = (best as f32) / (second.max(1) as f32) < RATIO;
        if ratio_ok && best <= MAX_DESC_DIST {
            fwd_best[i] = best_j;
            fwd_ok[i] = true;
        }
    }

    // 反向：每个 b 的最近邻（为了互检，只需要 argmin）。
    let mut bwd_best = vec![usize::MAX; bn];
    for j in 0..bn {
        let (mut best, mut best_i) = (u32::MAX, usize::MAX);
        for i in 0..an {
            let d = dist[i * bn + j];
            if d < best {
                best = d;
                best_i = i;
            }
        }
        bwd_best[j] = best_i;
    }

    let matches: Vec<(usize, usize)> = (0..an)
        .filter(|&i| fwd_ok[i] && bwd_best[fwd_best[i]] == i)
        .map(|i| (i, fwd_best[i]))
        .collect();

    if matches.is_empty() {
        return (0.0, 0);
    }
    if matches.len() < 2 || a.orb_keypoints.len() < an || b.orb_keypoints.len() < bn {
        // 一个点拟合不出变换，只能按比例给个很小的分。
        return ((matches.len() as f64 / an as f64) * 0.2, 0);
    }

    let tol = 0.02 * a.width.max(a.height).max(1) as f32;
    let inliers = ransac_inliers(&matches, &a.orb_keypoints, &b.orb_keypoints, tol);

    let coverage = inliers as f64 / an.min(bn) as f64;
    // 15 个 inlier 才算"证据充分"；少于这个数再高的比例也可能只是同一块 UI 边框。
    let support = (inliers as f64 / 15.0).min(1.0);
    ((coverage * support).clamp(0.0, 1.0), inliers)
}

/// 4-DOF 相似变换：`q = s·R·p + t`。
#[derive(Clone, Copy)]
pub(crate) struct SimTransform {
    s_cos: f32,
    s_sin: f32,
    tx: f32,
    ty: f32,
}

impl SimTransform {
    /// 由两对点解出相似变换；退化（两点重合）时返回 None。
    fn from_pair(p1: (f32, f32), q1: (f32, f32), p2: (f32, f32), q2: (f32, f32)) -> Option<Self> {
        let (dxp, dyp) = (p2.0 - p1.0, p2.1 - p1.1);
        let (dxq, dyq) = (q2.0 - q1.0, q2.1 - q1.1);
        let np = (dxp * dxp + dyp * dyp).sqrt();
        let nq = (dxq * dxq + dyq * dyq).sqrt();
        if np < 1e-3 || nq < 1e-3 {
            return None;
        }
        let scale = nq / np;
        // 复平面除法 (dxq + i·dyq) / (dxp + i·dyp) 同时给出缩放与旋转。
        let s_cos = scale * (dxq * dxp + dyq * dyp) / (np * nq);
        let s_sin = scale * (dyq * dxp - dxq * dyp) / (np * nq);
        let tx = q1.0 - (s_cos * p1.0 - s_sin * p1.1);
        let ty = q1.1 - (s_sin * p1.0 + s_cos * p1.1);
        Some(Self { s_cos, s_sin, tx, ty })
    }

    fn apply(&self, p: (f32, f32)) -> (f32, f32) {
        (
            self.s_cos * p.0 - self.s_sin * p.1 + self.tx,
            self.s_sin * p.0 + self.s_cos * p.1 + self.ty,
        )
    }
}

/// 确定性伪随机：RANSAC 需要抽样，但测试和线上行为必须可复现，
/// 所以不用系统随机源。
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// 返回落在最优模型上的匹配数。
pub(crate) fn ransac_inliers(
    matches: &[(usize, usize)],
    pa: &[(f32, f32)],
    pb: &[(f32, f32)],
    tol: f32,
) -> usize {
    if matches.len() < 2 {
        return 0;
    }
    let mut rng = Lcg::new(0x5EED_1234);
    let iterations = (matches.len() * 12).clamp(60, 400);
    let mut best = 0usize;
    let total = matches.len();

    for _ in 0..iterations {
        let i = rng.below(total);
        let mut j = rng.below(total);
        if j == i {
            j = (j + 1) % total;
        }
        let (ai, bi) = matches[i];
        let (aj, bj) = matches[j];
        let Some(model) =
            SimTransform::from_pair(pa[ai], pb[bi], pa[aj], pb[bj])
        else {
            continue;
        };
        let mut count = 0usize;
        for &(mi, mj) in matches {
            let q = model.apply(pa[mi]);
            let (dx, dy) = (q.0 - pb[mj].0, q.1 - pb[mj].1);
            if dx * dx + dy * dy <= tol * tol {
                count += 1;
            }
        }
        if count > best {
            best = count;
            // 绝大多数匹配都落在模型上就不必再抽了。
            if best as f32 >= 0.95 * total as f32 {
                break;
            }
        }
    }
    best
}
