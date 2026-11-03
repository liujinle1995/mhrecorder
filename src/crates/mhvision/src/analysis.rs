//! 每张截图的"分析结果"缓存：OCR 行 + 轻量指纹。
//!
//! 缓存键是**图片字节的 SHA-256**，不是文件名、也不是上传顺序：同一次采集
//! 存了两次、或者隔了几天把同一张图再导入一次，命中的是同一条缓存。
//!
//! 边界划在 OCR 上。一次识别里 OCR 占绝大部分时间（单张 1–2 秒），而它只
//! 取决于像素，对同一份字节必然给出同一份结果；后面的合并相邻片段、字段提取、
//! 身份关联、审核框定位在每次请求里**重跑**，所以改了提取规则不需要让老缓存
//! 失效。指纹（dHash + HSV 直方图）顺手一起存：它是"这张图长什么样"的紧凑
//! 描述，命中时用来复核这条缓存确实属于这张图。
//!
//! ORB 描述子**不进缓存**：一张图 10–40 KB，而比值检验每次比对都要重算，
//! 存下来没有消费者。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::features::{compute_dhash, compute_hsv_hist, HUE_BINS, SAT_BINS, VAL_BINS};
use crate::imgops::{rgba_to_bgr_mat, to_gray};
use crate::prelude::*;
use mhcore::ANALYSIS_VERSION;

/// 缓存键的内容部分：图片字节的 SHA-256（小写十六进制）。
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 一张截图的分析结果：识别出来的原始文字行 + 描述这张图长什么样的指纹。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAnalysis {
    /// 算出这份结果的算法版本，读回来时用来判断还能不能用。
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// 64-bit dHash。
    pub dhash: u64,
    /// HSV 直方图（32 色相 + 8 饱和度 + 8 亮度，各段独立归一化）。
    pub hsv_hist: Vec<f32>,
    /// OCR 原始行，坐标相对面板裁切后的图。**未**做相邻片段合并。
    pub lines: Vec<OcrLine>,
}

impl ImageAnalysis {
    /// 对一张图（面板裁切后的图）算出可缓存的分析结果。
    pub fn compute(img: &image::RgbaImage, lines: Vec<OcrLine>) -> Result<Self> {
        let mat = rgba_to_bgr_mat(img)?;
        let gray = to_gray(&mat)?;
        Ok(Self {
            version: ANALYSIS_VERSION,
            width: img.width(),
            height: img.height(),
            dhash: compute_dhash(&gray)?,
            hsv_hist: compute_hsv_hist(&mat)?,
            lines,
        })
    }

    /// 这条缓存真的是这张图算出来的吗？
    ///
    /// 内容哈希是强校验，正常不会撞；但缓存表是人可读可改的 SQLite 文件，
    /// 而"用了别人的 OCR 行"比"没命中"糟得多。dHash 只要一次 9×8 缩放，
    /// 复核成本可以忽略。
    pub fn matches_image(&self, img: &image::RgbaImage) -> bool {
        if self.width != img.width() || self.height != img.height() {
            return false;
        }
        let Ok(mat) = rgba_to_bgr_mat(img) else {
            return false;
        };
        let Ok(gray) = to_gray(&mat) else {
            return false;
        };
        matches!(compute_dhash(&gray), Ok(hash) if hash == self.dhash)
    }

    /// 序列化成可以进 `image_features.payload` 的字符串。
    pub fn encode(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| VisionError::Ocr(format!("analysis cache encode failed: {e}")))
    }

    /// 反序列化。版本或直方图长度对不上就当作没命中（返回 `None`），
    /// 让调用方重算一遍并覆盖老条目。
    pub fn decode(payload: &str) -> Option<Self> {
        let analysis: Self = serde_json::from_str(payload).ok()?;
        if analysis.version != ANALYSIS_VERSION {
            return None;
        }
        if analysis.hsv_hist.len() != HUE_BINS + SAT_BINS + VAL_BINS {
            return None;
        }
        Some(analysis)
    }
}
