-- v2 — 面板视觉指纹。
--
-- 每个 (角色, 面板种类) 只保留最新一张：指纹描述的是"这个角色现在的面板长什么样"，
-- 物品格会变，留历史没有意义。
CREATE TABLE IF NOT EXISTS character_fingerprints (
    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    screen_kind  TEXT NOT NULL,
    thumb        TEXT NOT NULL,
    score        INTEGER,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (character_id, screen_kind)
);
