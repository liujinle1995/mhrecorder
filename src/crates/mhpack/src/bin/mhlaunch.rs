//! `mhlaunch` — the shipped single-file launcher.
//!
//! This is the executable users get: the payload is appended to this image's
//! trailer, so double-clicking it unpacks the app (first run only) and starts it.
//!
//! **GUI subsystem on purpose.** A console-subsystem launcher makes Windows
//! allocate a console window on every double-click, and `mhservice` would then
//! `AttachConsole(ATTACH_PARENT_PROCESS)` and log into it, keeping a black
//! window alive for the entire session (the 0.9.2 bug report). A GUI-subsystem
//! image has no console at all, so there is nothing to inherit, nothing to show,
//! and no log window — even before the backend's own console gating kicks in.
//!
//! Because there is no console to report into, failures surface as a native
//! message box instead of a silent exit.
#![cfg_attr(windows, windows_subsystem = "windows")]

/// Per-session mutex name; a second double-click while the first copy is still
/// unpacking exits immediately instead of racing on the same target directory.
const LAUNCHER_MUTEX: &str = "Local\\mhrecorder-launcher.v1";

fn main() {
    if !mhpack::win::acquire_single_instance(LAUNCHER_MUTEX) {
        return;
    }

    if let Err(e) = mhpack::launch() {
        mhpack::win::message_box_error("梦幻手游记录助手", &format!("{e:#}"));
        std::process::exit(1);
    }
}
