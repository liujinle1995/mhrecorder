//! 导出与备份。
//!
//! 两种口径，语义不同，不能混用：
//!
//! * [`Store::export_document`] —— **普通数据导出**：角色、属性、货币、装备、
//!   背包，外加身份关联赖以工作的指纹/外观/物品字典。人可以读，也可以拿去做
//!   分析；体积小。
//! * [`Store::export_backup`] —— **完整备份**：上面那份 JSON + 整个 SQLite 文件
//!   （`VACUUM INTO` 出来的一致快照）+ 清单。换电脑、重装之后靠它能完整恢复，
//!   包括重新训练/校准识别所需的每一条证据。
//!
//! 之前只导角色/快照/设置，指纹和外观全丢了——换台机器就等于把积累的识别能力
//! 清零，而这恰恰是最难重建的部分。

use std::io::Write;
use std::path::Path;

use rusqlite::params;

use crate::{now_ms, Result, Store, StoreError};

/// 备份结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub path: String,
    pub bytes: u64,
    /// 打包进去的条目名。
    pub entries: Vec<String>,
    /// 是否带上了数据库文件。
    pub includes_database: bool,
}

impl Store {
    /// Dump everything worth keeping as one JSON document.
    pub fn export_document(&self) -> Result<serde_json::Value> {
        let characters = self.list_characters()?;
        let mut snapshot_docs = Vec::new();
        for d in &characters {
            for s in self.list_snapshots(&d.character.id, 10_000)? {
                snapshot_docs.push(serde_json::to_value(s).unwrap_or(serde_json::Value::Null));
            }
        }
        let settings = self.load_settings()?;
        let (fingerprints, appearances) = self.export_identity_rows()?;
        let items = self.list_items(100_000)?;
        let cache_entries = self.export_feature_cache_index()?;

        Ok(serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            // 算法版本：把这份 JSON 拿到另一个（可能更新的）版本里做对比时，
            // 先看它——特征分数跨算法不可比。
            "algorithmVersion": mhcore::ANALYSIS_VERSION,
            "schemaVersion": self.schema_version().unwrap_or(0),
            "exportedAt": now_ms(),
            "characters": characters.iter().map(|d| serde_json::json!({
                "id": d.character.id,
                "nickname": d.character.nickname,
                "gameId": d.character.game_id,
                "level": d.character.level,
                "school": d.character.school,
                "server": d.character.server,
                "identityConfidence": d.character.identity_confidence,
                "identityConfirmed": d.character.identity_confirmed,
                "createdAt": d.character.created_at,
                "updatedAt": d.character.updated_at,
                "latestStats": d.character.latest_stats,
                "latestCurrencies": d.character.latest_currencies,
                "snapshotCount": d.snapshot_count,
                "lastRecordedAt": d.last_recorded_at,
            })).collect::<Vec<_>>(),
            "snapshots": snapshot_docs,
            // 下面三块是身份关联的全部依据，缺了它们导出就只是"账本"。
            "fingerprints": fingerprints,
            "appearances": appearances,
            "items": items,
            "imageFeatures": cache_entries,
            "settings": settings,
            "runtime": {
                "schemaVersion": self.schema_version().unwrap_or(0),
                "algorithmVersion": mhcore::ANALYSIS_VERSION,
            },
        }))
    }

    /// 特征缓存索引：只报键与时间，不含 payload。
    ///
    /// payload 是图片的派生数据（dHash/ORB/直方图），随时可由截图重算，体积
    /// 却可能上兆；备份里带一份数据库即可完整恢复，JSON 里只留索引便于对照。
    fn export_feature_cache_index(&self) -> Result<serde_json::Value> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt =
            conn.prepare("SELECT cache_key, created_at FROM image_features ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(serde_json::json!({
                "cacheKey": r.get::<_, String>(0)?,
                "createdAt": r.get::<_, i64>(1)?,
            }))
        })?;
        let mut entries = Vec::new();
        for r in rows {
            entries.push(r?);
        }
        Ok(serde_json::json!({
            "count": entries.len(),
            "algorithmVersion": mhcore::ANALYSIS_VERSION,
            "entries": entries,
        }))
    }

    /// 完整备份：`data.json` + `mhrecorder.db` + `manifest.json` 打进一个 zip。
    ///
    /// 数据库用 `VACUUM INTO` 取快照，而不是直接拷文件——WAL 模式下直接复制
    /// 主文件会漏掉还没 checkpoint 的事务。
    pub fn export_backup(&self, dest: &Path, include_database: bool) -> Result<BackupSummary> {
        let doc = self.export_document()?;
        let json = serde_json::to_string_pretty(&doc)
            .map_err(|e| StoreError::Invalid(format!("serialising export: {e}")))?;

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Io(format!("creating {parent:?}: {e}")))?;
        }

        let file = std::fs::File::create(dest)
            .map_err(|e| StoreError::Io(format!("creating {dest:?}: {e}")))?;
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let stored: zip::write::SimpleFileOptions =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        let mut entries = Vec::new();
        let mut push = |name: &str, data: &[u8], opt: zip::write::SimpleFileOptions| -> Result<()> {
            zip.start_file(name, opt)
                .map_err(|e| StoreError::Io(format!("zip entry {name}: {e}")))?;
            zip.write_all(data)
                .map_err(|e| StoreError::Io(format!("zip write {name}: {e}")))?;
            Ok(())
        };

        push("data.json", json.as_bytes(), opts)?;
        entries.push("data.json".to_string());

        let mut includes_database = false;
        if include_database {
            let tmp = dest.with_extension("db.tmp");
            let _ = std::fs::remove_file(&tmp);
            {
                let conn = self.conn.lock().expect("store mutex poisoned");
                conn.execute("VACUUM INTO ?1", params![tmp.to_string_lossy()])
                    .map_err(|e| StoreError::Db(format!("VACUUM INTO: {e}")))?;
            }
            let bytes = std::fs::read(&tmp)
                .map_err(|e| StoreError::Io(format!("reading {tmp:?}: {e}")))?;
            // 已经是压缩过的页结构，再 deflate 一次几乎没收益，直接存。
            push("mhrecorder.db", &bytes, stored)?;
            entries.push("mhrecorder.db".to_string());
            includes_database = true;
            let _ = std::fs::remove_file(&tmp);
        }

        let manifest = serde_json::json!({
            "format": "mhrecorder-backup/1",
            "appVersion": env!("CARGO_PKG_VERSION"),
            "schemaVersion": self.schema_version().unwrap_or(0),
            "algorithmVersion": mhcore::ANALYSIS_VERSION,
            "createdAt": now_ms(),
            "includesDatabase": includes_database,
            "counts": {
                "characters": doc.get("characters").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                "snapshots": doc.get("snapshots").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                "fingerprints": doc.get("fingerprints").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                "appearances": doc.get("appearances").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                "items": doc.get("items").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
            },
        });
        let manifest_text = serde_json::to_string_pretty(&manifest)
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        push("manifest.json", manifest_text.as_bytes(), opts)?;
        entries.push("manifest.json".to_string());

        zip.finish()
            .map_err(|e| StoreError::Io(format!("finishing zip: {e}")))?;

        let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        Ok(BackupSummary {
            path: dest.to_string_lossy().into_owned(),
            bytes,
            entries,
            includes_database,
        })
    }
}
