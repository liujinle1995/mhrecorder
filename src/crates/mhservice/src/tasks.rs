//! 任务生命周期。
//!
//! 一次 `recognize` 可能跑几十秒（多张截图 × OCR + 关联）。在这期间客户端
//! 只能干等：超时了就断开，服务端却还在算。这一层给每个请求一个身份：
//!
//! * **开始**时登记，带一个取消标志和截止时间；
//! * **执行中**在阶段边界（每张图识别完）检查这两个条件；
//! * **结束**时落一行历史（`recognition_tasks`），带耗时和结果。
//!
//! 取消是协作式的：识别本身没法被硬中断（OpenCV/ORT 都是同步调用），所以
//! 粒度是一张图。这也正好是用户能感知的粒度——"点了取消，下一张不再处理"。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mhstore::maintenance::TaskRecord;

/// 交给识别流程的控制句柄。
#[derive(Clone)]
pub struct Control {
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}

impl Control {
    fn new(cancel: Arc<AtomicBool>, budget: Duration) -> Self {
        Self { cancel, deadline: Instant::now() + budget }
    }

    /// 还允许继续跑吗？返回 `Err(reason)` 说明该提前收工了。
    ///
    /// 识别流程内部走的是 `JobControl` 的两个布尔方法；这里提供一个合并判断，
    /// 给"要不要开始这次请求"之类的调用方用。
    #[allow(dead_code)]
    pub fn check(&self) -> std::result::Result<(), &'static str> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err("cancelled");
        }
        if Instant::now() >= self.deadline {
            return Err("timeout");
        }
        Ok(())
    }
}

impl mhvision::JobControl for Control {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn deadline_passed(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

/// 一个正在跑的任务。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningTask {
    pub id: String,
    pub method: String,
    pub started_at: i64,
    pub elapsed_ms: i64,
    pub state: &'static str,
}

struct Slot {
    id: String,
    method: String,
    started_at: i64,
    started: Instant,
    cancel: Arc<AtomicBool>,
    finished: bool,
}

/// 进程内的任务表。`Arc<Registry>` 挂在 `AppState` 上。
pub struct Registry {
    slots: Mutex<Vec<Slot>>,
    seq: AtomicU64,
}

impl Default for Registry {
    fn default() -> Self {
        Self { slots: Mutex::new(Vec::new()), seq: AtomicU64::new(0) }
    }
}

impl Registry {
    /// 登记一个任务，返回它的 id 与控制句柄。
    pub fn begin(&self, method: &str, budget: Duration) -> (String, Control) {
        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("t{n}-{}", chrono::Utc::now().timestamp_millis());
        let cancel = Arc::new(AtomicBool::new(false));
        let mut slots = self.slots.lock().expect("task registry poisoned");
        slots.push(Slot {
            id: id.clone(),
            method: method.to_string(),
            started_at: chrono::Utc::now().timestamp_millis(),
            started: Instant::now(),
            cancel: Arc::clone(&cancel),
            finished: false,
        });
        (id, Control::new(cancel, budget))
    }

    /// 收尾，返回可落盘的历史记录。
    pub fn finish(&self, id: &str, error: Option<&str>) -> Option<TaskRecord> {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        let slot = slots.iter_mut().find(|s| s.id == id && !s.finished)?;
        slot.finished = true;
        let elapsed = slot.started.elapsed();
        Some(TaskRecord {
            id: slot.id.clone(),
            method: slot.method.clone(),
            state: match error {
                Some("cancelled") => "cancelled".into(),
                Some("timeout") => "timeout".into(),
                Some(_) => "failed".into(),
                None => "done".into(),
            },
            started_at: slot.started_at,
            finished_at: Some(chrono::Utc::now().timestamp_millis()),
            duration_ms: Some(elapsed.as_millis() as i64),
            ok: Some(error.is_none()),
            error: error.map(String::from),
            detail: serde_json::Value::Null,
        })
    }

    /// 清掉已结束的条目；保留最近若干条未结束的以便诊断。
    pub fn prune(&self) {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        slots.retain(|s| !s.finished);
    }

    /// 正在跑的任务。
    pub fn running(&self) -> Vec<RunningTask> {
        let slots = self.slots.lock().expect("task registry poisoned");
        slots
            .iter()
            .filter(|s| !s.finished)
            .map(|s| RunningTask {
                id: s.id.clone(),
                method: s.method.clone(),
                started_at: s.started_at,
                elapsed_ms: s.started.elapsed().as_millis() as i64,
                state: "running",
            })
            .collect()
    }

    /// 请求取消一个任务。返回 true 表示找到了、且已经置位。
    pub fn cancel(&self, id: &str) -> bool {
        let slots = self.slots.lock().expect("task registry poisoned");
        match slots.iter().find(|s| s.id == id && !s.finished) {
            Some(slot) => {
                slot.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// 取消全部（关停时用）。
    pub fn cancel_all(&self) -> usize {
        let slots = self.slots.lock().expect("task registry poisoned");
        let mut n = 0;
        for s in slots.iter().filter(|s| !s.finished) {
            s.cancel.store(true, Ordering::Relaxed);
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mhvision::JobControl as _;

    #[test]
    fn begin_then_finish_records_duration() {
        let reg = Registry::default();
        let (id, control) = reg.begin("recognize", Duration::from_secs(30));
        assert!(control.check().is_ok());
        assert_eq!(reg.running().len(), 1);

        let rec = reg.finish(&id, None).expect("task should be found");
        assert_eq!(rec.state, "done");
        assert_eq!(rec.ok, Some(true));
        assert!(rec.duration_ms.unwrap() >= 0);
        assert!(reg.running().is_empty());
        // 重复收尾不产生第二条记录。
        assert!(reg.finish(&id, None).is_none());
    }

    #[test]
    fn canceling_flips_the_flag_for_the_pipeline() {
        let reg = Registry::default();
        let (id, control) = reg.begin("recognize", Duration::from_secs(30));
        assert!(!control.cancelled());
        assert!(reg.cancel(&id));
        assert!(control.cancelled(), "取消标志必须能被识别循环看到");
        assert_eq!(control.check().unwrap_err(), "cancelled");

        let rec = reg.finish(&id, Some("cancelled")).unwrap();
        assert_eq!(rec.state, "cancelled");
        assert_eq!(rec.ok, Some(false));

        // 已结束的任务不能再取消。
        assert!(!reg.cancel(&id));
    }

    #[test]
    fn deadline_is_enforced_between_stages() {
        let reg = Registry::default();
        let (id, control) = reg.begin("recognize", Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(control.deadline_passed());
        assert_eq!(control.check().unwrap_err(), "timeout");
        let rec = reg.finish(&id, Some("timeout")).unwrap();
        assert_eq!(rec.state, "timeout");
    }

    #[test]
    fn unknown_task_ids_are_rejected() {
        let reg = Registry::default();
        assert!(!reg.cancel("nope"));
        assert!(reg.finish("nope", None).is_none());
    }
}
