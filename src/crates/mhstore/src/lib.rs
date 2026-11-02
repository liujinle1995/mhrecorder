//! SQLite persistence layer.
//!
//! Schema, migrations and all queries live here. The service layer never
//! touches SQL directly so storage can be tested standalone.
//!
//! Design notes:
//! * Rich structs (`CombatStats`, equipment, inventory) are stored as JSON
//!   columns; the scalar fields the UI sorts/searches on are real columns.
//! * Timestamps are milliseconds since the Unix epoch, matching the C# side.
//! * A single `Mutex<Connection>` makes the store `Send`/`Sync`, which the
//!   multi-threaded pipe server needs.

use mhcore::{AppearanceSignature, AssetPoint, AssetSummary, Character, CharacterAssetBar,
             CombatStats, Currencies, EquipmentItem, IdentityRef, InventoryItem, PanelFingerprint,
             RecognitionResult, ScreenKind, Settings, Snapshot, StatisticsPayload};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

/// Current schema version. Bump when adding a migration step.
// 2: added character_fingerprints (visual identity for panels that render no
//    游戏ID — the 包裹 screenshot). Additive: the table is created on open with
//    IF NOT EXISTS, so existing databases pick it up without a data migration.
// 3: added characters.avatar_thumb (16x16 grayscale of the portrait) so the
//    matcher can compare faces. Added with ALTER TABLE on existing databases.
const SCHEMA_VERSION: i64 = 3;

/// A persisted character plus derived fields the UI needs.
#[derive(Debug, Clone)]
pub struct CharacterDetail {
    pub character: Character,
    pub snapshot_count: i64,
    pub last_recorded_at: Option<i64>,
}

pub struct Store {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Store {
    /// Open (creating if needed) the database at `db_path` and run migrations.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Io(format!("creating {parent:?}: {e}")))?;
        }
        let conn = Connection::open(db_path)?;
        Self::configure(&conn)?;
        let store = Self { conn: Mutex::new(conn), db_path: db_path.to_path_buf() };
        store.migrate()?;
        Ok(store)
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

    /// Create tables and stamp the schema version. Idempotent.
    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS characters (
                id                  TEXT PRIMARY KEY,
                nickname            TEXT NOT NULL,
                game_id             TEXT,
                level               INTEGER,
                school              TEXT,
                server              TEXT,
                identity_confidence REAL NOT NULL DEFAULT 0,
                identity_confirmed  INTEGER NOT NULL DEFAULT 0,
                avatar_path         TEXT,
                avatar_thumb        TEXT,
                latest_stats        TEXT NOT NULL DEFAULT '{}',
                latest_currencies   TEXT NOT NULL DEFAULT '{}',
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS snapshots (
                id            TEXT PRIMARY KEY,
                character_id  TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
                recorded_at   INTEGER NOT NULL,
                stats         TEXT NOT NULL DEFAULT '{}',
                currencies    TEXT NOT NULL DEFAULT '{}',
                equipment     TEXT NOT NULL DEFAULT '[]',
                inventory     TEXT NOT NULL DEFAULT '[]',
                confidence    REAL NOT NULL DEFAULT 0,
                source_screens TEXT NOT NULL DEFAULT '[]'
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS image_features (
                cache_key  TEXT PRIMARY KEY,
                payload    TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            -- One fingerprint per (character, panel kind): the latest capture of
            -- that panel for that character. Used to link a 包裹 screenshot,
            -- which carries no 游戏ID, back to its owner.
            CREATE TABLE IF NOT EXISTS character_fingerprints (
                character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
                screen_kind  TEXT NOT NULL,
                thumb        TEXT NOT NULL,
                score        INTEGER,
                updated_at   INTEGER NOT NULL,
                PRIMARY KEY (character_id, screen_kind)
            );

            -- 发色 / 武器 of the character, independent of which panel it was
            -- read from. The fingerprint above goes stale as soon as the bag's
            -- items change (or when the capture is the other panel kind), while
            -- these traits belong to the character and survive both.
            CREATE TABLE IF NOT EXISTS character_appearance (
                character_id TEXT PRIMARY KEY REFERENCES characters(id) ON DELETE CASCADE,
                hair_name    TEXT NOT NULL,
                hair_rgb     TEXT NOT NULL,
                weapon_name  TEXT,
                weapon_rgb   TEXT,
                updated_at   INTEGER NOT NULL
            );

            -- 新库直接带上 avatar_thumb；老库由下面的 ensure_column 补。
            CREATE INDEX IF NOT EXISTS idx_characters_nickname ON characters(nickname);
            CREATE INDEX IF NOT EXISTS idx_characters_game_id  ON characters(game_id);
            CREATE INDEX IF NOT EXISTS idx_snapshots_char_time ON snapshots(character_id, recorded_at);
            "#,
        )?;

        // 后加的列：老库没有就必须补，否则 SELECT 会直接报 no such column。
        Self::ensure_column(&conn, "characters", "avatar_thumb", "TEXT")?;

        // 游戏ID 的唯一性：作用域（全区唯一 / 服务器内唯一）尚未确认，所以做成
        // "能加就加"——已有重复数据的老库不因此启动失败，冲突交给
        // resolve_identity 的 Ambiguous 分支提示用户。
        if let Err(e) = conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_characters_game_id_unique
             ON characters(game_id) WHERE game_id IS NOT NULL AND game_id <> ''",
        ) {
            tracing::warn!("未能建立 game_id 唯一索引（库中可能已有重复）：{e}");
        }

        conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;

        Ok(())
    }

    /// Add `column` to `table` when a database created by an older build lacks
    /// it. SQLite has no `ADD COLUMN IF NOT EXISTS`, so we ask table_info first.
    fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|row| row.ok())
            .collect();
        if !names.iter().any(|n| n == column) {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
            tracing::info!("migrated: added {table}.{column}");
        }
        Ok(())
    }

    // ---- characters -----------------------------------------------------

    pub fn list_characters(&self) -> Result<Vec<CharacterDetail>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, nickname, game_id, level, school, server, identity_confidence,
                    identity_confirmed, avatar_path, latest_stats, latest_currencies,
                    created_at, updated_at,
                    (SELECT COUNT(*) FROM snapshots s WHERE s.character_id = characters.id),
                    (SELECT MAX(recorded_at) FROM snapshots s WHERE s.character_id = characters.id)
             FROM characters
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let snap_count: i64 = row.get(13)?;
            let last_at: Option<i64> = row.get(14)?;
            Ok(CharacterDetail {
                character: row_to_character(row)?,
                snapshot_count: snap_count,
                last_recorded_at: last_at,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_character(&self, id: &str) -> Result<CharacterDetail> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, nickname, game_id, level, school, server, identity_confidence,
                    identity_confirmed, avatar_path, latest_stats, latest_currencies,
                    created_at, updated_at,
                    (SELECT COUNT(*) FROM snapshots s WHERE s.character_id = characters.id),
                    (SELECT MAX(recorded_at) FROM snapshots s WHERE s.character_id = characters.id)
             FROM characters WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            let snap_count: i64 = row.get(13)?;
            let last_at: Option<i64> = row.get(14)?;
            Ok(CharacterDetail {
                character: row_to_character(row)?,
                snapshot_count: snap_count,
                last_recorded_at: last_at,
            })
        })?;
        match rows.next() {
            Some(Ok(d)) => Ok(d),
            _ => Err(StoreError::NotFound(format!("character {id}"))),
        }
    }

    /// Find candidate characters for identity association by game id or
    /// nickname. Ordered by how strong the match is.
    pub fn find_identity_candidates(
        &self,
        game_id: Option<&str>,
        nickname: Option<&str>,
    ) -> Result<Vec<Character>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut out = Vec::new();

        if let Some(gid) = game_id.filter(|s| !s.trim().is_empty()) {
            let mut stmt = conn.prepare(
                "SELECT id, nickname, game_id, level, school, server, identity_confidence,
                        identity_confirmed, avatar_path, latest_stats, latest_currencies,
                        created_at, updated_at
                 FROM characters WHERE game_id = ?1",
            )?;
            let rows = stmt.query_map(params![gid], row_to_character)?;
            for r in rows {
                out.push(r?);
            }
        }

        if let Some(nick) = nickname.filter(|s| !s.trim().is_empty()) {
            let mut stmt = conn.prepare(
                "SELECT id, nickname, game_id, level, school, server, identity_confidence,
                        identity_confirmed, avatar_path, latest_stats, latest_currencies,
                        created_at, updated_at
                 FROM characters WHERE nickname = ?1",
            )?;
            let rows = stmt.query_map(params![nick], row_to_character)?;
            for r in rows {
                let c = r?;
                if !out.iter().any(|existing| existing.id == c.id) {
                    out.push(c);
                }
            }
        }

        Ok(out)
    }

    /// Insert or update a character from a recognition result and append a
    /// snapshot. Returns the character id.
    pub fn save_recognition(
        &mut self,
        result: &RecognitionResult,
        source_screens: &[String],
    ) -> Result<String> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;

        let now = now_ms();
        // A 包裹 capture shows no nickname at all. Falling back to the
        // placeholder is fine for a brand-new row, but it must never overwrite
        // the nickname a character already has — that is how a bag capture used
        // to rename 莫莫莫 to 未命名角色.
        let incoming_nickname = result
            .character
            .nickname
            .clone()
            .filter(|s| !s.trim().is_empty());
        let nickname = incoming_nickname
            .clone()
            .unwrap_or_else(|| "未命名角色".to_string());
        let game_id = result.character.game_id.clone().filter(|s| !s.trim().is_empty());

        // Prefer an explicit id from the pipeline, then match by game id, then
        // fall back to nickname. Manual corrections always win. With neither an
        // id nor a nickname (a 包裹 capture that could not be linked) there is
        // nothing to match on, so a row is created rather than guessed at.
        let existing: Option<String> = if let Some(cid) = result.character_id.as_deref() {
            tx.query_row("SELECT id FROM characters WHERE id = ?1", params![cid], |r| r.get(0))
                .optional()?
        } else if let Some(gid) = game_id.as_deref() {
            tx.query_row(
                "SELECT id FROM characters WHERE game_id = ?1 ORDER BY updated_at DESC LIMIT 1",
                params![gid],
                |r| r.get(0),
            )
            .optional()?
        } else if let Some(nick) = incoming_nickname.as_deref() {
            tx.query_row(
                "SELECT id FROM characters WHERE nickname = ?1 ORDER BY updated_at DESC LIMIT 1",
                params![nick],
                |r| r.get(0),
            )
            .optional()?
        } else {
            None
        };

        let stats_json = serde_json::to_string(&result.stats).unwrap_or_else(|_| "{}".into());
        let currency_json = serde_json::to_string(&result.currencies).unwrap_or_else(|_| "{}".into());

        let character_id = match existing {
            Some(id) => {
                tx.execute(
                    // COALESCE / CASE keep what we already know when the new
                    // capture simply does not show it: a 包裹 screenshot has no
                    // nickname and no combat stats, and saving it must not blank
                    // out the 人物属性 data recorded a minute earlier.
                    "UPDATE characters
                     SET nickname = COALESCE(?1, nickname),
                         game_id = COALESCE(?2, game_id),
                         level = COALESCE(?3, level),
                         school = COALESCE(?4, school),
                         identity_confidence = MAX(identity_confidence, ?5),
                         latest_stats = CASE WHEN ?6 = '{}' THEN latest_stats ELSE ?6 END,
                         latest_currencies = CASE WHEN ?7 = '{}' THEN latest_currencies ELSE ?7 END,
                         updated_at = ?8,
                         avatar_path = COALESCE(?10, avatar_path),
                         avatar_thumb = COALESCE(?11, avatar_thumb)
                     WHERE id = ?9",
                    params![
                        incoming_nickname,
                        game_id,
                        result.character.level,
                        result.character.school,
                        result.confidence,
                        stats_json,
                        currency_json,
                        now,
                        id,
                        // Store the cropped portrait as a base64 PNG in the
                        // avatar_path column; COALESCE keeps the previous one
                        // when a later capture had no avatar.
                        result.avatar_png_base64,
                        result.avatar_thumb,
                    ],
                )?;
                id
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO characters
                     (id, nickname, game_id, level, school, server, identity_confidence,
                      identity_confirmed, avatar_path, avatar_thumb, latest_stats,
                      latest_currencies, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,0,?12,?13,?8,?9,?10,?11)",
                    params![
                        id,
                        nickname,
                        game_id,
                        result.character.level,
                        result.character.school,
                        result.character.server,
                        result.confidence,
                        stats_json,
                        currency_json,
                        now,
                        now,
                        result.avatar_png_base64,
                        result.avatar_thumb,
                    ],
                )?;
                id
            }
        };

        let snapshot_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO snapshots
             (id, character_id, recorded_at, stats, currencies, equipment, inventory,
              confidence, source_screens)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                snapshot_id,
                character_id,
                now,
                serde_json::to_string(&result.stats).unwrap_or_else(|_| "{}".into()),
                serde_json::to_string(&result.currencies).unwrap_or_else(|_| "{}".into()),
                serde_json::to_string(&result.equipment).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&result.inventory).unwrap_or_else(|_| "[]".into()),
                result.confidence,
                serde_json::to_string(source_screens).unwrap_or_else(|_| "[]".into()),
            ],
        )?;

        // Remember what this character's panels looked like, so a later 包裹
        // capture — which renders no 游戏ID at all — can be linked back to them.
        for fingerprint in &result.fingerprints {
            let kind = serde_json::to_value(fingerprint.screen)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "unknown".to_string());
            tx.execute(
                "INSERT INTO character_fingerprints
                 (character_id, screen_kind, thumb, score, updated_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(character_id, screen_kind) DO UPDATE SET
                     thumb = excluded.thumb,
                     score = COALESCE(excluded.score, character_fingerprints.score),
                     updated_at = excluded.updated_at",
                params![character_id, kind, fingerprint.thumb, fingerprint.score, now],
            )?;
        }

        // 发色 / 武器. Kept apart from the panel thumbnails on purpose: the
        // thumbnails describe a picture that goes stale when the bag's items
        // change, this describes the character and does not.
        if let Some(app) = result.appearance.as_ref() {
            let hair_rgb =
                serde_json::to_string(&app.hair_rgb).unwrap_or_else(|_| "[0,0,0]".to_string());
            let weapon_rgb = app
                .weapon_rgb
                .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| String::new()));
            tx.execute(
                "INSERT INTO character_appearance
                 (character_id, hair_name, hair_rgb, weapon_name, weapon_rgb, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(character_id) DO UPDATE SET
                     hair_name = excluded.hair_name,
                     hair_rgb = excluded.hair_rgb,
                     weapon_name = COALESCE(excluded.weapon_name, character_appearance.weapon_name),
                     weapon_rgb = COALESCE(excluded.weapon_rgb, character_appearance.weapon_rgb),
                     updated_at = excluded.updated_at",
                params![
                    character_id,
                    app.hair_name,
                    hair_rgb,
                    app.weapon_name,
                    weapon_rgb,
                    now
                ],
            )?;
        }

        tx.commit()?;
        Ok(character_id)
    }

    /// Apply a human correction to identity fields and mark it confirmed.
    pub fn update_character_identity(
        &mut self,
        id: &str,
        nickname: Option<&str>,
        game_id: Option<&str>,
        level: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE characters
             SET nickname = COALESCE(?1, nickname),
                 game_id  = COALESCE(?2, game_id),
                 level    = COALESCE(?3, level),
                 identity_confirmed = 1,
                 identity_confidence = 1.0,
                 updated_at = ?4
             WHERE id = ?5",
            params![nickname, game_id, level, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("character {id}")));
        }
        Ok(())
    }

    pub fn confirm_character(&mut self, id: &str, confidence: f64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE characters SET identity_confirmed = 1, identity_confidence = ?1,
                                  updated_at = ?2
             WHERE id = ?3",
            params![confidence, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("character {id}")));
        }
        Ok(())
    }

    pub fn delete_character(&mut self, id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute("DELETE FROM characters WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("character {id}")));
        }
        Ok(())
    }

    // ---- snapshots ------------------------------------------------------

    pub fn list_snapshots(&self, character_id: &str, limit: usize) -> Result<Vec<Snapshot>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, character_id, recorded_at, stats, currencies, equipment, inventory,
                    confidence, source_screens
             FROM snapshots WHERE character_id = ?1
             ORDER BY recorded_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![character_id, limit as i64], |row| {
            Ok(Snapshot {
                id: row.get(0)?,
                character_id: row.get(1)?,
                recorded_at: row.get(2)?,
                stats: from_json::<CombatStats>(row.get(3)?),
                currencies: from_json::<Currencies>(row.get(4)?),
                equipment: from_json::<Vec<EquipmentItem>>(row.get(5)?),
                inventory: from_json::<Vec<InventoryItem>>(row.get(6)?),
                confidence: row.get(7)?,
                source_screens: from_json::<Vec<String>>(row.get(8)?),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---- statistics -----------------------------------------------------

    pub fn statistics(&self, recent_limit: usize) -> Result<StatisticsPayload> {
        let chars = self.list_characters()?;

        let mut gold_total = 0i64;
        let mut silver_total = 0i64;
        let mut bars = Vec::with_capacity(chars.len());
        for d in &chars {
            let gold = d.character.latest_currencies.gold.as_ref().map(|v| v.value).unwrap_or(0);
            let silver =
                d.character.latest_currencies.silver.as_ref().map(|v| v.value).unwrap_or(0);
            gold_total += gold;
            silver_total += silver;
            bars.push(CharacterAssetBar {
                character_id: d.character.id.clone(),
                nickname: d.character.nickname.clone(),
                gold_wan: gold as f64 / 10_000.0,
                silver_wan: silver as f64 / 10_000.0,
            });
        }

        // Aggregate the newest snapshot per character per day, then take the
        // most recent `recent_limit` days.
        let by_day = self.daily_totals()?;
        let recent: Vec<AssetPoint> = by_day
            .iter()
            .rev()
            .take(recent_limit)
            .rev()
            .cloned()
            .collect();

        // 环比只在两天都有该字段的有效记录时才有意义。
        let (gold_change, silver_change) = if recent.len() >= 2 {
            let last = &recent[recent.len() - 1];
            let prev = &recent[recent.len() - 2];
            let pct = |now: Option<i64>, before: Option<i64>| -> Option<f64> {
                match (now, before) {
                    (Some(now), Some(before)) if before != 0 => {
                        Some(((now - before) as f64 / before as f64) * 100.0)
                    }
                    _ => None,
                }
            };
            (pct(last.gold, prev.gold), pct(last.silver, prev.silver))
        } else {
            (None, None)
        };

        let summary = AssetSummary {
            gold_total,
            silver_total,
            character_count: chars.len() as i64,
            gold_change_percent: gold_change,
            silver_change_percent: silver_change,
            character_change: None,
        };

        Ok(StatisticsPayload { summary, characters: bars, recent })
    }

    /// Sum 金币/银币 per day, **taking each field's own last known value**.
    ///
    /// The old version took the newest snapshot of the day and read both
    /// currencies out of it. But a 人物属性 capture records stats and no
    /// currency at all, so its `{}` currencies were summed as 0/0 and a day
    /// whose last capture was a portrait panel showed a fake "金币归零" even
    /// though the bag had been photographed an hour earlier.
    ///
    /// Now each field is resolved independently to that character's last
    /// capture **that actually carried it**, and a field nobody ever captured
    /// that day stays `None` (the UI renders it as 未知, not 0).
    fn daily_totals(&self) -> Result<Vec<AssetPoint>> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let conn = conn.transaction()?;
        let mut stmt = conn.prepare(
            "SELECT character_id, recorded_at, currencies FROM snapshots
             ORDER BY recorded_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
        })?;

        // key: (day, character) -> (last known gold, last known silver)
        type Field = Option<i64>;
        let mut latest: std::collections::BTreeMap<(String, String), (Field, Field)> =
            std::collections::BTreeMap::new();
        for r in rows {
            let (char_id, ts, currency_json) = r?;
            let day = chrono::DateTime::from_timestamp_millis(ts)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "1970-01-01".to_string());
            let cur: Currencies = from_json(currency_json);
            let entry = latest.entry((day, char_id)).or_default();
            // ORDER BY recorded_at ASC ⇒ later rows overwrite; a snapshot that
            // simply does not show a currency leaves the previous value alone.
            if let Some(g) = cur.gold.as_ref().map(|v| v.value) {
                entry.0 = Some(g);
            }
            if let Some(s) = cur.silver.as_ref().map(|v| v.value) {
                entry.1 = Some(s);
            }
        }

        let mut days: std::collections::BTreeMap<String, (Field, Field)> =
            std::collections::BTreeMap::new();
        for ((day, _char), (gold, silver)) in latest {
            let e = days.entry(day).or_insert((None, None));
            if let Some(g) = gold {
                e.0 = Some(e.0.unwrap_or(0) + g);
            }
            if let Some(s) = silver {
                e.1 = Some(e.1.unwrap_or(0) + s);
            }
        }

        Ok(days
            .into_iter()
            .map(|(date, (gold, silver))| AssetPoint { date, gold, silver })
            .collect())
    }

    // ---- settings -------------------------------------------------------

    pub fn load_settings(&self) -> Result<Settings> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let stored: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = 'settings'", [], |r| r.get(0))
            .optional()?;
        Ok(stored
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .unwrap_or_default())
    }

    pub fn save_settings(&mut self, settings: &Settings) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let json = serde_json::to_string(settings)
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        conn.execute(
            "INSERT INTO settings(key, value) VALUES ('settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![json],
        )?;
        Ok(())
    }

    // ---- character fingerprints ------------------------------------------

    /// Remember the latest capture of one panel kind for a character.
    ///
    /// Only the newest one is kept: the fingerprint is meant to describe what
    /// that character's panel looks like *now*, and item grids change.
    pub fn save_fingerprint(
        &mut self,
        character_id: &str,
        fingerprint: &PanelFingerprint,
        updated_at: i64,
    ) -> Result<()> {
        let kind = serde_json::to_value(fingerprint.screen)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO character_fingerprints(character_id, screen_kind, thumb, score, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(character_id, screen_kind) DO UPDATE SET
                 thumb = excluded.thumb,
                 score = COALESCE(excluded.score, character_fingerprints.score),
                 updated_at = excluded.updated_at",
            params![character_id, kind, fingerprint.thumb, fingerprint.score, updated_at],
        )?;
        Ok(())
    }

    /// Every character's stored fingerprints, for the recognition pipeline.
    ///
    /// The 人物评分 is carried forward across rows even when a later capture did
    /// not show one, so a bag captured while the score was off-screen can still
    /// be matched by score.
    pub fn list_identity_refs(&self) -> Result<Vec<IdentityRef>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        // LEFT JOIN on both sides: a character that has an appearance signature
        // but no fingerprint yet (or the other way round) is still a candidate.
        let mut stmt = conn.prepare(
            "SELECT c.id, c.nickname, f.screen_kind, f.thumb, f.score,
                    a.hair_name, a.hair_rgb, a.weapon_name, a.weapon_rgb,
                    c.game_id, c.server, c.level, c.avatar_thumb, c.updated_at
             FROM characters c
             LEFT JOIN character_fingerprints f ON f.character_id = c.id
             LEFT JOIN character_appearance a ON a.character_id = c.id
             ORDER BY c.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<i64>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, i64>(13)?,
            ))
        })?;

        let mut refs: Vec<IdentityRef> = Vec::new();
        for row in rows {
            let (
                id, nickname, kind, thumb, score, hair_name, hair_rgb, weapon_name, weapon_rgb,
                game_id, server, level, avatar_thumb, updated_at,
            ) = row?;
            let entry = match refs.iter_mut().find(|r| r.character_id == id) {
                Some(e) => e,
                None => {
                    refs.push(IdentityRef {
                        character_id: id.clone(),
                        nickname: nickname.clone(),
                        game_id: game_id.clone(),
                        server: server.clone(),
                        level,
                        fingerprints: Vec::new(),
                        appearance: None,
                        avatar_thumb: avatar_thumb.clone(),
                        last_seen_at: Some(updated_at),
                    });
                    refs.last_mut().unwrap()
                }
            };

            if let Some(kind) = kind {
                let screen = serde_json::from_value::<ScreenKind>(serde_json::Value::String(kind))
                    .unwrap_or(ScreenKind::Unknown);
                if let Some(thumb) = thumb {
                    entry.fingerprints.push(PanelFingerprint { screen, thumb, score });
                }
            }

            if entry.appearance.is_none() {
                if let (Some(hair_name), Some(hair_rgb)) = (hair_name, hair_rgb) {
                    entry.appearance = Some(AppearanceSignature {
                        hair_name,
                        hair_rgb: parse_rgb(&hair_rgb).unwrap_or([0, 0, 0]),
                        weapon_name,
                        weapon_rgb: weapon_rgb.as_deref().and_then(parse_rgb),
                    });
                }
            }
        }
        Ok(refs)
    }

    // ---- feature cache --------------------------------------------------

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
    pub fn put_image_feature(&mut self, cache_key: &str, payload: &str, created_at: i64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO image_features(cache_key, payload, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(cache_key) DO UPDATE SET payload = excluded.payload",
            params![cache_key, payload, created_at],
        )?;
        Ok(())
    }

    // ---- maintenance ----------------------------------------------------

    /// Delete every character and snapshot (cascading), keeping settings.
    pub fn clear_all(&mut self) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute_batch("DELETE FROM snapshots; DELETE FROM characters; DELETE FROM image_features;")?;
        Ok(())
    }

    /// Dump everything as one JSON document for export.
    pub fn export_document(&self) -> Result<serde_json::Value> {
        let characters = self.list_characters()?;
        let mut snapshot_docs = Vec::new();
        for d in &characters {
            for s in self.list_snapshots(&d.character.id, 10_000)? {
                snapshot_docs.push(serde_json::to_value(s).unwrap_or(serde_json::Value::Null));
            }
        }
        let settings = self.load_settings()?;
        Ok(serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "exportedAt": now_ms(),
            "characters": characters.iter().map(|d| serde_json::json!({
                "id": d.character.id,
                "nickname": d.character.nickname,
                "gameId": d.character.game_id,
                "level": d.character.level,
                "school": d.character.school,
                "identityConfidence": d.character.identity_confidence,
                "identityConfirmed": d.character.identity_confirmed,
                "createdAt": d.character.created_at,
                "updatedAt": d.character.updated_at,
                "latestStats": d.character.latest_stats,
                "latestCurrencies": d.character.latest_currencies,
                "snapshotCount": d.snapshot_count,
            })).collect::<Vec<_>>(),
            "snapshots": snapshot_docs,
            "settings": settings,
        }))
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

fn from_json<T: serde::de::DeserializeOwned + Default>(raw: String) -> T {
    serde_json::from_str(&raw).unwrap_or_default()
}

fn row_to_character(row: &Row<'_>) -> rusqlite::Result<Character> {
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
fn parse_rgb(raw: &str) -> Option<[u8; 3]> {
    let v: Vec<u8> = serde_json::from_str(raw).ok()?;
    if v.len() != 3 {
        return None;
    }
    Some([v[0], v[1], v[2]])
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mhcore::{AttributeValue, IdentitySource, PanelFingerprint, ScreenKind};

    /// Each test gets its own temp database so runs never interfere.
    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("mhstore-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(DB_FILE);
        (Store::open(&path).unwrap(), dir)
    }

    fn sample_result(nickname: &str, game_id: &str, gold: i64, silver: i64) -> RecognitionResult {
        let mut r = RecognitionResult::new(IdentitySource::IdField);
        r.character.nickname = Some(nickname.to_string());
        r.character.game_id = Some(game_id.to_string());
        r.character.level = Some(89);
        r.currencies.gold = Some(AttributeValue::new(gold, 0.99, gold.to_string()));
        r.currencies.silver = Some(AttributeValue::new(silver, 0.99, silver.to_string()));
        r.stats.hp = Some(AttributeValue::new(7721, 0.99, "7,721"));
        r.confidence = 0.96;
        r
    }

    /// A 包裹 capture has no nickname and no combat stats. Saving it against a
    /// character must not rename them to 未命名角色 nor blank out the stats the
    /// 人物属性 capture recorded a moment earlier.
    #[test]
    fn bag_save_keeps_nickname_and_stats() {
        let (mut store, _dir) = temp_store();

        let attrs = sample_result("莫莫莫", "429277361", 0, 0);
        let id = store.save_recognition(&attrs, &["a.png".into()]).unwrap();

        // Same character, from a 包裹 capture: no nickname, no stats, linked by
        // the user (or by its fingerprint) via `character_id`.
        let mut bag = RecognitionResult::new(IdentitySource::VisualMatch);
        bag.character_id = Some(id.clone());
        bag.currencies.gold = Some(AttributeValue::new(2_625_812, 0.99, "2.625.812"));
        bag.fingerprints.push(PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb: "AAAA".into(),
            score: Some(20214),
        });
        store.save_recognition(&bag, &["b.png".into()]).unwrap();

        let chars = store.list_characters().unwrap();
        assert_eq!(chars.len(), 1, "a linked bag must not create a second row");
        assert_eq!(chars[0].character.nickname, "莫莫莫", "nickname must survive");
        assert_eq!(
            chars[0].character.latest_stats.hp.as_ref().map(|v| v.value),
            Some(7721),
            "stats must survive a capture that did not show them"
        );

        let refs = store.list_identity_refs().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].nickname, "莫莫莫");
        assert_eq!(refs[0].fingerprints.len(), 1);
        assert_eq!(refs[0].fingerprints[0].screen, ScreenKind::Inventory);
        assert_eq!(refs[0].fingerprints[0].score, Some(20214));
    }

    #[test]
    fn open_twice_is_idempotent() {        let (store, dir) = temp_store();
        drop(store);
        // Re-opening must not fail on existing tables.
        let reopened = Store::open(&dir.join(DB_FILE)).unwrap();
        assert!(reopened.list_characters().unwrap().is_empty());
    }

    #[test]
    fn save_recognition_creates_character_and_snapshot() {
        let (mut store, _dir) = temp_store();
        let r = sample_result("海淘淘", "429277013", 2_942_685, 14_297_169);
        let id = store.save_recognition(&r, &["a.png".into()]).unwrap();

        let chars = store.list_characters().unwrap();
        assert_eq!(chars.len(), 1);
        assert_eq!(chars[0].character.nickname, "海淘淘");
        assert_eq!(chars[0].snapshot_count, 1);
        assert!(chars[0].last_recorded_at.is_some());

        let snaps = store.list_snapshots(&id, 10).unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].stats.hp.as_ref().unwrap().value, 7721);
        assert_eq!(snaps[0].source_screens, vec!["a.png".to_string()]);
    }

    #[test]
    fn saving_same_game_id_updates_instead_of_inserting() {
        let (mut store, _dir) = temp_store();
        let id1 = store
            .save_recognition(&sample_result("海淘淘", "429277013", 100, 200), &[])
            .unwrap();
        let id2 = store
            .save_recognition(&sample_result("海淘淘", "429277013", 300, 400), &[])
            .unwrap();

        assert_eq!(id1, id2, "same game id must resolve to the same character row");
        let chars = store.list_characters().unwrap();
        assert_eq!(chars.len(), 1, "no duplicate character row");
        let snaps = store.list_snapshots(&id1, 10).unwrap();
        assert_eq!(snaps.len(), 2, "both observations are kept as snapshots");
        assert_eq!(
            chars[0].character.latest_currencies.gold.as_ref().unwrap().value,
            300,
            "latest currency is updated"
        );
    }

    /// 直接写快照，用来构造"同一天里先包裹后人物属性"这种时序。
    fn seed_snapshot(
        store: &Store,
        character_id: &str,
        when: &str,
        gold: Option<i64>,
        silver: Option<i64>,
    ) {
        let ts = chrono::DateTime::parse_from_rfc3339(when).unwrap().timestamp_millis();
        let mut cur = Currencies::default();
        if let Some(g) = gold {
            cur.gold = Some(AttributeValue::new(g, 0.9, g.to_string()));
        }
        if let Some(s) = silver {
            cur.silver = Some(AttributeValue::new(s, 0.9, s.to_string()));
        }
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO snapshots
             (id, character_id, recorded_at, stats, currencies, equipment, inventory,
              confidence, source_screens)
             VALUES (?1, ?2, ?3, '{}', ?4, '[]', '[]', 0.9, '[]')",
            params![
                uuid::Uuid::new_v4().to_string(),
                character_id,
                ts,
                serde_json::to_string(&cur).unwrap()
            ],
        )
        .unwrap();
    }

    /// P0-5：当天最后一张是「人物属性」（不含金币）时，金币必须沿用当天最后一次
    /// **有效**记录，而不是被最新快照的空货币对象覆盖成 0。
    #[test]
    fn daily_totals_do_not_zero_a_field_missing_from_the_last_capture() {
        let (mut store, _dir) = temp_store();
        let id = store
            .save_recognition(&sample_result("海淘淘", "1", 0, 0), &[])
            .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("DELETE FROM snapshots", []).unwrap();
        }
        seed_snapshot(&store, &id, "2026-10-01T10:00:00Z", Some(2_600_000), Some(100));
        // 11:00 的人物属性截图：只有属性，没有金币银币
        seed_snapshot(&store, &id, "2026-10-01T11:00:00Z", None, None);

        let stats = store.statistics(5).unwrap();
        let day = stats.recent.last().expect("one day");
        assert_eq!(day.date, "2026-10-01");
        assert_eq!(day.gold, Some(2_600_000), "金币不能被空快照抹成 0");
        assert_eq!(day.silver, Some(100));
    }

    /// 每个字段各自取最后一次有效值：11 点只补了银币，金币应保留 10 点的。
    #[test]
    fn daily_totals_merge_fields_independently() {
        let (mut store, _dir) = temp_store();
        let id = store
            .save_recognition(&sample_result("海淘淘", "1", 0, 0), &[])
            .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("DELETE FROM snapshots", []).unwrap();
        }
        seed_snapshot(&store, &id, "2026-10-01T09:00:00Z", Some(1000), None);
        seed_snapshot(&store, &id, "2026-10-01T10:00:00Z", None, Some(77));

        let stats = store.statistics(5).unwrap();
        let day = stats.recent.last().unwrap();
        assert_eq!(day.gold, Some(1000));
        assert_eq!(day.silver, Some(77));
    }

    /// 当天从来没有出现过金币 → 未知（None），不是 0。
    #[test]
    fn a_field_never_captured_stays_unknown() {
        let (mut store, _dir) = temp_store();
        let id = store
            .save_recognition(&sample_result("海淘淘", "1", 0, 0), &[])
            .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("DELETE FROM snapshots", []).unwrap();
        }
        seed_snapshot(&store, &id, "2026-10-01T10:00:00Z", None, Some(50));

        let stats = store.statistics(5).unwrap();
        let day = stats.recent.last().unwrap();
        assert_eq!(day.gold, None, "从未采集到的字段必须是未知");
        assert_eq!(day.silver, Some(50));
    }

    #[test]
    fn statistics_computes_percent_change() {
        let (mut store, _dir) = temp_store();
        let id = store
            .save_recognition(&sample_result("海淘淘", "1", 1000, 2000), &[])
            .unwrap();
        let mut second = sample_result("海淘淘", "1", 1100, 2000);
        // Force distinct days so aggregation sees two points.
        second.character.nickname = Some("海淘淘".into());
        store.save_recognition(&second, &[]).unwrap();

        let stats = store.statistics(5).unwrap();
        assert_eq!(stats.summary.character_count, 1);
        // Same day => one aggregated point, so no percentage change available.
        assert_eq!(stats.recent.len(), 1);
        assert!(stats.summary.gold_change_percent.is_none());

        // Values are reported in 万元.
        let bar = &stats.characters[0];
        assert!((bar.gold_wan - 0.11).abs() < 1e-9, "1100 => 0.11 万, got {}", bar.gold_wan);
        let _ = id;
    }

    #[test]
    fn identity_candidates_match_game_id_and_nickname() {
        let (mut store, _dir) = temp_store();
        store.save_recognition(&sample_result("海淘淘", "429277013", 1, 1), &[]).unwrap();

        let by_id = store.find_identity_candidates(Some("429277013"), None).unwrap();
        assert_eq!(by_id.len(), 1);

        let by_name = store.find_identity_candidates(None, Some("海淘淘")).unwrap();
        assert_eq!(by_name.len(), 1);

        let dust = store.find_identity_candidates(Some("nope"), Some("nobody")).unwrap();
        assert!(dust.is_empty());

        // A game id hit and a nickname hit of the same row must not duplicate.
        let both = store.find_identity_candidates(Some("429277013"), Some("海淘淘")).unwrap();
        assert_eq!(both.len(), 1);
    }

    #[test]
    fn update_identity_marks_confirmed() {
        let (mut store, _dir) = temp_store();
        let id = store.save_recognition(&sample_result("海淘淘", "1", 1, 1), &[]).unwrap();
        store.update_character_identity(&id, Some("海涛涛"), Some("429277013"), Some(90)).unwrap();

        let d = store.get_character(&id).unwrap();
        assert_eq!(d.character.nickname, "海涛涛");
        assert!(d.character.identity_confirmed);
        assert_eq!(d.character.identity_confidence, 1.0);
        assert_eq!(d.character.level, Some(90));
    }

    #[test]
    fn delete_character_cascades_snapshots() {
        let (mut store, _dir) = temp_store();
        let id = store.save_recognition(&sample_result("海淘淘", "1", 1, 1), &[]).unwrap();
        store.delete_character(&id).unwrap();
        assert!(store.get_character(&id).is_err());
        assert!(store.list_snapshots(&id, 10).unwrap().is_empty());
    }

    #[test]
    fn clear_all_keeps_settings() {
        let (mut store, _dir) = temp_store();
        store.save_recognition(&sample_result("海淘淘", "1", 1, 1), &[]).unwrap();
        let mut settings = store.load_settings().unwrap();
        settings.auto_navigate_after_save = true;
        store.save_settings(&settings).unwrap();

        store.clear_all().unwrap();
        assert!(store.list_characters().unwrap().is_empty());

        let after = store.load_settings().unwrap();
        assert!(after.auto_navigate_after_save, "settings survive a data wipe");
    }

    #[test]
    fn export_document_has_expected_shape() {
        let (mut store, _dir) = temp_store();
        store.save_recognition(&sample_result("海淘淘", "1", 5, 6), &[]).unwrap();
        let doc = store.export_document().unwrap();
        assert!(doc.get("characters").unwrap().as_array().unwrap().len() == 1);
        assert!(doc.get("snapshots").unwrap().as_array().unwrap().len() == 1);
        assert!(doc.get("settings").is_some());
        assert!(doc.get("exportedAt").is_some());
    }

    #[test]
    fn feature_cache_roundtrips() {
        let (mut store, _dir) = temp_store();
        assert!(store.get_image_feature("abc").unwrap().is_none());
        store.put_image_feature("abc", "{\"phash\":123}", 42).unwrap();
        let got = store.get_image_feature("abc").unwrap().unwrap();
        assert_eq!(got, "{\"phash\":123}");
        // Upsert replaces rather than errors.
        store.put_image_feature("abc", "{\"phash\":456}", 43).unwrap();
        assert_eq!(store.get_image_feature("abc").unwrap().unwrap(), "{\"phash\":456}");
    }

    #[test]
    fn settings_defaults_are_sane() {
        let (store, _dir) = temp_store();
        let s = store.load_settings().unwrap();
        assert!(s.editable_before_save, "default matches the design's ON toggle");
        assert!(!s.auto_navigate_after_save);
    }
}
