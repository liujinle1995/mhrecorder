//! 维护动作：清库、任务历史。
//!
//! 任务历史（`recognition_tasks`）是"任务生命周期"落盘的那一半：运行中的任务
//! 在 `mhservice::tasks` 的内存表里，结束后写一行到这里。崩过一次之后能回答
//! "上次到底跑到哪一步、花了多久"，而不是只剩一行 stderr。

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{now_ms, Result, Store};

/// 一次已结束的识别任务。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub id: String,
    pub method: String,
    /// running / done / failed / cancelled
    pub state: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub ok: Option<bool>,
    pub error: Option<String>,
    /// 结构化细节（图片张数、识别出的角色、候选数……），JSON 对象。
    pub detail: serde_json::Value,
}

impl Store {
    /// Delete every character-driven row (cascading), keeping settings and the
    /// task log.
    pub fn clear_all(&mut self) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute_batch(
            "DELETE FROM snapshots;
             DELETE FROM characters;
             DELETE FROM image_features;
             DELETE FROM item_sightings;
             DELETE FROM items;",
        )?;
        Ok(())
    }

    /// 追加一条任务记录。
    pub fn record_task(&mut self, task: &TaskRecord) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO recognition_tasks
             (id, method, state, started_at, finished_at, duration_ms, ok, error, detail)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(id) DO UPDATE SET
                 state = excluded.state,
                 finished_at = excluded.finished_at,
                 duration_ms = excluded.duration_ms,
                 ok = excluded.ok,
                 error = excluded.error,
                 detail = excluded.detail",
            params![
                task.id,
                task.method,
                task.state,
                task.started_at,
                task.finished_at,
                task.duration_ms,
                task.ok.map(|b| b as i64),
                task.error,
                task.detail.to_string(),
            ],
        )?;
        Ok(())
    }

    /// 最近的任务，新的在前。
    pub fn list_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, method, state, started_at, finished_at, duration_ms, ok, error, detail
             FROM recognition_tasks ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let detail: String = r.get(8)?;
            Ok(TaskRecord {
                id: r.get(0)?,
                method: r.get(1)?,
                state: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                duration_ms: r.get(5)?,
                ok: r.get::<_, Option<i64>>(6)?.map(|v| v != 0),
                error: r.get(7)?,
                detail: serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 只保留最近 `keep` 条任务记录（默认 500）。历史是诊断用的，不是账本。
    pub fn prune_tasks(&mut self, keep: usize) -> Result<usize> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let removed = conn.execute(
            "DELETE FROM recognition_tasks WHERE id NOT IN (
                 SELECT id FROM recognition_tasks ORDER BY started_at DESC LIMIT ?1
             )",
            params![keep as i64],
        )?;
        Ok(removed)
    }

    /// 库自身的体检信息，给导出的清单和设置页用。
    pub fn store_health(&self) -> Result<serde_json::Value> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let page_count: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
        let page_size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok(serde_json::json!({
            "schemaVersion": version,
            "pageCount": page_count,
            "pageSize": page_size,
            "bytes": page_count * page_size,
            "measuredAt": now_ms(),
        }))
    }
}
