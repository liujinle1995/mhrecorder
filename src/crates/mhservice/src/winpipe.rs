//! Win32 命名管道服务端。
//!
//! 创建 `\\.\pipe\mhrecorder.v1`，每个连接一个线程跑行协议。直接用
//! `windows-sys`，不引 async runtime。
//!
//! 连接数有硬上限：每个连接背后是一个线程、一份缓冲区，外加一份对 OCR
//! 流水线的排队权。超过上限时**接上就拒**，回一条明确错误再断开，而不是让
//! 一堆线程堆在锁上把响应时间拖垮。

#![cfg(windows)]

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use mhcore::protocol::{errcode, Response};

use crate::state::AppState;

/// Wrapper so the raw handle can be moved into a thread and closed on drop.
struct PipeHandle(HANDLE);

unsafe impl Send for PipeHandle {}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE && !self.0.is_null() {
            unsafe {
                let _ = DisconnectNamedPipe(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// 连接计数：进线程时 +1，线程结束 -1。
struct ConnGuard(Arc<AtomicUsize>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Build a UTF-16, NUL-terminated pipe path ("\\\\.\\pipe\\<name>").
fn wide_pipe_path(name: &str) -> Vec<u16> {
    let mut s: Vec<u16> = format!("\\\\.\\pipe\\{name}").encode_utf16().collect();
    s.push(0);
    s
}

/// Block forever accepting clients on `name`, servicing each on its own thread.
pub fn run_server(name: &str, state: Arc<AppState>) -> std::io::Result<()> {
    let path = wide_pipe_path(name);
    let max_connections = state.limits.max_connections;
    let active = Arc::new(AtomicUsize::new(0));

    loop {
        let handle = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                1024 * 1024, // out buffer
                1024 * 1024, // in buffer
                0,
                std::ptr::null(),
            )
        };

        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            let code = unsafe { GetLastError() };
            return Err(std::io::Error::other(format!(
                "CreateNamedPipeW failed: error {code}"
            )));
        }

        let pipe = PipeHandle(handle);

        // Block until a client connects. ERROR_PIPE_CONNECTED (535) means the
        // client connected between CreateNamedPipe and ConnectNamedPipe.
        let connected = unsafe { ConnectNamedPipe(pipe.0, std::ptr::null_mut()) };
        if connected == 0 {
            let code = unsafe { GetLastError() };
            if code != 535 {
                tracing::warn!("ConnectNamedPipe failed: error {code}");
                continue;
            }
        }

        // 先接上再判上限：管道实例已经建立，此时拒绝最省事，客户端也能拿到
        // 一条可读的错误，而不是连不上。
        if active.load(Ordering::SeqCst) >= max_connections {
            tracing::warn!(
                "拒绝连接：已有 {max_connections} 个连接（MH_MAX_CONNECTIONS 可调）"
            );
            reject_busy(pipe, max_connections);
            continue;
        }

        let state = Arc::clone(&state);
        let active = Arc::clone(&active);
        active.fetch_add(1, Ordering::SeqCst);

        std::thread::spawn(move || {
            let _guard = ConnGuard(active);
            use std::os::windows::io::FromRawHandle;
            let file = unsafe { std::fs::File::from_raw_handle(pipe.0 as *mut std::ffi::c_void) };
            // Leak the PipeHandle's Drop: the File now owns the handle.
            std::mem::forget(pipe);

            if let Err(e) = service_connection(file, state) {
                tracing::warn!("connection ended: {e}");
            }
        });
    }
}

/// 连接数已满：回一条错误再关掉，不让客户端白等。
fn reject_busy(pipe: PipeHandle, max_connections: usize) {
    let resp = Response::err(
        0,
        errcode::TOO_MANY_CONNECTIONS,
        format!("后端连接数已达上限（{max_connections}），请关闭多余的实例后重试"),
    );
    if let Ok(line) = crate::pipe::encode_line(&resp) {
        use std::os::windows::io::FromRawHandle;
        let mut file = unsafe { std::fs::File::from_raw_handle(pipe.0 as *mut std::ffi::c_void) };
        // File 接管句柄，PipeHandle 的 Drop 不能再关一次。
        std::mem::forget(pipe);
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// Run the line protocol against one connected pipe.
fn service_connection(pipe_file: std::fs::File, state: Arc<AppState>) -> std::io::Result<()> {
    tracing::info!("client connected");
    let reader = pipe_file.try_clone()?;
    let mut writer = pipe_file;
    let opts = crate::pipe::ServeOptions {
        max_line_bytes: state.limits.max_message_bytes,
        close_on_oversize: true,
    };
    let result = crate::pipe::serve_stream(reader, &mut writer, opts, |line| {
        Some(crate::dispatch::handle_line(&state, line))
    });
    let _ = writer.flush();
    tracing::info!("client disconnected");
    result
}
