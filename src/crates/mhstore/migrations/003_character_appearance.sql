-- v3 — 角色外观特征（发色 / 武器）。
--
-- 与面板指纹分开存：指纹在换装备后立刻失效，而发色、武器属于角色本身，
-- 是"包裹截图不认识人"时最稳的一条线索。
CREATE TABLE IF NOT EXISTS character_appearance (
    character_id TEXT PRIMARY KEY REFERENCES characters(id) ON DELETE CASCADE,
    hair_name    TEXT NOT NULL,
    hair_rgb     TEXT NOT NULL,
    weapon_name  TEXT,
    weapon_rgb   TEXT,
    updated_at   INTEGER NOT NULL
);
