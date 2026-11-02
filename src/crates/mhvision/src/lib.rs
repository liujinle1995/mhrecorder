//! 图像流水线：截图分类、OCR、感知特征与角色关联。
//!
//! 归属：这个 crate 直接调用 OpenCV 与 ONNX Runtime。它必须能在纯命令行环境下
//! 使用（不依赖 WinUI、不依赖命名管道），这样识别器可以脱离界面单独跑。
//!
//! ## 模块划分
//!
//! | 模块 | 职责 | 改动频率 |
//! |---|---|---|
//! | [`types`] | 错误类型、任务控制口、OCR 行 | 几乎不动 |
//! | [`imgops`] | 解码、BGR Mat 转换 | 几乎不动 |
//! | [`features`] | dHash / ORB（双向 + RANSAC）/ HSV 直方图 | 换特征算法时 |
//! | [`classify`] | 界面分类、面板定位 | 换游戏界面时 |
//! | [`thumbnail`] | 32×32 面板缩略图相似度 | 少 |
//! | [`identity`] | 多证据加权、冲突保护、昵称歧义 | 调关联策略时 |
//! | [`appearance`] | 发色 / 武器特征 | 少 |
//! | [`ocr`] | PP-OCRv4 检测 + 识别 | 换引擎时 |
//! | [`extract`] | 从 OCR 行里抠字段 | 改启发式时 |
//! | [`regions`] | 字段值对应回截图上的像素框 | 少 |
//! | [`pipeline`] | 把上面这些串成一次调用 | 少 |
//!
//! 拆开的理由很直接：这几种东西的变化频率完全不同。改字段提取时，不该有
//! 半个文件是 OpenCV 调用；换特征算法时，也不该担心碰到 OCR 的会话管理。
//!
//! 公开接口通过下面的 `pub use` 保持稳定：`mhservice` 与 `mhcli` 只依赖这些
//! 名字，内部怎么分文件它们不关心。

mod appearance;
mod classify;
mod extract;
mod features;
mod identity;
mod imgops;
mod ocr;
mod pipeline;
mod regions;
mod thumbnail;
mod types;

#[cfg(test)]
mod tests;

/// 各模块共用的导入。
///
/// 每个模块只写一行 `use crate::prelude::*;`，而不是各自重复十来个 `use`。
/// 这里只放"到处都要用"的东西：外部依赖的 prelude、领域模型、本 crate 的
/// 公共类型。模块之间真正的依赖仍然显式写出来，读代码时看得见。
pub(crate) mod prelude {
    pub use crate::types::{JobControl, NoControl, OcrLine, Result, VisionError};
    pub use mhcore::{
        AttributeValue, Character, CombatStats, Currencies, EquipmentItem,
        EvidenceKind, FieldRegion, IdentityCandidate, IdentityRef, IdentitySource, InventoryItem, MatchEvidence,
        PanelFingerprint, RecognitionResult, ScreenKind,
    };
    pub use opencv::prelude::*;
    pub use std::path::Path;
    pub use std::sync::OnceLock;
}

pub use appearance::{appearance_for_capture, appearance_signature};
pub use classify::{classify_screen, locate_panel, PanelBox};
pub use features::{
    compare, extract_features, ImageFeatures, Similarity, HUE_BINS, SAT_BINS, VAL_BINS,
};
pub use identity::{associate_by_evidence, MatchPolicy, VisualAssociation};
pub use imgops::decode_image;
pub use ocr::OcrEngine;
pub use pipeline::RecognitionPipeline;
pub use thumbnail::{panel_thumb, thumb_similarity, THUMB_SIDE};
pub use types::{JobControl, NoControl, OcrLine, Result, VisionError};

/// 当前 crate 版本。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
