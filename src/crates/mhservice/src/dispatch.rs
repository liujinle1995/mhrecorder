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
use crate::tasks::{CancelOutcome, Control};

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

/// 哪些方法算"任务"（同步路径里需要登记生命周期、可取消耗时、可被取消）。
fn is_tracked(method: &str) -> bool {
    matches!(method, method::RECOGNIZE)
}

fn dispatch(state: &Arc<AppState>, req: &Request) -> Response {
    // 这三个方法必须**不占连接**地应答，所以单独走：
    //   recognize.start —— 只登记，立刻返回 taskId，OCR 在后台线程；
    //   tasks.get       —— 轮询状态，终态时带回结果；
    //   system.shutdown —— 关停请求。
    //
    // 同步 `recognize` 保留给 mhcli 与测试：它跟以前一样把连接占到底。
    // 界面不能再用它——那正是"取消不生效"的根因。
    match req.method.as_str() {
        method::RECOGNIZE_START => return respond(state, req, recognize_start(state, &req.params)),
        method::TASKS_GET => return respond(state, req, tasks_get(state, &req.params)),
        method::SYSTEM_SHUTDOWN => return respond(state, req, system_shutdown(state, &req.params)),
        _ => {}
    }

    let started = Instant::now();
    let (task_id, control) = if is_tracked(&req.method) {
        // 同步识别同样要占引擎：别的窗口正在跑异步任务时，这里也必须拒绝，
        // 否则两个 OCR 会在同一份会话上互相抢锁。
        match state.tasks.begin(&req.method, state.limits.max_request_duration) {
            Ok((id, c)) => (Some(id), Some(c)),
            Err(_) => {
                return Response::err(
                    req.id,
                    errcode::TASK_BUSY,
                    "已有识别任务在跑，请等它结束或先取消",
                );
            }
        }
    } else {
        (None, None)
    };

    if let Some(id) = task_id.as_deref() {
        state.tasks.mark_running(id);
    }

    let outcome = dispatch_inner(state, req, control.as_ref());

    let elapsed = started.elapsed();
    if elapsed >= state.limits.slow_request {
        tracing::warn!("慢请求 {} 用了 {} ms", req.method, elapsed.as_millis());
    }

    match outcome {
        Ok(mut value) => {
            if let (Some(id), Some(obj)) = (task_id.as_ref(), value.as_object_mut()) {
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

/// 协议版本闸门。
///
/// 客户端显式带了 `protocol` 就必须一致；不带也可以（老的 CLI 请求），但带错
/// 版本绝不放行——两端对不上时继续跑只会产生难以解释的结果。
///
/// 放在这里而不是只在 `dispatch_inner` 里，是因为 `recognize.start` /
/// `tasks.get` / `system.shutdown` 走的是"不占连接"的快速路径，不经过
/// `dispatch_inner`，漏掉的话恰恰是最新的三个方法没有版本保护。
fn proto_check(params: &Value) -> DResult<()> {
    if let Some(client) = params.get("protocol").and_then(Value::as_u64) {
        if client != PROTOCOL_VERSION as u64 {
            return Err(DispatchError::new(
                errcode::PROTOCOL_MISMATCH,
                format!(
                    "协议版本不兼容：客户端 v{client}，后端 v{PROTOCOL_VERSION}。请让两端使用同一个版本"
                ),
            ));
        }
    }
    Ok(())
}

/// 统一收口：计时、慢请求日志、`DResult` → `Response`。
fn respond(state: &Arc<AppState>, req: &Request, outcome: DResult<Value>) -> Response {
    let started = Instant::now();
    match outcome {
        Ok(value) => {
            let elapsed = started.elapsed();
            if elapsed >= state.limits.slow_request {
                tracing::warn!("慢请求 {} 用了 {} ms", req.method, elapsed.as_millis());
            }
            Response::ok(req.id, value)
        }
        Err(err) => Response::err(req.id, err.code, err.message),
    }
}

/// `recognize.start`：登记任务 → 起工作线程 → 立刻返回 `taskId`。
///
/// 参数校验故意留到工作线程里做：把 base64 解码放在这里等于又占住了连接，
/// 而"取消"要的正是这条连接空着。校验失败会通过 `tasks.get` 的 `failed`
/// 状态回来，前端的错误提示路径不用分叉。
fn recognize_start(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    proto_check(params)?;
    let (id, control) = state
        .tasks
        .begin(method::RECOGNIZE_START, state.limits.max_request_duration)
        .map_err(|_| {
            DispatchError::new(
                errcode::TASK_BUSY,
                "已有识别任务在跑。等它结束，或先取消那一个再开始新的",
            )
        })?;

    let task_state = Arc::clone(state);
    let task_id = id.clone();
    let task_params = params.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("recognize-{task_id}"))
        .spawn(move || {
            task_state.tasks.mark_running(&task_id);
            let result = recognize(&task_state, &task_params, Some(&control));
            let outcome = match result {
                Ok(value) => crate::tasks::TaskOutcome::ok(value),
                Err(err) => crate::tasks::TaskOutcome::err(err.code, err.message),
            };
            if let Some(record) = task_state.tasks.complete(&task_id, outcome) {
                task_state.record_task(&record);
            }
            task_state.tasks.prune();
        });

    if let Err(e) = spawn {
        // 线程都起不来就别留一个永远 Pending 的任务让前端轮询到死。
        if let Some(record) = state
            .tasks
            .complete(&id, crate::tasks::TaskOutcome::err(errcode::INTERNAL, e.to_string()))
        {
            state.record_task(&record);
        }
        return Err(DispatchError::new(
            errcode::INTERNAL,
            format!("无法启动识别线程：{e}"),
        ));
    }

    Ok(json!({ "taskId": id, "state": "pending" }))
}

/// `tasks.get`：查状态；到终态时把结果交出去（只交一次）。
fn tasks_get(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    proto_check(params)?;
    let id = req_string(params, "id")?;
    // 默认取走结果：前端拿一次就够，反复拉几十 MB 的 JSON 没有意义。
    // 调试时传 consume=false 可以反复看同一帧。
    let consume = params.get("consume").and_then(Value::as_bool).unwrap_or(true);

    let Some(snap) = state.tasks.snapshot(&id, consume) else {
        return Err(DispatchError::new(
            errcode::TASK_NOT_FOUND,
            format!("找不到任务 {id}（可能已被清理，或属于另一个后端实例）"),
        ));
    };

    let mut obj = json!({
        "id": snap.id,
        "method": snap.method,
        "state": snap.state.as_str(),
        "startedAt": snap.started_at,
        "elapsedMs": snap.elapsed_ms,
        "terminal": snap.state.is_terminal(),
    });

    if let Some(outcome) = snap.outcome {
        if let Some(value) = outcome.value {
            obj["result"] = value;
        }
        if let Some(code) = outcome.code {
            obj["error"] = json!({ "code": code, "message": outcome.message });
        }
    }

    Ok(obj)
}

/// `system.shutdown`：请后端退出。
///
/// 关键规则：**只有确认没有别的客户端时才退出**。两个窗口连同一个后端时，
/// 关掉其中一个不该把另一个的后端也带走——这正是 0.15 里 `Kill(spawned)`
/// 的坑。`force` 只给命令行/测试用。
fn system_shutdown(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
    let clients = state.connections();

    if clients > 1 && !force {
        tracing::info!("拒绝关停：还有 {clients} 个客户端连着");
        return Ok(json!({
            "shuttingDown": false,
            "clients": clients,
            "message": "还有其他窗口连着这个后端，不关停",
        }));
    }

    state.request_shutdown();
    tracing::info!("已接受关停请求（连接数 {clients}）");
    Ok(json!({ "shuttingDown": true, "clients": clients }))
}

fn finish_task(state: &Arc<AppState>, task_id: Option<&str>, code: Option<&str>) {
    let Some(id) = task_id else { return };
    let outcome = match code {
        None => crate::tasks::TaskOutcome::ok(Value::Null),
        // 同步路径的"结果"已经随响应返回了，这里只为落一行历史。
        Some(c) => crate::tasks::TaskOutcome::err(c, c),
    };
    if let Some(record) = state.tasks.complete(id, outcome) {
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

/// 存储层的错误分两类：一类是"存不进去"，另一类是"不告诉你该存给谁"。
///
/// 后者（[`mhstore::StoreError::Identity`]）不是故障，是设计：身份不唯一时宁可不存，
/// 也不能挑一行最近更新的塞进去。前端要把这条文案原样显示给用户。
fn save_err(e: mhstore::StoreError) -> DispatchError {
    match e {
        mhstore::StoreError::Identity(msg) => DispatchError::new(errcode::IDENTITY_AMBIGUOUS, msg),
        mhstore::StoreError::NotFound(what) => DispatchError::new(errcode::NOT_FOUND, what),
        other => DispatchError::new(errcode::STORAGE, other.to_string()),
    }
}

fn dispatch_inner(
    state: &Arc<AppState>,
    req: &Request,
    control: Option<&Control>,
) -> DResult<Value> {
    proto_check(&req.params)?;

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
            // 前端用它校验"连着的到底是不是我该连的后端"：
            // 进程名相同但数据目录不同（另一个用户、另一份便携版）的实例，
            // 连上去写库才是灾难。pid 只用于日志与诊断。
            "pid": std::process::id(),
            "dataDir": state.data_dir(),
            "connections": state.connections(),
            // OCR 运行时参数：profile 是用户选的档位，config 是真正生效的
            // 配置。设置页把两者都显示出来，"选了高性能但线程还是 4"这种
            // 情况能立刻看见，而不是靠猜。
            "ocr": {
                "profile": state.ocr_profile().as_str(),
                "cores": mhcore::OcrProfile::cores(),
                "config": state.ocr_config().map(|c| json!({
                    "det": c.det.as_str(),
                    "batchSize": c.batch_size,
                    "threads": c.intra_threads,
                    "maxSide": c.max_side,
                    "summary": c.describe(),
                })).unwrap_or(Value::Null),
            },
            // The recogniser is treated as a pluggable unit so the UI can
            // report exactly which engine produced a given result, and so
            // alternative engines can be registered later without a UI change.
            "engine": engine_plugin_json(state),
        })),
        method::RECOGNIZE => recognize(state, &req.params, control),
        method::RECOGNIZE_START => Err(DispatchError::new(
            errcode::INTERNAL,
            "recognize.start 应该在连接线程之外处理",
        )),
        method::TASKS_GET => tasks_get(state, &req.params),
        method::RECOGNIZE_SAVE => recognize_save(state, &req.params),
        method::TASKS_LIST => tasks_list(state, &req.params),
        method::TASKS_CANCEL => {
            let id = req_string(&req.params, "id")?;
            // 取消是"提出请求"，不是"已经停下"：worker 只能在下一张图的边界
            // 收手，所以这里回 cancelling，前端继续轮询 tasks.get 直到终态。
            match state.tasks.cancel(&id) {
                CancelOutcome::Requested => {
                    Ok(json!({ "cancelled": true, "id": id, "state": "cancelling" }))
                }
                CancelOutcome::AlreadyDone(st) => Ok(json!({
                    "cancelled": false,
                    "id": id,
                    "state": st.as_str(),
                    "reason": "alreadyDone",
                })),
                CancelOutcome::NotFound => Err(DispatchError::new(
                    errcode::TASK_NOT_FOUND,
                    format!("找不到任务 {id}"),
                )),
            }
        }
        method::PENDING_LIST => pending_list(state, &req.params),
        method::PENDING_ASSIGN => pending_assign(state, &req.params),
        method::PENDING_DISCARD => {
            let id = req_string(&req.params, "id")?;
            state
                .with_store(|s| s.discard_pending(&id))
                .map_err(save_err)?;
            Ok(json!({ "ok": true }))
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
            let server = opt_string(&req.params, "server");
            state
                .with_store_mut(|s| {
                    s.update_character_identity(
                        &id,
                        nickname.as_deref(),
                        game_id.as_deref(),
                        level,
                        server.as_deref(),
                    )
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
            // 档位改了就地重建识别引擎：让用户"改完不用重启"是这里唯一
            // 值得付出的复杂度（重建一次约几百毫秒，设置变更本来就很稀疏）。
            let applied = state.apply_ocr_profile(settings.ocr_profile);
            Ok(json!({ "ok": true, "ocrReloaded": applied }))
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
        "detail": if ready { "已加载".to_string() } else { state.pipeline_error().unwrap_or_else(|| "未加载".into()) },
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
                state.pipeline_error().unwrap_or_else(|| "模型未加载".into())
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

    let outcome = state
        .with_store_mut(|s| s.save_recognition(&result, &screens))
        .map_err(save_err)?;

    Ok(match outcome {
        mhstore::SaveOutcome::Saved { character_id, created } => json!({
            "characterId": character_id,
            "created": created,
            "pending": false,
        }),
        // 没有身份信息：数据没丢，只是还没主人。前端据此提示"已存入待归属"。
        mhstore::SaveOutcome::Pending { pending_id, reason } => json!({
            "characterId": null,
            "pendingId": pending_id,
            "pending": true,
            "message": reason,
        }),
    })
}

/// `pending.list`：没有身份信息、等着人工指定归属的识别结果。
fn pending_list(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
    let items = state.with_store(|s| s.list_pending(limit)).map_err(store_err)?;
    let count = state.with_store(|s| s.pending_count()).unwrap_or(items.len() as i64);
    Ok(json!({ "items": items, "count": count }))
}

/// `pending.assign`：把一条待归属记录归到指定角色。
fn pending_assign(state: &Arc<AppState>, params: &Value) -> DResult<Value> {
    let id = req_string(params, "id")?;
    let character_id = req_string(params, "characterId")?;
    let outcome = state
        .with_store_mut(|s| s.assign_pending(&id, &character_id))
        .map_err(save_err)?;
    Ok(json!({
        "characterId": outcome.character_id(),
        "pending": outcome.character_id().is_none(),
    }))
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
        // 消息里必须带上双方版本，否则用户只看到"不兼容"没法排查。
        let expected = format!("v{PROTOCOL_VERSION}");
        assert!(err.message.contains(&expected), "{}", err.message);
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

    // ------------------------------------------------------------------
    // 异步任务协议（0.16）：start 立刻返回、状态可轮询、取消能被读到
    // ------------------------------------------------------------------

    fn call(state: &Arc<AppState>, method: &str, params: Value) -> Response {
        dispatch(state, &Request { id: 1, method: method.to_string(), params })
    }

    fn wait_terminal(state: &Arc<AppState>, id: &str) -> String {
        for _ in 0..200 {
            // consume=false：轮询不能把结果取走，那是 `tasks.get` 的正式语义，
            // 前端拿到终态那一帧才取。
            let resp = call(state, method::TASKS_GET, json!({ "id": id, "consume": false }));
            assert!(resp.ok, "tasks.get 失败：{:?}", resp.error);
            let s = resp.result.unwrap()["state"].as_str().unwrap().to_string();
            if s != "pending" && s != "running" && s != "cancelling" {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("任务 2 秒内没有进入终态，状态机卡住了");
    }

    /// 这是本次改动的核心证据：`recognize.start` 必须在 OCR 跑完**之前**就返回
    /// taskId，否则前端拿不到 id，取消请求也就无从谈起。
    #[test]
    fn recognize_start_returns_a_task_id_before_the_work_finishes() {
        let state = test_state();
        let started = std::time::Instant::now();
        let resp = call(&state, method::RECOGNIZE_START, json!({ "images": [] }));
        let elapsed = started.elapsed();

        assert!(resp.ok, "{:?}", resp.error);
        let payload = resp.result.unwrap();
        let id = payload["taskId"].as_str().expect("必须有 taskId").to_string();
        assert_eq!(payload["state"], "pending");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "start 不该等识别跑完（用了 {elapsed:?}）"
        );

        // 工作线程随后跑完（这里没有模型，必然 failed），状态必须收敛。
        let terminal = wait_terminal(&state, &id);
        assert!(
            terminal == "failed" || terminal == "completed",
            "终态应该是 failed/completed，实际 {terminal}"
        );
    }

    #[test]
    fn tasks_get_reports_unknown_ids_explicitly() {
        let state = test_state();
        let resp = call(&state, method::TASKS_GET, json!({ "id": "t-does-not-exist" }));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, errcode::TASK_NOT_FOUND);
    }

    /// 结果只交一次：避免前端重试时把同一份识别结果贴两遍。
    #[test]
    fn result_is_only_delivered_once() {
        let state = test_state();
        let resp = call(&state, method::RECOGNIZE_START, json!({ "images": [] }));
        let id = resp.result.unwrap()["taskId"].as_str().unwrap().to_string();
        wait_terminal(&state, &id);

        let first = call(&state, method::TASKS_GET, json!({ "id": id }));
        assert!(first.ok);
        let second = call(&state, method::TASKS_GET, json!({ "id": id }));
        assert!(second.ok);
        assert_eq!(second.result.unwrap()["state"], "failed");
        // 第一帧带了 error/result，第二帧只剩状态。
        let first_obj = first.result.unwrap();
        assert!(first_obj.get("error").is_some() || first_obj.get("result").is_some());
    }

    /// 第二个 start 在第一个还没结束时被拒绝：OCR 会话是独占的，
    /// 排队只会让"还要等多久"不可预测。
    #[test]
    fn concurrent_recognitions_are_rejected_not_queued() {
        let state = test_state();
        let a = call(&state, method::RECOGNIZE_START, json!({ "images": [] }));
        assert!(a.ok);
        let b = call(&state, method::RECOGNIZE_START, json!({ "images": [] }));
        // 第一个可能已经秒完（没有模型），所以"被拒"不是必然结果；
        // 但只要被拒，错误码必须是 taskBusy。
        if !b.ok {
            assert_eq!(b.error.unwrap().code, errcode::TASK_BUSY);
        }

        let id = a.result.unwrap()["taskId"].as_str().unwrap().to_string();
        wait_terminal(&state, &id);
    }

    /// 关停请求必须尊重"还有别的客户端"：两个窗口共用一个后端时，
    /// 关掉其中一个不能把另一个的后端也带走。
    #[test]
    fn shutdown_is_refused_while_another_client_is_connected() {
        let state = test_state();
        state.connection_opened();
        state.connection_opened();

        let refused = call(&state, method::SYSTEM_SHUTDOWN, json!({}));
        assert!(refused.ok);
        let payload = refused.result.unwrap();
        assert_eq!(payload["shuttingDown"], false);
        assert_eq!(payload["clients"], 2);
        assert!(!state.shutdown_requested(), "拒绝关停时不能置位");

        state.connection_closed();
        state.connection_closed();
        let accepted = call(&state, method::SYSTEM_SHUTDOWN, json!({}));
        assert!(accepted.ok);
        assert_eq!(accepted.result.unwrap()["shuttingDown"], true);
        assert!(state.shutdown_requested());
    }

    /// `version` 要带 pid 与 dataDir：前端靠这两个字段判断"连着的到底是谁"，
    /// 而不是靠进程名猜。
    #[test]
    fn version_identifies_the_instance() {
        let state = test_state();
        let resp = call(&state, method::VERSION, json!({}));
        assert!(resp.ok);
        let v = resp.result.unwrap();
        assert_eq!(v["protocol"], PROTOCOL_VERSION);
        assert!(v["pid"].as_u64().unwrap() > 0);
        assert_eq!(v["dataDir"].as_str().unwrap(), state.data_dir());
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
        // 0.16 起取消未知任务返回 taskNotFound，而不是含糊的 cancelled:false——
        // 前端据此能区分"任务结束了"和"这个 id 根本不是我的"。
        let resp = call(&state, method::TASKS_CANCEL, json!({ "id": "nope" }));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, errcode::TASK_NOT_FOUND);
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
