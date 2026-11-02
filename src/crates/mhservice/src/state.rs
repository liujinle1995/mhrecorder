//! 进程共享状态：存储 + （昂贵的）识别流水线 + 请求边界 + 任务表。
//!
//! 流水线持有 ONNX 会话、存储持有 SQLite 连接，两者都只创建一次，用锁共享，
//! 而不是每个请求重建。`AppState` 以 `Arc` 形式交给每个管道连接线程。
//!
//! 流水线是**可选**的：模型缺失时仍然照常服务存储/设置/导出类方法，只在真正
//! 要识别时给出明确错误。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

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
        }))
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
