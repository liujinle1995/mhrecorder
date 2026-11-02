//! Win32 named-pipe server.
//!
//! Creates `\\.\pipe\mhrecorder.v1`, accepts clients and runs the line protocol
//! on a thread per connection. Uses `windows-sys` directly to keep the
//! dependency surface small and avoid an async runtime.

#![cfg(windows)]

use std::io::Write;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

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

/// Build a UTF-16, NUL-terminated pipe path ("\\\\.\\pipe\\<name>").
fn wide_pipe_path(name: &str) -> Vec<u16> {
    let mut s: Vec<u16> = format!("\\\\.\\pipe\\{name}").encode_utf16().collect();
    s.push(0);
    s
}

/// Block forever accepting clients on `name`, servicing each on its own thread.
pub fn run_server(name: &str, state: Arc<AppState>) -> std::io::Result<()> {
    let path = wide_pipe_path(name);
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
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("CreateNamedPipeW failed: error {code}"),
            ));
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

        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            use std::os::windows::io::FromRawHandle;
            let file = unsafe {
                std::fs::File::from_raw_handle(pipe.0 as *mut std::ffi::c_void)
            };
            // Leak the PipeHandle's Drop: the File now owns the handle.
            std::mem::forget(pipe);

            if let Err(e) = service_connection(file, state) {
                tracing::warn!("connection ended: {e}");
            }
        });
    }
}

/// Run the line protocol against one connected pipe.
fn service_connection(pipe_file: std::fs::File, state: Arc<AppState>) -> std::io::Result<()> {
    tracing::info!("client connected");
    let reader = pipe_file.try_clone()?;
    let mut writer = pipe_file;
    let result = crate::pipe::serve_stream(reader, &mut writer, |line| {
        Some(crate::dispatch::handle_line(&state, line))
    });
    let _ = writer.flush();
    tracing::info!("client disconnected");
    result
}
