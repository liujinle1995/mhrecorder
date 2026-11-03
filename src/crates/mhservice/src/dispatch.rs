//! 请求分发：把协议方法名接到 vision + store 上。
//!
//! 不含任何 Win32 代码，所以可以直接喂 JSON 行做单元测试。
//!
//! 这一层同时是**所有边界的把关点**：协议版本、图片数量、像素数、解码字节数
//! 都在这里检查，识别入口只有一条，漏检的地方会立刻暴露成测试失败。

use mhcore::protocol::{errcode, method, Request, Response, PROTOCOL_VERSION};
use mhcore::{RecognitionResult, ScreenKind, Settings};
use serde_json::{json, Value};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::state::AppState;
use crate::tasks::Control;

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

/// 哪些方法算"任务"（需要登记生命周期、可取消耗时、可被取消）。
fn is_tracked(method: &str) -> bool {
    matches!(method, method::RECOGNIZE)
}

fn dispatch(state: &Arc<AppState>, req: &Request) -> Response {
    let started = Instant::now();
    let (task_id, control) = if is_tracked(&req.method) {
        let (id, c) = state.tasks.begin(&req.method, state.limits.max_request_duration);
        (Some(id), Some(c))
    } else {
        (None, None)
    };

    let outcome = dispatch_inner(state, req, control.as_ref());

    let elapsed = started.elapsed();
    if elapsed >= state.limits.slow_request {
        tracing::warn!(
            "慢请求 {} 用了 {} ms",
            req.method,
            elapsed.as_millis()
        );
    }

    match outcome {
        Ok(mut value) => {
            if let (Some(id), Some(obj)) = (task_id.as_ref(), value.as_object_mut()) {
                // 客户端拿到 taskId 就可以在用户点"取消"时调 tasks.cancel。
                obj.insert("taskId".into(), json!(id));
            }
            finish_task(state, task_id.as_deref(), None);
            Response::ok(req.id, value)
        }
        Err(err) => {
            finish_task(state, task_id.as_deref(), Some(err.code));
            Response::err(req.id, err.code, err.message)
        }
    }
}

fn finish_task(state: &Arc<AppState>, task_id: Option<&str>, code: Option<&str>) {
    let Some(id) = task_id else { return };
    if let Some(record) = state.tasks.finish(id, code) {
        state.record_task(&record);
    }
    state.tasks.prune();
}

#[derive(Debug)]
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

fn dispatch_inner(
    state: &Arc<AppState>,
    req: &Request,
    control: Option<&Control>,
) -> DResult<Value> {
    // 协议版本：客户端显式带了就必须一致。不带也可以（老的 CLI 请求），
    // 但带错版本绝不放行——两端对不上时继续跑只会产生难以解释的结果。
    if let Some(client) = req.params.get("protocol").and_then(Value::as_u64) {
        if client != PROTOCOL_VERSION as u64 {
            return Err(DispatchError::new(
                errcode::PROTOCOL_MISMATCH,
                format!(
                    "协议版本不兼容：客户端 v{client}，后端 v{PROTOCOL_VERSION}。请让两端使用同一个版本"
                ),
            ));
        }
    }

    match req.method.as_str() {
        method::PING => Ok(json!({
            "status": "ready",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL_VERSION,
            "schemaVersion": state.with_store(|s| s.schema_version().ok()),
            "algorithmVersion": mhvision::ANALYSIS_VERSION,
            "limits": {
                "maxMessageBytes": state.limits.max_message_bytes,
                "maxImages": state.limits.max_images,
                "maxImagePixels": state.limits.max_image_pixels,
                "maxImageBytes": state.limits.max_image_bytes,
            },
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
        method::RECOGNIZE => recognize(state, &req.params, control),
        method::RECOGNIZE_SAVE => recognize_save(state, &req.params),
        method::TASKS_LIST => tasks_list(state, &req.params),
        method::TASKS_CANCEL => {
            let id = req_string(&req.params, "id")?;
            let cancelled = state.tasks.cancel(&id);
            Ok(json!({ "cancelled": cancelled, "id": id }))
        }
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
        method::EXPORT_BACKUP => export_backup(state, &req.params),
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
///
/// 三道闸门，逐级收紧（先便宜的检查，再花钱的解码）：
/// 1. 张数 ≤ `max_images`；
/// 2. base64 估算字节 ≤ `max_image_bytes`（不做解码，避免为超限数据分配）；
/// 3. 图片头里的像素数 ≤ `max_image_pixels`（防解压炸弹：几百 KB 的 PNG
///    可以解出 2 亿像素）。
fn recognize(
    state: &Arc<AppState>,
    params: &Value,
    control: Option<&Control>,
) -> DResult<Value> {
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

    let shots = decode_shots(&state.limits, images)?;
    let mut names = Vec::with_capacity(shots.len());
    let mut decoded = Vec::with_capacity(shots.len());
    for shot in &shots {
        let img = mhvision::decode_image(&shot.bytes)
            .map_err(|e| DispatchError::new(errcode::DECODE, e.to_string()))?;
        names.push(shot.name.clone());
        decoded.push(img);
    }

    // 图片字节的 SHA-256 是缓存键：同一张图无论隔多久、从哪条路径导入，
    // 都落到同一条缓存上。算法版本进键前缀，升级后老条目自动不用。
    let cache_keys: Vec<String> = shots
        .iter()
        .map(|shot| {
            mhstore::cache::feature_key(
                mhvision::ANALYSIS_VERSION,
                &mhvision::content_hash(&shot.bytes),
            )
        })
        .collect();

    // 读缓存放在流水线锁之外：一次拿全，别在识别过程中反复抢存储锁。
    let cached: Vec<Option<mhvision::ImageAnalysis>> = state.with_store(|s| {
        cache_keys
            .iter()
            .map(|key| match s.get_image_feature(key) {
                Ok(Some(payload)) => mhvision::ImageAnalysis::decode(&payload),
                Ok(None) => None,
                Err(e) => {
                    // 缓存读失败只是没省下时间，不该让这次识别失败。
                    tracing::warn!("feature cache lookup failed: {e}");
                    None
                }
            })
            .collect()
    });
    let hits_before = cached.iter().filter(|c| c.is_some()).count();
    if hits_before > 0 {
        tracing::info!("feature cache: {hits_before}/{} hit", cached.len());
    }

    let known: Vec<mhcore::Character> = state
        .with_store(|s| s.list_characters())
        .map_err(store_err)?
        .into_iter()
        .map(|d| d.character)
        .collect();

    // Stored panel fingerprints let a 包裹 capture — which shows no 游戏ID and no
    // nickname — still be attributed to the right character.
    let refs = state.with_store(|s| s.list_identity_refs()).map_err(store_err)?;

    let outcome = state
        .with_pipeline(|p| match control {
            Some(c) => p.recognize_batch_cached(&decoded, &cached, &known, &refs, c),
            None => p.recognize_batch_cached(&decoded, &cached, &known, &refs, &mhvision::NoControl),
        })
        .ok_or_else(|| DispatchError::new(errcode::OCR, "识别引擎不可用"))?
        .map_err(|e| match e {
            mhvision::VisionError::Cancelled => {
                DispatchError::new(errcode::CANCELLED, "识别已取消")
            }
            mhvision::VisionError::Timeout => DispatchError::new(
                errcode::TIMEOUT,
                format!(
                    "识别超过 {} 秒仍未完成，已中止；请减少一次提交的截图数量",
                    state.limits.max_request_duration.as_secs()
                ),
            ),
            other => DispatchError::new(errcode::OCR, other.to_string()),
        })?;

    let mut result = outcome.result;
    // 写回本次算出来的分析。写失败只记日志：缓存是优化，不该让识别结果作废。
    if !outcome.computed.is_empty() {
        state.with_store_mut(|s| {
            let mut written = 0;
            for (index, analysis) in &outcome.computed {
                let Some(key) = cache_keys.get(*index) else { continue };
                match analysis.encode() {
                    Ok(payload) => match s.put_image_feature(key, &payload) {
                        Ok(()) => written += 1,
                        Err(e) => tracing::warn!("feature cache write failed: {e}"),
                    },
                    Err(e) => tracing::warn!("feature cache encode failed: {e}"),
                }
            }
            if written > 0 {
                if let Err(e) = s.prune_image_features(mhstore::cache::MAX_IMAGE_FEATURES) {
                    tracing::warn!("feature cache prune failed: {e}");
                }
            }
        });
    }
    // 只报告**真正用上**的条目：命中但被 dHash 复核否掉的会重新 OCR，
    // 这时说"缓存命中"是在撒谎。
    result.cache_hits = outcome
        .cached_screens
        .iter()
        .filter_map(|index| cache_keys.get(*index).cloned())
        .collect();

    let mut payload = serde_json::to_value(&result)
        .map_err(|e| DispatchError::new(errcode::INTERNAL, e.to_string()))?;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("sourceNames".into(), json!(names));
    }
    Ok(payload)
}

/// 一张通过校验、但还没解出像素的截图。
#[derive(Debug)]
struct Shot {
    name: String,
    bytes: Vec<u8>,
}

/// 逐张校验并解码 base64，**不**解像素。
///
/// 检查从便宜到贵：张数 → base64 估算字节（不解码、不分配）→ 真实字节 →
/// 图片头里的像素数。最后一项防的是解压炸弹：几百 KB 的 PNG 能解出两亿
/// 像素，足够把 32 位进程的内存吃光。
fn decode_shots(limits: &crate::limits::Limits, images: &[Value]) -> DResult<Vec<Shot>> {
    use base64::Engine as _;

    if images.is_empty() {
        return Err(DispatchError::new(errcode::BAD_REQUEST, "'images' must not be empty"));
    }
    if images.len() > limits.max_images {
        return Err(DispatchError::new(
            errcode::TOO_LARGE,
            format!("一次最多 {} 张截图，收到 {} 张", limits.max_images, images.len()),
        ));
    }

    let mut out = Vec::with_capacity(images.len());
    for (index, entry) in images.iter().enumerate() {
        let name = entry.get("name").and_then(Value::as_str).unwrap_or("screenshot").to_string();
        let data = entry
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| DispatchError::new(errcode::BAD_REQUEST, "image entry missing 'data'"))?;

        // base64 是 4 字符 → 3 字节，先估再解。
        let approx_bytes = (data.len() as u64) * 3 / 4;
        if approx_bytes > limits.max_image_bytes {
            return Err(DispatchError::new(
                errcode::TOO_LARGE,
                format!(
                    "第 {} 张截图约 {:.1} MB，超过 {:.0} MB 上限",
                    index + 1,
                    approx_bytes as f64 / 1_048_576.0,
                    limits.max_image_bytes as f64 / 1_048_576.0
                ),
            ));
        }

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| DispatchError::new(errcode::DECODE, format!("bad base64: {e}")))?;
        if bytes.len() as u64 > limits.max_image_bytes {
            return Err(DispatchError::new(
                errcode::TOO_LARGE,
                format!("第 {} 张截图过大：{} 字节", index + 1, bytes.len()),
            ));
        }

        let (w, h) = image_dimensions(&bytes).map_err(|e| {
            DispatchError::new(errcode::DECODE, format!("第 {} 张截图无法解码：{e}", index + 1))
        })?;
        let pixels = w as u64 * h as u64;
        if pixels > limits.max_image_pixels {
            return Err(DispatchError::new(
                errcode::TOO_LARGE,
                format!(
                    "第 {} 张截图 {w}×{h} = {:.1} MP，超过 {:.0} MP 上限",
                    index + 1,
                    pixels as f64 / 1_000_000.0,
                    limits.max_image_pixels as f64 / 1_000_000.0
                ),
            ));
        }

        out.push(Shot { name, bytes });
    }
    Ok(out)
}

/// 只读图片头拿宽高。
fn image_dimensions(bytes: &[u8]) -> std::result::Result<(u32, u32), String> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.into_dimensions().map_err(|e| e.to_string())
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
    if result.screenshot_count == 0 {
        result.screenshot_count = 1;
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

/// 任务：正在跑的和最近跑过的。
fn tasks_list(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    let running = state.tasks.running();
    let history = state.with_store(|s| s.list_tasks(limit)).map_err(store_err)?;
    Ok(json!({ "running": running, "history": history }))
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
        let currency = |k: &str| -> Option<i64> {
            currencies.get(k).and_then(|v| v.get("value")).and_then(Value::as_i64)
        };
        // 未知写成空字段而不是 0：CSV 里 0 会被当成真实读数汇总。
        let field = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_default();
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            csv_escape(get("id").as_str().unwrap_or("")),
            csv_escape(get("nickname").as_str().unwrap_or("")),
            csv_escape(get("gameId").as_str().unwrap_or("")),
            match get("level") {
                v if v.is_null() => String::new(),
                v => v.to_string(),
            },
            get("identityConfidence").as_f64().unwrap_or(0.0),
            get("identityConfirmed").as_bool().unwrap_or(false),
            get("updatedAt").as_i64().unwrap_or(0),
            field(currency("gold")),
            field(currency("silver")),
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

/// `export.backup`: JSON + 数据库文件 + 清单，一次可完整恢复。
fn export_backup(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let path = req_string(params, "path")?;
    let include_db = params
        .get("includeDatabase")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let summary = state
        .with_store(|s| s.export_backup(std::path::Path::new(&path), include_db))
        .map_err(store_err)?;
    serde_json::to_value(summary).map_err(|e| DispatchError::new(errcode::INTERNAL, e.to_string()))
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

    /// 图片头解析必须先于解码：一个几百 KB 的 PNG 可以是 2 亿像素。
    #[test]
    fn dimensions_come_from_the_header() {
        let img = image::RgbaImage::from_pixel(640, 480, image::Rgba([1, 2, 3, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(image_dimensions(&png).unwrap(), (640, 480));

        // 不是图片的数据要报错而不是 panic。
        assert!(image_dimensions(b"not an image").is_err());
    }

    #[test]
    fn protocol_mismatch_is_rejected_before_any_work() {
        let err = proto_gate(PROTOCOL_VERSION as u64 + 1).unwrap_err();
        assert_eq!(err.code, errcode::PROTOCOL_MISMATCH);
        assert!(err.message.contains("v1"), "{}", err.message);
        assert!(proto_gate(PROTOCOL_VERSION as u64).is_ok());
        assert!(proto_gate_no_param().is_ok(), "不带 protocol 的旧请求仍然放行");
    }

    fn proto_gate(client: u64) -> DResult<()> {
        let req = Request {
            id: 1,
            method: method::PING.into(),
            params: json!({ "protocol": client }),
        };
        let state = test_state();
        dispatch_inner(&state, &req, None).map(|_| ())
    }

    fn proto_gate_no_param() -> DResult<()> {
        let req = Request { id: 1, method: method::PING.into(), params: json!({}) };
        let state = test_state();
        dispatch_inner(&state, &req, None).map(|_| ())
    }

    fn test_state() -> Arc<AppState> {
        let dir = std::env::temp_dir().join(format!("mhservice-dispatch-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        // models 目录不存在也没关系：存储类方法照常可用。
        AppState::with_limits(
            std::path::Path::new("models-does-not-exist"),
            dir.to_str().unwrap(),
            crate::limits::Limits::default(),
        )
        .unwrap()
    }

    fn uuid_like() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    fn limits() -> crate::limits::Limits {
        crate::limits::Limits::default()
    }

    fn shot(w: u32, h: u32) -> Value {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([9, 9, 9, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        use base64::Engine as _;
        json!({
            "name": format!("{w}x{h}.png"),
            "data": base64::engine::general_purpose::STANDARD.encode(&png),
        })
    }

    /// 超过张数上限时，一条都不解码就地拒绝。
    #[test]
    fn too_many_images_is_rejected_up_front() {
        let l = limits();
        let images: Vec<Value> = (0..l.max_images + 1).map(|_| json!({ "data": "" })).collect();
        let err = decode_shots(&l, &images).unwrap_err();
        assert_eq!(err.code, errcode::TOO_LARGE);
        assert!(err.message.contains(&l.max_images.to_string()), "{}", err.message);
    }

    /// base64 估算超限：在解码之前就拒绝（不为超限数据分配内存）。
    #[test]
    fn oversized_payload_is_refused_before_decoding() {
        let mut l = limits();
        l.max_image_bytes = 1024;
        let huge = json!({ "name": "huge.png", "data": "A".repeat(4096) });
        let err = decode_shots(&l, &[huge]).unwrap_err();
        assert_eq!(err.code, errcode::TOO_LARGE);
        assert!(err.message.contains("MB"), "{}", err.message);
    }

    /// 解压炸弹：文件很小，但声明的像素数远超上限。
    #[test]
    fn decompression_bomb_is_refused_by_header_pixels() {
        let mut l = limits();
        l.max_image_pixels = 1_000;
        let err = decode_shots(&l, &[shot(64, 64)]).unwrap_err();
        assert_eq!(err.code, errcode::TOO_LARGE);
        assert!(err.message.contains("MP"), "{}", err.message);
    }

    /// 正常尺寸的截图通过，名字原样带出。
    #[test]
    fn normal_shot_passes_and_keeps_its_name() {
        let shots = decode_shots(&limits(), &[shot(320, 200)]).unwrap();
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].name, "320x200.png");
        assert!(!shots[0].bytes.is_empty());
    }

    #[test]
    fn empty_image_array_is_bad_request() {
        let err = decode_shots(&limits(), &[]).unwrap_err();
        assert_eq!(err.code, errcode::BAD_REQUEST);
    }

    #[test]
    fn entry_without_data_is_bad_request() {
        let err = decode_shots(&limits(), &[json!({ "name": "x.png" })]).unwrap_err();
        assert_eq!(err.code, errcode::BAD_REQUEST);
    }

    #[test]
    fn tasks_cancel_reports_unknown_ids() {
        let state = test_state();
        let req = Request {
            id: 1,
            method: method::TASKS_CANCEL.into(),
            params: json!({ "id": "nope" }),
        };
        let out = dispatch_inner(&state, &req, None).unwrap();
        assert_eq!(out["cancelled"], false);
    }

    #[test]
    fn tasks_list_starts_empty() {
        let state = test_state();
        let req = Request { id: 1, method: method::TASKS_LIST.into(), params: json!({}) };
        let out = dispatch_inner(&state, &req, None).unwrap();
        assert_eq!(out["running"].as_array().unwrap().len(), 0);
        assert!(out["history"].is_array());
    }
}
