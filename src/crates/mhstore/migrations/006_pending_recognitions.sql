-- v6 — 待归属识别记录。
--
-- 一批截图完全没有身份信息（没有游戏ID、没有昵称，视觉也认不出是谁）时，以前
-- 会建一个叫「未命名角色」的行：它永远不会再被匹配到，却和真实角色一起出现在
-- 角色列表和资产统计里，把数字越摊越薄。
--
-- 现在这类结果先落到这张表，等用户指定归属后再变成正式记录。存的是完整的
-- RecognitionResult，归属时原样重放保存流程，不需要另一套字段映射。
CREATE TABLE IF NOT EXISTS pending_recognitions (
    id             TEXT PRIMARY KEY,
    created_at     INTEGER NOT NULL,
    -- 为什么没能归属，直接给人看
    reason         TEXT NOT NULL DEFAULT '',
    confidence     REAL NOT NULL DEFAULT 0,
    source_screens TEXT NOT NULL DEFAULT '[]',
    payload        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pending_created ON pending_recognitions(created_at);
