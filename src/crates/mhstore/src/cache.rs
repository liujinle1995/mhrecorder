//! 识别特征缓存。
//!
//! 键里带算法版本：换了特征算法之后，老条目算出来的分数和新算法不可比，
//! 但它们**看起来**仍是合法的 JSON。用前缀把它们自然失效，比加一列
//! `algo_version` 再到处过滤要省事，也不会让老库背着无用的行。

use rusqlite::{params, OptionalExtension};

use crate::{now_ms, Result, Store, StoreError};

/// 特征算法版本。**改了 `mhvision` 的特征/相似度算法就要 +1。**
/// 2 = dHash + ORB(互检 + RANSAC) + HSV(H/S/V) 三维直方图。
pub const FEATURE_ALGO_VERSION: u32 = 2;

/// 由图片内容哈希生成缓存键。
pub fn feature_key(content_hash: &str) -> String {
    format!("v{FEATURE_ALGO_VERSION}:{content_hash}")
}

impl Store {
    /// Look up cached perceptual features by content hash.
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
             ON CONFLICT(cache_key) DO UPDATE SET payload = excluded.payload",
            params![cache_key, payload, now_ms()],
        )?;
        Ok(())
    }

    /// 缓存条目数（导出时报告用）。
    pub fn feature_cache_len(&self) -> Result<i64> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        Ok(conn.query_row("SELECT COUNT(*) FROM image_features", [], |r| r.get(0))?)
    }
}
