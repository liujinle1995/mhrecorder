//! 版本化迁移引擎。
//!
//! 之前 `migrate()` 每次启动都把全部建表语句重跑一遍，然后直接把版本号写成
//! 常量——那只在"表结构永远只增不改"的假设下成立，一旦要做 `ALTER TABLE`
//! 或者给某个字段补数据就没法表达，也无法判断该不该跑。
//!
//! 现在：
//! * 每一步迁移是 `migrations/NNN_name.sql`，编号即版本，编译期嵌入二进制；
//! * 版本号写在 `PRAGMA user_version`（SQLite 原地存储，不需要额外的表）；
//! * 每一步单独一个事务，任何一条语句失败整步回滚，版本号不前进；
//! * 库比程序新时**拒绝打开**，而不是拿旧代码去写新结构。

use rusqlite::Connection;

use crate::{Result, StoreError};

/// 一步迁移。
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    /// 该步的建表 / 改表语句，整体在一个事务里执行。
    pub sql: &'static str,
    /// 纯 SQL 表达不了的修补。SQLite 没有 `ADD COLUMN IF NOT EXISTS`，
    /// 而老库可能已经带着这一列（更早的版本用代码补过），只能先查再改。
    pub fixup: Option<fn(&Connection) -> Result<()>>,
}

/// 全部迁移，按版本升序。**只允许追加，不允许修改已发布的步骤。**
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("../migrations/001_initial.sql"),
        fixup: None,
    },
    Migration {
        version: 2,
        name: "character_fingerprints",
        sql: include_str!("../migrations/002_character_fingerprints.sql"),
        fixup: None,
    },
    Migration {
        version: 3,
        name: "character_appearance",
        sql: include_str!("../migrations/003_character_appearance.sql"),
        // avatar_thumb 当初是用 ensure_column 随手加的，老库里可能已经有了。
        fixup: Some(add_avatar_thumb),
    },
    Migration {
        version: 4,
        name: "item_database",
        sql: include_str!("../migrations/004_item_database.sql"),
        fixup: None,
    },
    Migration {
        version: 5,
        name: "recognition_tasks",
        sql: include_str!("../migrations/005_recognition_tasks.sql"),
        fixup: None,
    },
];

/// 代码认识的最新版本。
pub fn schema_version() -> i64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

/// 读数据库当前版本（0 表示从未迁移过：0.10.0 及更早的库都是这样）。
pub fn current_version(conn: &Connection) -> Result<i64> {
    let v: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(StoreError::from)?;
    Ok(v)
}

/// 把 `conn` 升到最新版本。
pub fn apply(conn: &mut Connection) -> Result<()> {
    apply_list(conn, MIGRATIONS)?;
    verify(conn)
}

/// 执行一组迁移并落版本号；校验留给调用方。
fn apply_list(conn: &mut Connection, migrations: &[Migration]) -> Result<()> {
    let current = current_version(conn)?;
    let latest = migrations.last().map(|m| m.version).unwrap_or(0);

    if current > latest {
        return Err(StoreError::Invalid(format!(
            "数据库版本 v{current} 比当前程序支持的 v{latest} 新；\
             请升级 MhRecorder，不要用旧版本打开新数据（否则可能损坏）"
        )));
    }

    if current == latest {
        tracing::debug!("schema v{current}, nothing to migrate");
        return Ok(());
    }

    tracing::info!("migrating schema v{current} -> v{latest}");
    for m in migrations.iter().filter(|m| m.version > current) {
        // 每一步一个事务：语句或 fixup 失败时整步回滚，user_version 不前进，
        // 下次启动会从同一个版本重试，不会留下半张表。
        let tx = conn.transaction().map_err(StoreError::from)?;
        tx.execute_batch(m.sql).map_err(|e| {
            StoreError::Db(format!("migration {} ({}) failed: {e}", m.version, m.name))
        })?;
        if let Some(fixup) = m.fixup {
            fixup(&tx)?;
        }
        tx.pragma_update(None, "user_version", m.version)
            .map_err(StoreError::from)?;
        tx.commit().map_err(StoreError::from)?;
        tracing::info!("migrated to v{} ({})", m.version, m.name);
    }

    Ok(())
}

/// 迁移后的自检：代码要用的表和列必须真的存在。
///
/// 这一步专门抓"迁移脚本写漏了"的情况——否则错误会推迟到某个冷门功能第一次
/// 被点到时才以 `no such column` 的形式冒出来。
fn verify(conn: &Connection) -> Result<()> {
    const REQUIRED: &[(&str, &[&str])] = &[
        (
            "characters",
            &[
                "id",
                "nickname",
                "game_id",
                "avatar_thumb",
                "latest_stats",
                "latest_currencies",
            ],
        ),
        (
            "snapshots",
            &["id", "character_id", "recorded_at", "stats", "currencies", "inventory"],
        ),
        ("settings", &["key", "value"]),
        ("image_features", &["cache_key", "payload"]),
        ("character_fingerprints", &["character_id", "screen_kind", "thumb"]),
        ("character_appearance", &["character_id", "hair_rgb"]),
        ("items", &["name", "last_seen_at"]),
        ("recognition_tasks", &["id", "method", "state", "started_at"]),
    ];

    for (table, columns) in REQUIRED {
        let present = table_columns(conn, table)?;
        if present.is_empty() {
            return Err(StoreError::Db(format!(
                "迁移后缺少表 {table}（schema v{} 不完整）",
                current_version(conn).unwrap_or(-1)
            )));
        }
        for col in *columns {
            if !present.iter().any(|c| c == col) {
                return Err(StoreError::Db(format!("迁移后 {table} 缺少列 {col}")));
            }
        }
    }
    Ok(())
}

/// 列出某张表的列名；表不存在时返回空。
pub(crate) fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(names)
}

/// 缺列则补。只给"历史上用代码补过列"的迁移用。
fn add_avatar_thumb(conn: &Connection) -> Result<()> {
    let columns = table_columns(conn, "characters")?;
    if !columns.iter().any(|c| c == "avatar_thumb") {
        conn.execute_batch("ALTER TABLE characters ADD COLUMN avatar_thumb TEXT")?;
        tracing::info!("migrated: added characters.avatar_thumb");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn
    }

    #[test]
    fn fresh_database_lands_on_latest_version() {
        let mut conn = mem();
        apply(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), schema_version());
        // 幂等：再跑一次什么都不做。
        apply(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), schema_version());
    }

    #[test]
    fn legacy_database_without_user_version_is_adopted() {
        // 模拟 0.10.0 的库：表齐全、avatar_thumb 已由代码补过、user_version=0。
        let mut conn = mem();
        conn.execute_batch(
            r#"
            CREATE TABLE characters (
                id TEXT PRIMARY KEY, nickname TEXT NOT NULL, game_id TEXT, level INTEGER,
                school TEXT, server TEXT, identity_confidence REAL NOT NULL DEFAULT 0,
                identity_confirmed INTEGER NOT NULL DEFAULT 0, avatar_path TEXT,
                avatar_thumb TEXT, latest_stats TEXT NOT NULL DEFAULT '{}',
                latest_currencies TEXT NOT NULL DEFAULT '{}',
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE snapshots (
                id TEXT PRIMARY KEY,
                character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
                recorded_at INTEGER NOT NULL, stats TEXT NOT NULL DEFAULT '{}',
                currencies TEXT NOT NULL DEFAULT '{}', equipment TEXT NOT NULL DEFAULT '[]',
                inventory TEXT NOT NULL DEFAULT '[]', confidence REAL NOT NULL DEFAULT 0,
                source_screens TEXT NOT NULL DEFAULT '[]'
            );
            CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE image_features (cache_key TEXT PRIMARY KEY, payload TEXT NOT NULL,
                                         created_at INTEGER NOT NULL);
            INSERT INTO characters (id, nickname, created_at, updated_at)
                VALUES ('c1', '莫莫莫', 1, 1);
            "#,
        )
        .unwrap();

        apply(&mut conn).unwrap();

        assert_eq!(current_version(&conn).unwrap(), schema_version());
        // 老数据必须还在，而且新表可用。
        let name: String = conn
            .query_row("SELECT nickname FROM characters WHERE id='c1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "莫莫莫");
        conn.execute(
            "INSERT INTO recognition_tasks (id, method, state, started_at) VALUES ('t1','x','ok',1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn legacy_database_without_avatar_thumb_gets_the_column() {
        // 完整的老表结构，只缺 avatar_thumb（0.9.x 建的库）。
        let mut conn = mem();
        conn.execute_batch(
            r#"
            CREATE TABLE characters (
                id TEXT PRIMARY KEY, nickname TEXT NOT NULL, game_id TEXT, level INTEGER,
                school TEXT, server TEXT, identity_confidence REAL NOT NULL DEFAULT 0,
                identity_confirmed INTEGER NOT NULL DEFAULT 0, avatar_path TEXT,
                latest_stats TEXT NOT NULL DEFAULT '{}',
                latest_currencies TEXT NOT NULL DEFAULT '{}',
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            "#,
        )
        .unwrap();
        apply(&mut conn).unwrap();
        let cols = table_columns(&conn, "characters").unwrap();
        assert!(cols.iter().any(|c| c == "avatar_thumb"), "cols {cols:?}");
    }

    #[test]
    fn refuses_to_open_a_database_from_a_newer_build() {
        let mut conn = mem();
        apply(&mut conn).unwrap();
        conn.pragma_update(None, "user_version", schema_version() + 1).unwrap();

        let err = apply(&mut conn).unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
        assert!(err.to_string().contains("比当前程序支持的"), "{err}");
    }

    #[test]
    fn a_failing_step_rolls_back_and_keeps_the_version() {
        fn boom(_: &Connection) -> Result<()> {
            Err(StoreError::Db("fixup exploded".into()))
        }
        let list = &[
            Migration { version: 1, name: "ok", sql: "CREATE TABLE a(x INTEGER);", fixup: None },
            Migration {
                version: 2,
                name: "half",
                sql: "CREATE TABLE b(x INTEGER);",
                fixup: Some(boom),
            },
        ];

        let mut conn = mem();
        let err = apply_list(&mut conn, list).unwrap_err();
        assert!(err.to_string().contains("fixup exploded"), "{err}");

        // v1 已提交，v2 整步回滚：b 表不存在，版本停在 1。
        assert_eq!(current_version(&conn).unwrap(), 1);
        let has_b: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='b'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(has_b, 0, "失败的那一步必须整体回滚");
    }
}
