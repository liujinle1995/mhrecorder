//! 每张图的识别分析缓存（OCR 行 + 轻量指纹）。
//!
//! 键里带算法版本：换了特征算法或 OCR 模型之后，老条目**看起来**仍是合法的
//! JSON，但内容是旧算法算出来的。用版本前缀把它们自然失效，比加一列
//! `algo_version` 再到处过滤要省事，也不会让老库背着无用的行。
//!
//! 版本常量在 `mhvision::ANALYSIS_VERSION`——"什么时候算变了"只有算法自己
//! 知道，存储层不该替它判断。

use rusqlite::{params, OptionalExtension};

use crate::{now_ms, Result, Store, StoreError};

/// 缓存条目上限。超了按写入时间删最旧的。
///
/// 一条约 1–4 KB（OCR 行 + 直方图），2000 条最多几 MB。识别是"同一批图偶尔
/// 重导一次"，不需要保留无限历史。
pub const MAX_IMAGE_FEATURES: i64 = 2_000;

/// 由算法版本 + 内容哈希生成缓存键，例如 `v3:4f1c…`。
pub fn feature_key(algo: u32, content_hash: &str) -> String {
    format!("v{algo}:{content_hash}")
}

impl Store {
    /// Look up cached perceptual features by cache key.
    pub fn get_image_feature(&self, cache_key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT payload FROM image_features WHERE cache_key = ?1",
            params![cache_key],
            |r| r.get(0),
        )
        .optional()
        .map_err(StoreError::from)
    }

    /// Store perceptual features so repeated uploads skip the expensive path.
    pub fn put_image_feature(&mut self, cache_key: &str, payload: &str) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO image_features(cache_key, payload, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(cache_key) DO UPDATE SET payload = excluded.payload, created_at = excluded.created_at",
            params![cache_key, payload, now_ms()],
        )?;
        Ok(())
    }

    /// 缓存条目数（导出时报告用）。
    pub fn feature_cache_len(&self) -> Result<i64> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        Ok(conn.query_row("SELECT COUNT(*) FROM image_features", [], |r| r.get(0))?)
    }

    /// 只保留最近写入的 `keep` 条，返回删掉的行数。
    pub fn prune_image_features(&mut self, keep: i64) -> Result<usize> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let deleted = conn.execute(
            "DELETE FROM image_features WHERE cache_key NOT IN (
                 SELECT cache_key FROM image_features
                 ORDER BY created_at DESC, cache_key DESC LIMIT ?1
             )",
            params![keep.max(0)],
        )?;
        Ok(deleted)
    }
}
