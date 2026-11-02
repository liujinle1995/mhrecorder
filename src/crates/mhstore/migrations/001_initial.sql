-- v1 — 初始表结构。
--
-- 全部使用 IF NOT EXISTS：0.10.0 及更早的版本建过这些表，却只把版本号写在
-- schema_meta 里、从不设置 PRAGMA user_version。这样的库第一次被新引擎打开时
-- version=0，会从头重放一遍 001。必须是幂等的，否则老用户升级即炸。
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
    id             TEXT PRIMARY KEY,
    character_id   TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    recorded_at    INTEGER NOT NULL,
    stats          TEXT NOT NULL DEFAULT '{}',
    currencies     TEXT NOT NULL DEFAULT '{}',
    equipment      TEXT NOT NULL DEFAULT '[]',
    inventory      TEXT NOT NULL DEFAULT '[]',
    confidence     REAL NOT NULL DEFAULT 0,
    source_screens TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 识别特征缓存。cache_key 自带算法版本前缀（见 mhstore::cache::feature_key），
-- 算法一变前缀就变，老条目自然失效，不需要迁移。
CREATE TABLE IF NOT EXISTS image_features (
    cache_key  TEXT PRIMARY KEY,
    payload    TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_characters_nickname ON characters(nickname);
CREATE INDEX IF NOT EXISTS idx_characters_game_id  ON characters(game_id);
CREATE INDEX IF NOT EXISTS idx_snapshots_char_time ON snapshots(character_id, recorded_at);
