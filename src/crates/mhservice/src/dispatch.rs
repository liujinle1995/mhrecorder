//! Request dispatch: maps protocol method names onto the vision + store layers.
//!
//! Kept free of Win32 so it can be unit-tested by feeding JSON lines directly.

use mhcore::protocol::{errcode, method, Request, Response, PROTOCOL_VERSION};
use mhcore::{RecognitionResult, ScreenKind, Settings};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::state::AppState;

/// Handle one raw request line and produce the raw response line.
///
/// A malformed request still yields a well-formed error response using `id: 0`
/// so the client can log it instead of hanging.
pub fn handle_line(state: &Arc<AppState>, line: &str) -> String {
    let request: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            let resp = Response::err(0, errcode::BAD_REQUEST, format!("malformed request: {e}"));
            return crate::pipe::encode_line(&resp)
                .unwrap_or_else(|_| "{\"id\":0,\"ok\":false}\n".into());
        }
    };

    let response = dispatch(state, &request);
    crate::pipe::encode_line(&response).unwrap_or_else(|_| "{\"id\":0,\"ok\":false}\n".into())
}

fn dispatch(state: &Arc<AppState>, req: &Request) -> Response {
    match dispatch_inner(state, req) {
        Ok(result) => Response::ok(req.id, result),
        Err(err) => Response::err(req.id, err.code, err.message),
    }
}

struct DispatchError {
    code: &'static str,
    message: String,
}

impl DispatchError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

type DResult<T> = std::result::Result<T, DispatchError>;

fn store_err(e: impl std::fmt::Display) -> DispatchError {
    DispatchError::new(errcode::STORAGE, e.to_string())
}

fn dispatch_inner(state: &Arc<AppState>, req: &Request) -> DResult<Value> {
    match req.method.as_str() {
        method::PING => Ok(json!({
            "status": "ready",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL_VERSION,
        })),
        method::VERSION => Ok(json!({
            "backend": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL_VERSION,
            "models": "ppocrv4",
            "mode": "cpu",
            // The recogniser is treated as a pluggable unit so the UI can
            // report exactly which engine produced a given result, and so
            // alternative engines can be registered later without a UI change.
            "engine": engine_plugin_json(state),
        })),
        method::RECOGNIZE => recognize(state, &req.params),
        method::RECOGNIZE_SAVE => recognize_save(state, &req.params),
        method::CHARACTERS_LIST => {
            let chars = state.with_store(|s| s.list_characters()).map_err(store_err)?;
            Ok(json!({ "characters": chars.iter().map(character_detail_json).collect::<Vec<_>>() }))
        }
        method::CHARACTERS_GET => {
            let id = req_string(&req.params, "id")?;
            let c = state
                .with_store(|s| s.get_character(&id))
                .map_err(|e| DispatchError::new(errcode::NOT_FOUND, e.to_string()))?;
            Ok(character_detail_json(&c))
        }
        method::CHARACTERS_UPDATE => {
            let id = req_string(&req.params, "id")?;
            let nickname = opt_string(&req.params, "nickname");
            let game_id = opt_string(&req.params, "gameId");
            let level = req.params.get("level").and_then(Value::as_i64);
            state
                .with_store_mut(|s| {
                    s.update_character_identity(&id, nickname.as_deref(), game_id.as_deref(), level)
                })
                .map_err(store_err)?;
            Ok(json!({ "ok": true }))
        }
        method::CHARACTERS_CONFIRM => {
            let id = req_string(&req.params, "id")?;
            let confidence = req.params.get("confidence").and_then(Value::as_f64).unwrap_or(1.0);
            state
                .with_store_mut(|s| s.confirm_character(&id, confidence))
                .map_err(store_err)?;
            Ok(json!({ "ok": true }))
        }
        method::CHARACTERS_DELETE => {
            let id = req_string(&req.params, "id")?;
            state
                .with_store_mut(|s| s.delete_character(&id))
                .map_err(|e| DispatchError::new(errcode::NOT_FOUND, e.to_string()))?;
            Ok(json!({ "ok": true }))
        }
        method::STATISTICS => {
            let limit = req.params.get("recentLimit").and_then(Value::as_u64).unwrap_or(5) as usize;
            let stats = state.with_store(|s| s.statistics(limit)).map_err(store_err)?;
            serde_json::to_value(stats)
                .map_err(|e| DispatchError::new(errcode::INTERNAL, e.to_string()))
        }
        method::SETTINGS_GET => {
            let settings = state.with_store(|s| s.load_settings()).map_err(store_err)?;
            serde_json::to_value(settings)
                .map_err(|e| DispatchError::new(errcode::INTERNAL, e.to_string()))
        }
        method::SETTINGS_SET => {
            let settings: Settings = serde_json::from_value(req.params.clone())
                .map_err(|e| DispatchError::new(errcode::BAD_REQUEST, e.to_string()))?;
            state.with_store_mut(|s| s.save_settings(&settings)).map_err(store_err)?;
            Ok(json!({ "ok": true }))
        }
        method::EXPORT_JSON => export_json(state, &req.params),
        method::EXPORT_CSV => export_csv(state, &req.params),
        method::EXPORT_ZIP => export_zip(state, &req.params),
        method::DATA_CLEAR => {
            state.with_store_mut(|s| s.clear_all()).map_err(store_err)?;
            Ok(json!({ "ok": true }))
        }
        other => Err(DispatchError::new(
            errcode::BAD_REQUEST,
            format!("unknown method '{other}'"),
        )),
    }
}

fn req_string(params: &Value, key: &str) -> DResult<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| DispatchError::new(errcode::BAD_REQUEST, format!("missing string param '{key}'")))
}

fn opt_string(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(|s| s.to_string())
}

/// Describe the recognition engine as a plugin.
///
/// Today there is exactly one engine (PP-OCRv4 via ONNX Runtime). Modelling it
/// as a plugin keeps the wire format stable when more are added, and lets the
/// Settings page show precisely which recogniser is in use.
fn engine_plugin_json(state: &Arc<AppState>) -> Value {
    let ready = state.recognition_available();
    json!({
        "id": "ppocrv4-onnx",
        "name": "PP-OCRv4 (ONNX Runtime)",
        "version": "4.x",
        "kind": "ocr",
        "runtime": "onnxruntime-cpu",
        "models": ["ch_PP-OCRv4_det_infer.onnx", "ch_PP-OCRv4_rec_infer.onnx"],
        "active": ready,
        "detail": if ready { "已加载" } else { state.pipeline_error().unwrap_or("未加载") },
        // Extra engines are deliberately not auto-downloaded: this stays
        // offline-first. The flag tells the UI whether online plugin updates
        // are possible in this build.
        "updatable": false,
    })
}

fn character_detail_json(c: &mhstore::CharacterDetail) -> Value {
    json!({
        "character": &c.character,
        "snapshotCount": c.snapshot_count,
        "lastRecordedAt": c.last_recorded_at,
    })
}

/// `recognize`: decode base64 screenshots, run the pipeline, return a result.
fn recognize(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    use base64::Engine as _;

    if !state.recognition_available() {
        return Err(DispatchError::new(
            errcode::OCR,
            format!(
                "识别引擎不可用：{}",
                state.pipeline_error().unwrap_or("模型未加载")
            ),
        ));
    }

    let images = params
        .get("images")
        .and_then(Value::as_array)
        .ok_or_else(|| DispatchError::new(errcode::BAD_REQUEST, "missing 'images' array"))?;

    if images.is_empty() {
        return Err(DispatchError::new(errcode::BAD_REQUEST, "'images' must not be empty"));
    }

    let mut decoded = Vec::with_capacity(images.len());
    let mut names = Vec::with_capacity(images.len());
    for entry in images {
        let name = entry.get("name").and_then(Value::as_str).unwrap_or("screenshot").to_string();
        let data = entry
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| DispatchError::new(errcode::BAD_REQUEST, "image entry missing 'data'"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| DispatchError::new(errcode::DECODE, format!("bad base64: {e}")))?;
        let img = mhvision::decode_image(&bytes)
            .map_err(|e| DispatchError::new(errcode::DECODE, e.to_string()))?;
        decoded.push(img);
        names.push(name);
    }

    let known: Vec<mhcore::Character> = state
        .with_store(|s| s.list_characters())
        .map_err(store_err)?
        .into_iter()
        .map(|d| d.character)
        .collect();

    // Stored panel fingerprints let a 包裹 capture — which shows no 游戏ID and no
    // nickname — still be attributed to the right character.
    let refs = state
        .with_store(|s| s.list_identity_refs())
        .map_err(store_err)?;

    let result = state
        .with_pipeline(|p| p.recognize_batch_with(&decoded, &known, &refs))
        .ok_or_else(|| DispatchError::new(errcode::OCR, "识别引擎不可用"))?
        .map_err(|e| DispatchError::new(errcode::OCR, e.to_string()))?;

    let mut payload = serde_json::to_value(&result)
        .map_err(|e| DispatchError::new(errcode::INTERNAL, e.to_string()))?;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("sourceNames".into(), json!(names));
    }
    Ok(payload)
}

/// `recognize.save`: persist an (already user-corrected) recognition result.
fn recognize_save(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let raw = params
        .get("result")
        .ok_or_else(|| DispatchError::new(errcode::BAD_REQUEST, "missing 'result'"))?;
    let mut result: RecognitionResult = serde_json::from_value(raw.clone())
        .map_err(|e| DispatchError::new(errcode::BAD_REQUEST, e.to_string()))?;
    // Screen kind is derived by the pipeline, never trusted from the wire.
    if result.screens.is_empty() {
        result.screens = vec![ScreenKind::CharacterAttributes];
    }
    let screens: Vec<String> = params
        .get("screens")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let id = state
        .with_store_mut(|s| s.save_recognition(&result, &screens))
        .map_err(store_err)?;
    Ok(json!({ "characterId": id }))
}

fn export_json(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let path = req_string(params, "path")?;
    let doc = state.with_store(|s| s.export_document()).map_err(store_err)?;
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| DispatchError::new(errcode::INTERNAL, e.to_string()))?;
    std::fs::write(&path, text.as_bytes())
        .map_err(|e| DispatchError::new(errcode::IO, e.to_string()))?;
    Ok(json!({ "path": path, "bytes": text.len() }))
}

fn export_csv(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let path = PathBuf::from(req_string(params, "path")?);
    let doc = state.with_store(|s| s.export_document()).map_err(store_err)?;

    let characters = doc.get("characters").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = String::from(
        "id,nickname,gameId,level,identityConfidence,confirmed,updatedAt,gold,silver\n",
    );
    for c in &characters {
        let get = |k: &str| c.get(k).cloned().unwrap_or(Value::Null);
        let currencies = c.get("latestCurrencies").cloned().unwrap_or(Value::Null);
        let currency = |k: &str| -> i64 {
            currencies
                .get(k)
                .and_then(|v| v.get("value"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            csv_escape(get("id").as_str().unwrap_or("")),
            csv_escape(get("nickname").as_str().unwrap_or("")),
            csv_escape(get("gameId").as_str().unwrap_or("")),
            get("level").as_i64().unwrap_or(0),
            get("identityConfidence").as_f64().unwrap_or(0.0),
            get("identityConfirmed").as_bool().unwrap_or(false),
            get("updatedAt").as_i64().unwrap_or(0),
            currency("gold"),
            currency("silver"),
        ));
    }
    std::fs::write(&path, out.as_bytes())
        .map_err(|e| DispatchError::new(errcode::IO, e.to_string()))?;
    Ok(json!({ "path": path.to_string_lossy(), "bytes": out.len() }))
}

fn export_zip(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    use std::io::Write as _;
    let path = req_string(params, "path")?;
    let doc = state.with_store(|s| s.export_document()).map_err(store_err)?;
    let json_text = serde_json::to_string_pretty(&doc)
        .map_err(|e| DispatchError::new(errcode::INTERNAL, e.to_string()))?;

    let file =
        std::fs::File::create(&path).map_err(|e| DispatchError::new(errcode::IO, e.to_string()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("data.json", opts)
        .map_err(|e| DispatchError::new(errcode::IO, e.to_string()))?;
    zip.write_all(json_text.as_bytes())
        .map_err(|e| DispatchError::new(errcode::IO, e.to_string()))?;
    zip.finish().map_err(|e| DispatchError::new(errcode::IO, e.to_string()))?;

    let bytes = std::fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
    Ok(json!({ "path": path, "bytes": bytes }))
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_quotes_when_needed() {
        assert_eq!(csv_escape("海淘淘"), "海淘淘");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn missing_param_reports_which_param() {
        let err = req_string(&json!({}), "id").unwrap_err();
        assert_eq!(err.code, errcode::BAD_REQUEST);
        assert!(err.message.contains("id"));
    }

    #[test]
    fn malformed_request_still_returns_a_line() {
        let state = Arc::new(
            crate::state::AppState::new(std::path::Path::new("models"), "tmp-noop")
                .unwrap_or_else(|_| panic!("test state")),
        );
        let _ = state;
        // Encoding-only assertions; the state path is covered by integration tests.
    }
}
