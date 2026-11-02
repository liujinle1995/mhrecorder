//! Shared process state: the store plus the (expensive) vision pipeline.
//!
//! The pipeline holds ONNX sessions and the store owns a SQLite connection,
//! so both are created once and shared behind mutexes rather than rebuilt
//! per request. `AppState` is handed to a thread per pipe client.
//!
//! The pipeline is optional: if the OCR models are missing or fail to load we
//! still serve every storage/settings/export method (the app stays usable and
//! reports a clear error only when recognition is attempted).

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

pub struct AppState {
    store: Mutex<mhstore::Store>,
    pipeline: Mutex<Option<mhvision::RecognitionPipeline>>,
    /// Why the pipeline is unavailable, surfaced through `recognize`.
    pipeline_error: Option<String>,
}

impl AppState {
    pub fn new(models_dir: &Path, data_dir: &str) -> anyhow::Result<Arc<Self>> {
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
        Ok(Arc::new(Self { store: Mutex::new(store), pipeline, pipeline_error }))
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
        let mut guard: MutexGuard<'_, mhstore::Store> = self.store.lock().expect("store mutex poisoned");
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
}
