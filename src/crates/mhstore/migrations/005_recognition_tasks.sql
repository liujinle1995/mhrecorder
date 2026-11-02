-- v5 — 识别任务生命周期。
--
-- 运行中的任务只存在于内存（mhservice::tasks），这里落盘的是已结束的记录：
-- 谁在什么时候跑了什么、花了多久、成功还是失败、失败原因。
-- 崩过一次之后能回答"上次到底卡在哪一步"，而不是只有一行 stderr。
CREATE TABLE IF NOT EXISTS recognition_tasks (
    id           TEXT PRIMARY KEY,
    method       TEXT NOT NULL,
    state        TEXT NOT NULL,
    started_at   INTEGER NOT NULL,
    finished_at  INTEGER,
    duration_ms  INTEGER,
    ok           INTEGER,
    error        TEXT,
    detail       TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_tasks_started ON recognition_tasks(started_at DESC);
