//! 物品字典。
//!
//! 现在只回答"我见过什么"：每次保存快照时把装备/背包里出现过的名字登记
//! 一次。将来做图标识别时，这张表就是图标的标注来源——不需要再手动维护清单。

use mhcore::RecognitionResult;
use rusqlite::{params, Connection};

use crate::Result;

/// 把一批识别结果里的物品登记进字典。每个名字只在第一次出现时写 `items`，
/// 之后只更新 `last_seen_at` 和"哪些角色见过"。
pub(crate) fn record_seen_items(
    tx: &Connection,
    character_id: &str,
    result: &RecognitionResult,
    now: i64,
) -> Result<()> {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for item in &result.inventory {
        let name = item.name.trim();
        if name.is_empty() {
            continue;
        }
        seen.push((name, "inventory"));
    }
    for item in &result.equipment {
        let name = item.name.trim();
        if name.is_empty() {
            continue;
        }
        seen.push((name, "equipment"));
    }
    seen.sort_unstable();
    seen.dedup_by(|a, b| a.0 == b.0);

    for (name, category) in seen {
        tx.execute(
            "INSERT INTO items(name, category, character_count, first_seen_at, last_seen_at)
             VALUES (?1, ?2, 0, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            params![name, category, now],
        )?;
        tx.execute(
            "INSERT INTO item_sightings(item_name, character_id, last_seen_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(item_name, character_id) DO UPDATE SET
                 last_seen_at = excluded.last_seen_at",
            params![name, character_id, now],
        )?;
        tx.execute(
            "UPDATE items
             SET character_count = (SELECT COUNT(*) FROM item_sightings WHERE item_name = ?1)
             WHERE name = ?1",
            params![name],
        )?;
    }
    Ok(())
}

impl crate::Store {
    /// 字典里全部物品，按最近出现排序。只给导出/统计用。
    pub fn list_items(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT name, category, character_count, first_seen_at, last_seen_at
             FROM items ORDER BY last_seen_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(serde_json::json!({
                "name": r.get::<_, String>(0)?,
                "category": r.get::<_, String>(1)?,
                "characterCount": r.get::<_, i64>(2)?,
                "firstSeenAt": r.get::<_, i64>(3)?,
                "lastSeenAt": r.get::<_, i64>(4)?,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 物品字典条目数。
    pub fn item_count(&self) -> Result<i64> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        Ok(conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?)
    }
}
