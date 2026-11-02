-- v4 — 物品字典。
--
-- 目前只做"见过什么"：每次保存快照时把背包/装备里出现过的名字登记一次，
-- 记下首次与最近一次出现的时间、所在角色数。将来做图标识别时，这张表就是
-- 图标的标注来源；现在它让导出内容包含"我到底见过哪些东西"，且不依赖截图。
CREATE TABLE IF NOT EXISTS items (
    name          TEXT PRIMARY KEY,
    category      TEXT NOT NULL DEFAULT 'unknown',
    -- 首次出现时可用的图标缩略图（base64），暂无则为 NULL
    icon_thumb    TEXT,
    -- 见过这件东西的角色数，由 item_sightings 计数派生（写入时重算）
    character_count INTEGER NOT NULL DEFAULT 0,
    first_seen_at INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL
);

-- 谁见过它。单独一张表是为了让 character_count 是真数出来的，而不是自增计数器
-- （后者在同一个角色反复拍摄时会把 1 加到 7）。
CREATE TABLE IF NOT EXISTS item_sightings (
    item_name    TEXT NOT NULL REFERENCES items(name) ON DELETE CASCADE,
    character_id TEXT NOT NULL,
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY (item_name, character_id)
);

CREATE INDEX IF NOT EXISTS idx_items_last_seen ON items(last_seen_at);
