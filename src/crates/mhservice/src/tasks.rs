//! 任务生命周期。
//!
//! 一次识别可能跑几十秒（多张截图 × OCR + 关联）。0.15 及以前它是**同步**的：
//! 请求发出去，服务端在连接线程里一路算完，任务 id 要等结果一起返回。于是
//! "取消"在协议层面就不可能生效——服务端还堵在上一条请求里，读不到后面那条
//! `tasks.cancel`。
//!
//! 现在拆成两半：
//!
//! * `recognize.start` 只做登记，立刻返回 `taskId`，OCR 交给后台线程；
//! * 前端用 `tasks.get` 轮询状态，用户点取消就发 `tasks.cancel`，此时连接是
//!   空闲的，取消请求能被立刻读到。
//!
//! 状态机：
//!
//! ```text
//! Pending ──> Running ──> Completed / Failed / Timeout
//!    │           └──> Cancelling ──> Cancelled
//!    └──────────────> Cancelling ──> Cancelled   （还没开工就被取消）
//! ```
//!
//! 取消仍是**协作式**的：OpenCV / ONNX Runtime 都是同步调用，没法硬中断。
//! 粒度是一张图——"点了取消，下一张不再处理"，这正是用户能感知的粒度。所以
//! `Cancelling` 是真实存在、可能停留若干秒的状态，不是摆设。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mhstore::maintenance::TaskRecord;
use serde_json::Value;

/// 终态结果在表里保留多久。够前端补一次 `tasks.get`（网络抖动、切页面），
/// 又不会把几十 MB 的识别结果一直挂在内存里。
const RESULT_TTL_MS: i64 = 30_000;

/// 任务状态。序列化成前端 `TaskState` 枚举，两边字符串必须逐字一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// 已登记，工作线程还没真正开工。
    Pending,
    /// OCR 正在跑。
    Running,
    /// 已置取消位，等工作线程在下一个检查点停下。
    Cancelling,
    /// 用户取消并已停下。
    Cancelled,
    /// 正常跑完，有结果。
    Completed,
    /// 出错。
    Failed,
    /// 超过执行预算。
    Timeout,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Cancelling => "cancelling",
            TaskState::Cancelled => "cancelled",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Timeout => "timeout",
        }
    }

    /// 终态：不会再变，结果可以取走。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Cancelled | TaskState::Completed | TaskState::Failed | TaskState::Timeout
        )
    }

    /// 还占着 OCR 会话的状态。
    fn occupies_engine(self) -> bool {
        matches!(self, TaskState::Pending | TaskState::Running | TaskState::Cancelling)
    }
}

/// 任务的最终结果。放在任务表里，由 `tasks.get` 取走。
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    /// 成功时的结果负载（识别结果 JSON）。
    pub value: Option<Value>,
    /// 失败时的错误码（`cancelled` / `timeout` / 存储层的动态码）。
    ///
    /// 用 `String` 而不是 `&'static str`：同步路径的错误码来自存储层，
    /// 是运行时拼出来的，硬要求 'static 只会逼着调用方丢掉原始信息。
    pub code: Option<String>,
    pub message: Option<String>,
}

impl TaskOutcome {
    pub fn ok(value: Value) -> Self {
        Self { value: Some(value), code: None, message: None }
    }

    pub fn err(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { value: None, code: Some(code.into()), message: Some(message.into()) }
    }

    fn state(&self) -> TaskState {
        match self.code.as_deref() {
            None => TaskState::Completed,
            Some("cancelled") => TaskState::Cancelled,
            Some("timeout") => TaskState::Timeout,
            Some(_) => TaskState::Failed,
        }
    }
}

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
    /// 识别流程内部走的是 `JobControl` 的两个布尔方法（每张图边界各问一次），
    /// 这个合并判断留给"开工前先看一眼"的调用方与测试。
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

    /// 已置取消位（用于把任务标成 Cancelling）。
    #[allow(dead_code)]
    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
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

/// 一个正在跑的任务（`tasks.list` 的条目）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningTask {
    pub id: String,
    pub method: String,
    pub started_at: i64,
    pub elapsed_ms: i64,
    pub state: &'static str,
}

/// `tasks.get` 的一帧快照。
#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub id: String,
    pub method: String,
    pub state: TaskState,
    pub started_at: i64,
    pub elapsed_ms: i64,
    /// 终态时才有；只给一次（取走后置 None，避免前端反复拿到大对象）。
    pub outcome: Option<TaskOutcome>,
}

/// 取消的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 已置位，状态改为 Cancelling。
    Requested,
    /// 任务已经是终态，没什么可取消的。
    AlreadyDone(TaskState),
    NotFound,
}

/// 任务启动被拒：已有识别占着引擎。
#[derive(Debug, Clone, Copy)]
pub struct EngineBusy;

struct Slot {
    id: String,
    method: String,
    started_at: i64,
    started: Instant,
    finished_at: Option<i64>,
    state: TaskState,
    cancel: Arc<AtomicBool>,
    outcome: Option<TaskOutcome>,
    /// 结果已被 `tasks.get` 取走。取走后条目在下次 prune 时清掉。
    taken: bool,
    /// 终态后何时清理（毫秒时间戳），给前端留一次重试的余地。
    expires_at: Option<i64>,
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
    ///
    /// 识别是独占的（OCR 会话 + 流水线锁），所以已有任务在跑时直接拒绝，而不是
    /// 排进队列——排队会让"我还要等多久"不可预测，还会把前端重复点击藏起来。
    pub fn begin(&self, method: &str, budget: Duration) -> Result<(String, Control), EngineBusy> {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        if slots.iter().any(|s| s.state.occupies_engine()) {
            return Err(EngineBusy);
        }

        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("t{n}-{}", chrono::Utc::now().timestamp_millis());
        let cancel = Arc::new(AtomicBool::new(false));
        slots.push(Slot {
            id: id.clone(),
            method: method.to_string(),
            started_at: chrono::Utc::now().timestamp_millis(),
            started: Instant::now(),
            finished_at: None,
            state: TaskState::Pending,
            cancel: Arc::clone(&cancel),
            outcome: None,
            taken: false,
            expires_at: None,
        });
        Ok((id, Control::new(cancel, budget)))
    }

    /// 工作线程真正开工。
    pub fn mark_running(&self, id: &str) {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        if let Some(slot) = slots.iter_mut().find(|s| s.id == id) {
            // 开工前就被取消的，停在 Cancelling 让 worker 自己收尾。
            if slot.state == TaskState::Pending {
                slot.state = if slot.cancel.load(Ordering::Relaxed) {
                    TaskState::Cancelling
                } else {
                    TaskState::Running
                };
            }
        }
    }

    /// 收尾，返回可落盘的历史记录。
    pub fn complete(&self, id: &str, outcome: TaskOutcome) -> Option<TaskRecord> {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        let slot = slots.iter_mut().find(|s| s.id == id && !s.state.is_terminal())?;
        slot.state = outcome.state();
        slot.finished_at = Some(chrono::Utc::now().timestamp_millis());
        slot.expires_at = Some(chrono::Utc::now().timestamp_millis() + RESULT_TTL_MS);

        let record = TaskRecord {
            id: slot.id.clone(),
            method: slot.method.clone(),
            state: slot.state.as_str().to_string(),
            started_at: slot.started_at,
            finished_at: slot.finished_at,
            duration_ms: Some(slot.started.elapsed().as_millis() as i64),
            ok: Some(slot.state == TaskState::Completed),
            error: outcome.code.clone(),
            detail: Value::Null,
        };
        slot.outcome = Some(outcome);
        Some(record)
    }

    /// 请求取消一个任务。
    pub fn cancel(&self, id: &str) -> CancelOutcome {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        let Some(slot) = slots.iter_mut().find(|s| s.id == id) else {
            return CancelOutcome::NotFound;
        };
        if slot.state.is_terminal() {
            return CancelOutcome::AlreadyDone(slot.state);
        }

        slot.cancel.store(true, Ordering::Relaxed);
        // Pending / Running 都立刻进 Cancelling：前端能马上看到"正在停"，
        // 而不是点了取消界面毫无反应（worker 可能正卡在一张很大的图上）。
        slot.state = TaskState::Cancelling;
        CancelOutcome::Requested
    }

    /// 查一帧快照；`take` 为真时把终态结果取走（只给一次）。
    pub fn snapshot(&self, id: &str, take: bool) -> Option<TaskSnapshot> {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        let slot = slots.iter_mut().find(|s| s.id == id)?;

        let elapsed_ms = match slot.finished_at {
            Some(end) => end - slot.started_at,
            None => slot.started.elapsed().as_millis() as i64,
        };

        let outcome = if slot.state.is_terminal() && !slot.taken {
            if take {
                slot.taken = true;
            }
            slot.outcome.clone()
        } else {
            None
        };

        Some(TaskSnapshot {
            id: slot.id.clone(),
            method: slot.method.clone(),
            state: slot.state,
            started_at: slot.started_at,
            elapsed_ms,
            outcome,
        })
    }

    /// 正在跑的任务。
    pub fn running(&self) -> Vec<RunningTask> {
        let slots = self.slots.lock().expect("task registry poisoned");
        slots
            .iter()
            .filter(|s| !s.state.is_terminal())
            .map(|s| RunningTask {
                id: s.id.clone(),
                method: s.method.clone(),
                started_at: s.started_at,
                elapsed_ms: s.started.elapsed().as_millis() as i64,
                state: s.state.as_str(),
            })
            .collect()
    }

    /// 取消全部（关停时用）。
    pub fn cancel_all(&self) -> usize {
        let mut slots = self.slots.lock().expect("task registry poisoned");
        let mut n = 0;
        for s in slots.iter_mut().filter(|s| !s.state.is_terminal()) {
            s.cancel.store(true, Ordering::Relaxed);
            s.state = TaskState::Cancelling;
            n += 1;
        }
        n
    }

    /// 清掉可以丢的条目：完成已久、且没人再来取。
    pub fn prune(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        let mut slots = self.slots.lock().expect("task registry poisoned");
        slots.retain(|s| match s.expires_at {
            Some(exp) => now < exp,
            None => true,
        });
    }

    /// 只给测试用：当前条目数。
    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.lock().expect("task registry poisoned").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mhvision::JobControl as _;

    fn begin(reg: &Registry) -> (String, Control) {
        reg.begin("recognize.start", Duration::from_secs(30))
            .expect("first task should start")
    }

    #[test]
    fn task_walks_pending_running_completed() {
        let reg = Registry::default();
        let (id, _c) = begin(&reg);

        let snap = reg.snapshot(&id, true).unwrap();
        assert_eq!(snap.state, TaskState::Pending);
        assert!(snap.outcome.is_none(), "没跑完就没有结果");

        reg.mark_running(&id);
        assert_eq!(reg.snapshot(&id, true).unwrap().state, TaskState::Running);

        let rec = reg
            .complete(&id, TaskOutcome::ok(serde_json::json!({"ok": 1})))
            .unwrap();
        assert_eq!(rec.state, "completed");
        assert_eq!(rec.ok, Some(true));

        let snap = reg.snapshot(&id, true).unwrap();
        assert_eq!(snap.state, TaskState::Completed);
        assert!(snap.outcome.is_some());
    }

    #[test]
    fn result_is_handed_out_exactly_once() {
        let reg = Registry::default();
        let (id, _c) = begin(&reg);
        reg.mark_running(&id);
        reg.complete(&id, TaskOutcome::ok(serde_json::json!({"n": 1})));

        assert!(reg.snapshot(&id, true).unwrap().outcome.is_some());
        // 第二次仍能查到状态，但不再重复给结果（避免前端把结果贴两遍）。
        let second = reg.snapshot(&id, true).unwrap();
        assert_eq!(second.state, TaskState::Completed);
        assert!(second.outcome.is_none());
    }

    #[test]
    fn cancel_moves_running_task_to_cancelling_then_cancelled() {
        let reg = Registry::default();
        let (id, control) = begin(&reg);
        reg.mark_running(&id);

        assert_eq!(reg.cancel(&id), CancelOutcome::Requested);
        assert!(control.cancelled(), "取消标志必须能被识别循环看到");
        assert_eq!(control.check().unwrap_err(), "cancelled");
        // 立刻可见的中间态：前端据此显示"正在停止"。
        assert_eq!(reg.snapshot(&id, false).unwrap().state, TaskState::Cancelling);

        let rec = reg
            .complete(&id, TaskOutcome::err("cancelled", "用户取消"))
            .unwrap();
        assert_eq!(rec.state, "cancelled");
        assert_eq!(reg.snapshot(&id, false).unwrap().state, TaskState::Cancelled);
    }

    #[test]
    fn cancelling_before_work_starts_still_works() {
        let reg = Registry::default();
        let (id, control) = begin(&reg);
        assert_eq!(reg.cancel(&id), CancelOutcome::Requested);
        // 工作线程随后才开工，不应该把状态改回 Running。
        reg.mark_running(&id);
        assert!(control.cancelled());
        assert_eq!(reg.snapshot(&id, false).unwrap().state, TaskState::Cancelling);
    }

    #[test]
    fn terminal_tasks_cannot_be_cancelled_again() {
        let reg = Registry::default();
        let (id, _c) = begin(&reg);
        reg.complete(&id, TaskOutcome::ok(serde_json::json!({})));
        assert_eq!(reg.cancel(&id), CancelOutcome::AlreadyDone(TaskState::Completed));
        assert_eq!(reg.cancel("nope"), CancelOutcome::NotFound);
    }

    #[test]
    fn second_start_is_rejected_while_one_runs() {
        let reg = Registry::default();
        let (id, _c) = begin(&reg);
        assert!(
            reg.begin("recognize.start", Duration::from_secs(5)).is_err(),
            "引擎独占，第二次 start 必须被拒"
        );

        reg.complete(&id, TaskOutcome::ok(serde_json::json!({})));
        assert!(
            reg.begin("recognize.start", Duration::from_secs(5)).is_ok(),
            "上一个结束后应放行"
        );
    }

    #[test]
    fn deadline_is_reported_as_timeout() {
        let reg = Registry::default();
        let (id, control) = reg.begin("recognize.start", Duration::from_millis(1)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(control.deadline_passed());
        assert_eq!(control.check().unwrap_err(), "timeout");

        let rec = reg.complete(&id, TaskOutcome::err("timeout", "超时")).unwrap();
        assert_eq!(rec.state, "timeout");
        assert_ne!(rec.ok, Some(true));
    }

    #[test]
    fn completing_twice_does_not_duplicate_history() {
        let reg = Registry::default();
        let (id, _c) = begin(&reg);
        assert!(reg.complete(&id, TaskOutcome::ok(serde_json::json!({}))).is_some());
        assert!(reg.complete(&id, TaskOutcome::ok(serde_json::json!({}))).is_none());
    }

    #[test]
    fn prune_keeps_running_and_drops_expired_results() {
        let reg = Registry::default();
        let (done, _c) = begin(&reg);
        reg.complete(&done, TaskOutcome::ok(serde_json::json!({})));

        // 手工把过期时间调到过去：等价于"前端 30 秒都没来取"。
        {
            let mut slots = reg.slots.lock().unwrap();
            for s in slots.iter_mut() {
                s.expires_at = Some(0);
            }
        }
        reg.prune();
        assert_eq!(reg.len(), 0);

        let (running, _c) = begin(&reg);
        reg.mark_running(&running);
        reg.prune();
        assert_eq!(reg.len(), 1, "在跑的任务不能被 prune 掉");
    }

    #[test]
    fn cancel_all_stops_everything() {
        let reg = Registry::default();
        let (id, control) = begin(&reg);
        reg.mark_running(&id);
        assert_eq!(reg.cancel_all(), 1);
        assert!(control.cancelled());
        assert_eq!(reg.snapshot(&id, false).unwrap().state, TaskState::Cancelling);
    }
}
