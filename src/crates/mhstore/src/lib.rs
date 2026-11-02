//! SQLite 持久化层。
//!
//! 仓储式拆分：每个子模块只放一类查询，`Store` 本身只负责连接、迁移和
//! 事务边界。服务层永远不碰 SQL，所以存储层可以脱离识别流程单独测试。
//!
//! 设计要点：
//! * 结构化字段（属性、装备、背包）以 JSON 列存；UI 要排序/检索的标量字段是
//!   真正的列。
//! * 时间戳是毫秒 Unix 时间，与 C# 侧一致。
//! * 单个 `Mutex<Connection>` 让 `Store` 满足 `Send`/`Sync`，多线程管道服务
//!   可以直接共享。
//! * 表结构由 [`migrations`] 管理，而不是在启动时无脑重跑建表语句。

pub mod cache;
mod characters;
mod export;
mod identity;
mod items;
pub mod maintenance;
mod migrations;
mod settings;
mod snapshots;

#[cfg(test)]
mod tests;

use mhcore::{Character, ScreenKind};
use rusqlite::{Connection, Row};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use migrations::schema_version;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("io error: {0}")]
    Io(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Default database file name inside a data directory.
pub const DB_FILE: &str = "mhrecorder.db";

/// A persisted character plus derived fields the UI needs.
#[derive(Debug, Clone)]
pub struct CharacterDetail {
    pub character: Character,
    pub snapshot_count: i64,
    pub last_recorded_at: Option<i64>,
}

pub struct Store {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) db_path: PathBuf,
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Store {
    /// Open (creating if needed) the database at `db_path` and run migrations.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Io(format!("creating {parent:?}: {e}")))?;
        }
        let mut conn = Connection::open(db_path)?;
        Self::configure(&conn)?;
        migrations::apply(&mut conn)?;
        Ok(Self { conn: Mutex::new(conn), db_path: db_path.to_path_buf() })
    }

    /// Open using the settings' data_dir, creating the directory if needed.
    pub fn open_in_data_dir(data_dir: &str) -> Result<Self> {
        let dir = Path::new(data_dir);
        std::fs::create_dir_all(dir)
            .map_err(|e| StoreError::Io(format!("creating {data_dir}: {e}")))?;
        Self::open(&dir.join(DB_FILE))
    }

    /// Apply the pragmas we rely on for correctness and concurrency.
    fn configure(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;",
        )?;
        Ok(())
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// 当前库的结构版本（已迁移到的版本）。
    pub fn schema_version(&self) -> Result<i64> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        migrations::current_version(&conn)
    }

    /// 走一个显式事务。`save_recognition` 这类"多表一起改"的操作必须用它，
    /// 中途失败要整批回滚，否则会留下没有快照的角色。
    pub(crate) fn transaction<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

/// JSON 列解码；坏数据退化成默认值而不是让整个查询失败。
pub(crate) fn from_json<T: serde::de::DeserializeOwned + Default>(raw: String) -> T {
    serde_json::from_str(&raw).unwrap_or_default()
}

/// `characters` 表的列顺序即 `row_to_character` 的索引顺序。
pub(crate) const CHARACTER_COLUMNS: &str = "id, nickname, game_id, level, school, server, \
     identity_confidence, identity_confirmed, avatar_path, latest_stats, latest_currencies, \
     created_at, updated_at";

pub(crate) fn row_to_character(row: &Row<'_>) -> rusqlite::Result<Character> {
    Ok(Character {
        id: row.get(0)?,
        nickname: row.get(1)?,
        game_id: row.get(2)?,
        level: row.get(3)?,
        school: row.get(4)?,
        server: row.get(5)?,
        identity_confidence: row.get(6)?,
        identity_confirmed: row.get::<_, i64>(7)? != 0,
        avatar_path: row.get(8)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        latest_stats: from_json(row.get(9)?),
        latest_currencies: from_json(row.get(10)?),
    })
}

/// Decode a stored colour, e.g. "[52,60,92]", back into three channels.
pub(crate) fn parse_rgb(raw: &str) -> Option<[u8; 3]> {
    let v: Vec<u8> = serde_json::from_str(raw).ok()?;
    if v.len() != 3 {
        return None;
    }
    Some([v[0], v[1], v[2]])
}

/// 面板种类在库里的字符串形式，和 serde 的 camelCase 保持一致。
pub(crate) fn screen_kind_str(kind: ScreenKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
