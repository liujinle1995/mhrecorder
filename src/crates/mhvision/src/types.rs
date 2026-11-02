//! 公共类型：错误、任务控制口、OCR 行。
//!
//! 只放"被别人引用"的东西，没有算法。

#[derive(Debug, thiserror::Error)]
pub enum VisionError {
    #[error("failed to decode image: {0}")]
    Decode(String),
    #[error("OpenCV error: {0}")]
    OpenCv(String),
    #[error("OCR error: {0}")]
    Ocr(String),
    #[error("model not found: {0}")]
    ModelMissing(String),
    #[error("onnx runtime not loadable: {0}")]
    RuntimeUnavailable(String),
    /// 调用方要求停止（用户在界面上点了取消）。
    #[error("识别已取消")]
    Cancelled,
    /// 超过了调用方给的执行预算。
    #[error("识别超时")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, VisionError>;

/// 长任务的协作式控制口。
///
/// 识别本身是同步的（OpenCV / ORT 都是阻塞调用），没法硬中断，所以取消和
/// 超时只能在阶段边界检查——批次里每张图识别完是一个天然的检查点。
pub trait JobControl: Send + Sync {
    fn cancelled(&self) -> bool {
        false
    }
    fn deadline_passed(&self) -> bool {
        false
    }
}

/// 不控制：CLI、测试和一次性调用的默认。
pub struct NoControl;

impl JobControl for NoControl {}

/// One recognised text line from the OCR engine, in screen coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub text: String,
    /// 0.0 - 1.0 recognition confidence.
    pub confidence: f64,
    /// Axis-aligned bounding box: (x, y, width, height) in pixels.
    pub rect: (i32, i32, i32, i32),
}

impl OcrLine {
    pub fn center_x(&self) -> f64 {
        self.rect.0 as f64 + self.rect.2 as f64 / 2.0
    }
    pub fn center_y(&self) -> f64 {
        self.rect.1 as f64 + self.rect.3 as f64 / 2.0
    }
}
