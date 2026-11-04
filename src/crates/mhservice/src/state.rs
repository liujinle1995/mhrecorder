//! 进程共享状态：存储 + （昂贵的）识别流水线 + 请求边界 + 任务表。
//!
//! 流水线持有 ONNX 会话、存储持有 SQLite 连接，两者都只创建一次，用锁共享，
//! 而不是每个请求重建。`AppState` 以 `Arc` 形式交给每个管道连接线程。
//!
//! 流水线是**可选**的：模型缺失时仍然照常服务存储/设置/导出类方法，只在真正
//! 要识别时给出明确错误。

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::limits::Limits;
use crate::tasks::Registry;

pub struct AppState {
    store: Mutex<mhstore::Store>,
    pipeline: Mutex<Option<mhvision::RecognitionPipeline>>,
    /// Why the pipeline is unavailable, surfaced through `recognize`.
    pipeline_error: Option<String>,
    /// 请求边界（消息大小、图片数量、连接数、执行预算）。
    pub limits: Limits,
    /// 正在跑的任务，支持查询与取消。
    pub tasks: Arc<Registry>,

    // ---- 进程生命周期（由后端自己管，前端只负责连/断）----
    /// 这个后端服务的是哪个数据目录。`version` 会把它报给前端，用来判断
    /// "连着的是不是我该连的那个后端"，而不是靠进程名撞运气。
    data_dir: String,
    /// 当前活跃连接数（管道连接线程进出时各调一次）。
    connections: AtomicUsize,
    /// 是否有过至少一次连接。没有过就一直等——启动器可能只是先拉起了后端。
    served: AtomicBool,
    /// 最后一次连接变化的时间（毫秒时间戳），空闲退出用。
    last_activity_ms: AtomicI64,
    /// 有人请求关停（`system.shutdown`）。
    shutdown: AtomicBool,
}

/// 空闲多久没人连就自己退出。
///
/// 为什么需要它：前端崩溃 / 被任务管理器结束后，不会有人发 `system.shutdown`。
/// 0.15 靠"启动者退出时 Kill"兜底，但那既误伤别的窗口，又救不了崩溃。后端自己
/// 数连接，最后一个客户端走了再过宽限期就退出，谁都不用记"是谁拉起的我"。
const IDLE_EXIT_DEFAULT_SECS: u64 = 8;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl AppState {
    pub fn new(models_dir: &Path, data_dir: &str) -> anyhow::Result<Arc<Self>> {
        Self::with_limits(models_dir, data_dir, Limits::from_env())
    }

    pub fn with_limits(
        models_dir: &Path,
        data_dir: &str,
        limits: Limits,
    ) -> anyhow::Result<Arc<Self>> {
        let store = mhstore::Store::open_in_data_dir(data_dir)?;
        let (pipeline, pipeline_error) = match mhvision::RecognitionPipeline::new(models_dir) {
            Ok(p) => {
                tracing::info!("OCR pipeline loaded from {}", models_dir.display());
                (Mutex::new(Some(p)), None)
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!("OCR pipeline unavailable ({msg}); storage-only mode");
                (Mutex::new(None), Some(msg))
            }
        };
        tracing::info!("{}", limits.describe());
        Ok(Arc::new(Self {
            store: Mutex::new(store),
            pipeline,
            pipeline_error,
            limits,
            tasks: Arc::new(Registry::default()),
            data_dir: data_dir.to_string(),
            connections: AtomicUsize::new(0),
            served: AtomicBool::new(false),
            last_activity_ms: AtomicI64::new(now_ms()),
            shutdown: AtomicBool::new(false),
        }))
    }

    /// 后端服务的数据目录（绝对路径）。前端拿它校验归属。
    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }

    /// 活跃连接数。
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// 管道连接建立时调用。
    pub fn connection_opened(&self) {
        self.connections.fetch_add(1, Ordering::SeqCst);
        self.served.store(true, Ordering::SeqCst);
        self.last_activity_ms.store(now_ms(), Ordering::SeqCst);
        tracing::debug!("connections -> {}", self.connections());
    }

    /// 管道连接断开时调用。
    pub fn connection_closed(&self) {
        let prev = self.connections.fetch_sub(1, Ordering::SeqCst);
        // 计数归零（或异常情况下已经为 0）时重置空闲计时。
        if prev <= 1 {
            self.connections.store(0, Ordering::SeqCst);
            self.last_activity_ms.store(now_ms(), Ordering::SeqCst);
        }
        tracing::debug!("connections -> {}", self.connections());
    }

    /// 是否已经服务过至少一个客户端。
    pub fn has_served(&self) -> bool {
        self.served.load(Ordering::SeqCst)
    }

    /// 空闲了多久（毫秒）。
    pub fn idle_ms(&self) -> i64 {
        now_ms() - self.last_activity_ms.load(Ordering::SeqCst)
    }

    pub fn idle_exit_after(&self) -> std::time::Duration {
        let secs = std::env::var("MH_IDLE_EXIT_SEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(IDLE_EXIT_DEFAULT_SECS);
        std::time::Duration::from_secs(secs)
    }

    /// 请求关停（`system.shutdown` 或收到 Ctrl+C）。
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// True when recognition can actually run.
    pub fn recognition_available(&self) -> bool {
        self.pipeline_error.is_none()
    }

    pub fn pipeline_error(&self) -> Option<&str> {
        self.pipeline_error.as_deref()
    }

    /// Run a read-only store operation.
    pub fn with_store<T>(&self, f: impl FnOnce(&mhstore::Store) -> T) -> T {
        let guard: MutexGuard<'_, mhstore::Store> = self.store.lock().expect("store mutex poisoned");
        f(&guard)
    }

    /// Run a mutating store operation.
    pub fn with_store_mut<T>(&self, f: impl FnOnce(&mut mhstore::Store) -> T) -> T {
        let mut guard: MutexGuard<'_, mhstore::Store> =
            self.store.lock().expect("store mutex poisoned");
        f(&mut guard)
    }

    /// Run an operation against the vision pipeline.
    ///
    /// Returns `None` when the pipeline could not be loaded.
    pub fn with_pipeline<T>(
        &self,
        f: impl FnOnce(&mut mhvision::RecognitionPipeline) -> T,
    ) -> Option<T> {
        let mut guard = self.pipeline.lock().expect("pipeline mutex poisoned");
        guard.as_mut().map(f)
    }

    /// 落一条任务历史。写失败只记日志：任务已经是过去了，不该因为归档失败
    /// 再把结果吞掉。
    pub fn record_task(&self, task: &mhstore::maintenance::TaskRecord) {
        let result = self.with_store_mut(|s| s.record_task(task));
        if let Err(e) = result {
            tracing::warn!("task history not persisted: {e}");
        }
    }
}
