//! JSON-RPC-style wire protocol between the C# front end and the Rust
//! `mhservice` backend over a Windows named pipe.
//!
//! One request per line with these keys:
//! * `id`       - echo back so the client can correlate responses
//! * `method`   - e.g. `recognize`, `characters.list`
//! * `params`   - method-specific object
//!
//! One response per line:
//! * `id`       - mirrored from the request
//! * `ok`       - true when the method succeeded
//! * `result`   - method payload on success
//! * `error`    - `{ code, message }` on failure
//!
//! The transport is newline-delimited UTF-8 JSON. Base64 is used for binary
//! payloads (screenshots) so the protocol stays line-safe.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current protocol version. The client refuses to talk to a backend built
/// against a different major version.
pub const PROTOCOL_VERSION: u32 = 1;

/// 识别分析（特征 + OCR）的算法版本。
///
/// 两个地方靠它判断"这份旧数据还能不能用"：`image_features` 的缓存键前缀
/// （`v3:<sha256>`）和导出/备份清单里的 `algorithmVersion`。**改了 `mhvision`
/// 的特征算法（dHash / ORB / HSV）或换了 OCR 模型，就把这里 +1**，老缓存会
/// 自然失效，而不是被当成"合法的旧结果"读出来。
///
/// 放在 mhcore 是因为它同时属于存储层（缓存键、清单）和识别层（算法），
/// 两边都必须看到同一个数字。
pub const ANALYSIS_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self { id, ok: true, result: Some(result), error: None }
    }

    pub fn err(id: u64, code: &str, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody { code: code.to_string(), message: message.into() }),
        }
    }
}

/// Canonical method names. Keeping them in one place stops the C# client and
/// the Rust dispatcher drifting apart.
pub mod method {
    pub const PING: &str = "ping";
    pub const VERSION: &str = "version";
    pub const RECOGNIZE: &str = "recognize";
    pub const RECOGNIZE_SAVE: &str = "recognize.save";
    pub const CHARACTERS_LIST: &str = "characters.list";
    pub const CHARACTERS_GET: &str = "characters.get";
    pub const CHARACTERS_UPDATE: &str = "characters.update";
    pub const CHARACTERS_DELETE: &str = "characters.delete";
    pub const CHARACTERS_CONFIRM: &str = "characters.confirm";
    pub const STATISTICS: &str = "statistics.get";
    pub const SETTINGS_GET: &str = "settings.get";
    pub const SETTINGS_SET: &str = "settings.set";
    pub const EXPORT_JSON: &str = "export.json";
    pub const EXPORT_CSV: &str = "export.csv";
    pub const EXPORT_ZIP: &str = "export.zip";
    /// 完整备份：JSON + SQLite 文件 + 清单。
    pub const EXPORT_BACKUP: &str = "export.backup";
    pub const DATA_CLEAR: &str = "data.clear";
    /// 正在跑的任务（含耗时），给"识别中"面板和诊断用。
    pub const TASKS_LIST: &str = "tasks.list";
    /// 请求取消一个任务；识别会在下一张图之前停下。
    pub const TASKS_CANCEL: &str = "tasks.cancel";
}

/// Error codes the client switch-cases on.
pub mod errcode {
    pub const BAD_REQUEST: &str = "badRequest";
    pub const NOT_FOUND: &str = "notFound";
    pub const DECODE: &str = "decodeFailed";
    pub const OCR: &str = "ocrFailed";
    pub const STORAGE: &str = "storageFailed";
    pub const IO: &str = "ioFailed";
    pub const INTERNAL: &str = "internal";
    /// 两端协议版本不一致，直接拒绝服务。
    pub const PROTOCOL_MISMATCH: &str = "protocolMismatch";
    /// 被调用方取消。
    pub const CANCELLED: &str = "cancelled";
    /// 超过执行预算。
    pub const TIMEOUT: &str = "timeout";
    /// 超过大小 / 数量限制。
    pub const TOO_LARGE: &str = "tooLarge";
    /// 连接数已满。
    pub const TOO_MANY_CONNECTIONS: &str = "tooManyConnections";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips() {
        let r = Request { id: 7, method: "ping".into(), params: serde_json::json!({}) };
        let line = serde_json::to_string(&r).unwrap();
        assert!(!line.contains('\n'));
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.method, "ping");
    }

    #[test]
    fn success_response_omits_error() {
        let r = Response::ok(1, serde_json::json!({"status":"ready"}));
        let line = serde_json::to_string(&r).unwrap();
        assert!(!line.contains("error"));
        assert!(line.contains("\"ok\":true"));
    }

    #[test]
    fn error_response_omits_result() {
        let r = Response::err(2, errcode::BAD_REQUEST, "missing field");
        let line = serde_json::to_string(&r).unwrap();
        assert!(!line.contains("result"));
        let back: Response = serde_json::from_str(&line).unwrap();
        assert!(!back.ok);
        assert_eq!(back.error.unwrap().code, "badRequest");
    }

    #[test]
    fn method_names_are_stable() {
        assert_eq!(method::CHARACTERS_LIST, "characters.list");
        assert_eq!(method::EXPORT_BACKUP, "export.backup");
        assert_eq!(method::TASKS_CANCEL, "tasks.cancel");
    }
}
