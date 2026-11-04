#![cfg_attr(windows, windows_subsystem = "windows")]

//! mhservice — long-lived backend process.
//!
//! Listens on the `mhrecorder.v1` named pipe and serves the JSON-RPC protocol.
//! Emits `MHREADY` on stdout once listening (best effort: stdout does not exist
//! when the process is detached, so the launcher polls the pipe instead).
//!
//! The binary is meant to be launchable by double-clicking: `build.rs` copies
//! the vendored native DLLs beside it, and `pin_runtime_dylib` below makes sure
//! `ort` loads OUR onnxruntime rather than the ancient 1.17 copy in System32.
//!
//! **No console, ever, unless asked.** See `main` for the 0.9.2 postmortem.

mod dispatch;
mod limits;
mod pipe;
mod state;
mod tasks;
mod winpipe;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Directory the running executable lives in.
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Force `ort` to load the onnxruntime shipped beside this executable.
///
/// `C:\Windows\System32` contains an onnxruntime.dll v1.17 that the dynamic
/// loader would happily pick up, but `ort` requires >= 1.31 and fails hard on
/// it. `ORT_DYLIB_PATH` wins over the loader search, so point it at our copy
/// when we can find one and the user has not already chosen a path.
fn pin_runtime_dylib() {
    static PINNED: OnceLock<()> = OnceLock::new();
    PINNED.get_or_init(|| {
        // An explicit user override always wins.
        if std::env::var_os("ORT_DYLIB_PATH").is_some() {
            return;
        }
        let Some(dir) = exe_dir() else { return };
        let candidate = dir.join("onnxruntime.dll");
        if candidate.exists() {
            // Safe: single-threaded here, before any ort code runs.
            std::env::set_var("ORT_DYLIB_PATH", &candidate);
            tracing::debug!("pinned ORT_DYLIB_PATH to {}", candidate.display());
        }
    });
}

/// Resolve the models directory: env override, else `models` beside the exe.
fn resolve_models_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MH_MODELS_DIR") {
        return PathBuf::from(p);
    }
    if let Some(dir) = exe_dir() {
        let beside = dir.join("models");
        if beside.exists() {
            return beside;
        }
    }
    PathBuf::from("models")
}

/// 同一数据目录只允许一个后端。
///
/// 做法是"共享模式 0"的独占 open：**看的不是文件内容，而是句柄**。第二个
/// 进程打开会拿到 ERROR_SHARING_VIOLATION(32)，进程退出后锁自动释放，不会
/// 留下需要清理的"僵尸锁"。
///
/// 为什么不按进程名判断：`Process.GetProcessesByName("mhservice")` 会把别的
/// 目录、别的用户会话、甚至另一份便携版的同名进程都算进来，然后据此做出
/// "后端已经在跑"或"把它杀掉"的决定——两个判断都可能是错的。
fn acquire_instance_lock(data_dir: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = std::path::Path::new(data_dir);
    std::fs::create_dir_all(dir)?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(dir.join("mhservice.lock"))
}

/// 生命周期看门狗：后端自己决定什么时候退出。
///
/// 两个触发条件：
/// * `system.shutdown`（前端关窗口时发）——立刻走，但要等 200 ms 让响应先写回；
/// * 一个客户端都没有、且已经空闲超过宽限期——兜底，前端崩溃/被强杀时用。
///
/// 前端因此不需要知道"是谁拉起的后端"，更不需要杀进程。
fn spawn_lifecycle_watchdog(state: Arc<state::AppState>) {
    let spawned = std::thread::Builder::new()
        .name("lifecycle".into())
        .spawn(move || {
            let idle_after = state.idle_exit_after();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(200));

                if state.shutdown_requested() {
                    tracing::info!("收到关停请求，正在退出");
                    state.tasks.cancel_all();
                    // 让 system.shutdown 的响应先写回管道再退出。
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    break;
                }

                if state.has_served() && state.connections() == 0 {
                    let idle = state.idle_ms();
                    if idle >= idle_after.as_millis() as i64 {
                        tracing::info!(
                            "已经 {} ms 没有客户端，后端自行退出",
                            idle
                        );
                        state.tasks.cancel_all();
                        break;
                    }
                }
            }

            let _ = std::io::Write::flush(&mut std::io::stdout());
            std::process::exit(0);
        });

    if let Err(e) = spawned {
        tracing::warn!("lifecycle watchdog 启动失败（后端将不会自动退出）：{e}");
    }
}

fn main() {
    // The backend runs detached from the launcher, so it is a GUI-subsystem
    // binary and never allocates a console of its own.
    //
    // Attaching to the *parent* console unconditionally was a mistake: when the
    // launcher still had one, `AttachConsole(ATTACH_PARENT_PROCESS)` hooked into
    // that window and every log line kept it visible for the whole session — the
    // persistent black window users reported. Console output is only wanted when
    // someone is actually looking at it: `--stdio` (mhcli / integration tests) or
    // an explicit MH_LOG_CONSOLE=1 from a terminal. Everything else goes to a log
    // file beside the data directory.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdio_mode = args.iter().any(|a| a == "--stdio");
    let wants_console = stdio_mode || std::env::var_os("MH_LOG_CONSOLE").is_some();
    let has_console = wants_console && win_console::attach_parent_console();

    // Build the subscriber once; only the writer differs between the
    // console/no-console cases, so use a single boxed writer object.
    let writer: Box<dyn std::io::Write + Send + Sync> = if has_console {
        Box::new(std::io::stderr())
    } else {
        match std::fs::File::create(log_file_path()) {
            Ok(file) => Box::new(file),
            Err(_) => Box::new(std::io::sink()),
        }
    };
    // `MakeWriter` is implemented for anything implementing `Write`, so a
    // mutex-wrapped boxed writer keeps the layer type stable.
    let writer = std::sync::Mutex::new(writer);

    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer);
    let _ = tracing_subscriber::registry()
        .with(env_filter())
        .with(layer)
        .try_init();

    // Must run before anything touches ONNX Runtime.
    pin_runtime_dylib();

    let models_dir = resolve_models_dir();

    let data_dir = std::env::var("MH_DATA_DIR").unwrap_or_else(|_| mhcore::default_data_dir());

    // Optional: serve only one request from stdin JSON, then exit. Used by the
    // CLI and by integration tests that don't want a long-lived server.
    if stdio_mode {
        if let Err(e) = run_stdio(&models_dir, &data_dir, limits::Limits::from_env()) {
            emit(true, &format!("mhservice --stdio failed: {e:#}"));
            std::process::exit(1);
        }
        return;
    }

    // 同一数据目录只允许一个后端实例。必须在 open 数据库之前拿到，
    // 否则第二个实例会先打开 SQLite、再发现抢不到锁，日志里留下一串
    // "database is locked" 的噪声。
    let _instance_lock = match acquire_instance_lock(&data_dir) {
        Ok(f) => f,
        Err(e) => {
            emit(
                true,
                &format!("另一个后端已经在服务数据目录 {data_dir}，本实例退出：{e}"),
            );
            std::process::exit(3);
        }
    };

    let state = match state::AppState::new(&models_dir, &data_dir) {
        Ok(s) => s,
        Err(e) => {
            emit(true, &format!("failed to initialise backend: {e:#}"));
            std::process::exit(1);
        }
    };

    tracing::info!("models dir: {}", models_dir.display());
    tracing::info!("data dir:   {data_dir}");
    tracing::info!("listening on pipe {}", pipe::PIPE_NAME);

    spawn_lifecycle_watchdog(Arc::clone(&state));

    // Signal readiness to the parent process before blocking on accept. Written
    // through `emit` because stdout does not exist in the detached case and a
    // panicking `println!` would abort the backend on start-up.
    emit(false, "MHREADY");

    let shared: Arc<state::AppState> = state;
    if let Err(e) = winpipe::run_server(pipe::PIPE_NAME, Arc::clone(&shared)) {
        // 关停前把还在跑的识别任务标记为取消：它们占着 OCR 会话，让进程
        // 干等不如明确放弃。
        shared.tasks.cancel_all();
        emit(true, &format!("pipe server failed: {e}"));
        std::process::exit(1);
    }
}

/// Write one line to stdout (`to_stderr == false`) or stderr without panicking.
///
/// Both handles are invalid when the backend is started detached, and the
/// `println!`/`eprintln!` macros panic when the write fails — that would kill a
/// long-lived service over a log line nobody was going to read.
fn emit(to_stderr: bool, line: &str) {
    use std::io::Write as _;
    if to_stderr {
        let _ = writeln!(std::io::stderr(), "{line}");
    } else {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// Shared logging filter; `ort` is noisy at info level.
fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ort=warn"))
}

/// Fallback log file when running without a console (normal double-click case).
fn log_file_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("MH_DATA_DIR").map(std::path::PathBuf::from) {
        let _ = std::fs::create_dir_all(&dir);
        return dir.join("mhservice.log");
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("MhRecorder");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("mhservice.log")
}

/// Console attach helper for the GUI-subsystem backend.
#[cfg(windows)]
mod win_console {
    use std::ffi::c_void;

    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    type Handle = *mut c_void;
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;

    /// True when a parent console exists and was successfully attached.
    pub fn attach_parent_console() -> bool {
        let _ = std::ptr::null_mut::<Handle>();
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
    }
}

#[cfg(not(windows))]
mod win_console {
    pub fn attach_parent_console() -> bool {
        true
    }
}

/// Read request lines from stdin, write responses to stdout.
///
/// 走和管道同一个 `serve_stream`：这样单行上限、超限处理、非法 UTF-8 的行为
/// 在 `--stdio` 和管道两种入口下完全一致，不会出现"CLI 能过、界面炸"的分叉。
fn run_stdio(
    models_dir: &std::path::Path,
    data_dir: &str,
    limits: limits::Limits,
) -> anyhow::Result<()> {
    let state = state::AppState::with_limits(models_dir, data_dir, limits.clone())?;
    let opts = pipe::ServeOptions {
        max_line_bytes: limits.max_message_bytes,
        close_on_oversize: true,
    };
    pipe::serve_stream(
        std::io::stdin(),
        std::io::stdout(),
        opts,
        |line| Some(dispatch::handle_line(&state, line)),
    )?;
    Ok(())
}

