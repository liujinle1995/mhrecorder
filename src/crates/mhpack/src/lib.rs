//! `mhpack` — shared core of the single-file distribution toolchain.
//!
//! ## Why a self-extracting launcher
//!
//! WinUI 3 cannot be statically linked into one file: Windows App SDK resolves
//! its DLLs (`Microsoft.ui.xaml.dll`, `Microsoft.Windows.SDK.NET.dll`, …) at
//! load time from the executable's directory. The app also needs a *second*
//! process (`mhservice.exe`) plus native OpenCV/ONNX Runtime and OCR models.
//!
//! So a genuine one-file artifact has to be a **self-extracting launcher**: a
//! small stem executable carries the whole application as a ZIP payload appended
//! after its own image, unpacks it into a private per-user directory on first
//! run, and then starts the app exactly as the two-file layout would.
//!
//! ## On-disk format
//!
//! ```text
//! [ stem.exe image ][ zip payload ][ trailer ]
//! ```
//!
//! The trailer is fixed-width so it can be located without scanning the whole
//! file:
//!
//! ```text
//! offset_of_payload : u64 little-endian
//! payload_length    : u64 little-endian
//! sha256(payload)   : 32 bytes
//! magic             : 8 bytes  = "MHPACK01"
//! ```
//!
//! `sha256` lets a partially-copied or truncated download be detected instead of
//! silently unpacking a corrupt app.
//!
//! ## Console behaviour (the "黑窗" postmortem)
//!
//! 0.9.2 made the *backend* GUI-subsystem, yet users still saw a console that
//! stayed open for the whole session. Cause: the launcher itself was
//! console-subsystem (its `pack` mode needs stdout), Windows allocated a console
//! for it on double-click, and `mhservice` then called
//! `AttachConsole(ATTACH_PARENT_PROCESS)` — attaching to that console and
//! logging into it, which re-showed the window `ShowWindow(SW_HIDE)` had hidden.
//!
//! Two fixes, both required:
//!   * the shipped stem (`mhlaunch.exe`) is GUI-subsystem, so no console is ever
//!     allocated for it, and
//!   * `mhservice` only attaches to a parent console when the caller asked for
//!     console output (`--stdio`, `MH_LOG_CONSOLE=1`), never implicitly.
//!
//! ## Modes
//!
//! * `mhpack pack <dir> -o <exe> [--stem <exe>]` — build a single file.
//! * `mhpack --verify [<exe>]` — check an embedded payload's integrity.
//! * `mhpack --extract-only` — unpack without launching (diagnostics).
//! * `mhpack --extract-to <dir>` — unpack into an explicit directory.
//! * `mhlaunch` (no args) — the shipped launcher: ensure extracted, then start.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Trailer magic. Change the version suffix if the trailer layout ever changes.
pub const MAGIC: &[u8; 8] = b"MHPACK01";
/// payload_offset(8) + payload_length(8) + sha256(32) + magic(8)
pub const TRAILER_LEN: u64 = 8 + 8 + 32 + 8;
/// Marker file written into the extraction dir. Holds the payload hash so a new
/// version of the launcher re-extracts over an older install.
pub const STAMP_FILE: &str = ".mhpack-stamp";
/// The executable started after extraction, relative to the extraction dir.
pub const ENTRY_EXE: &str = "MhRecorder.exe";
/// The backend service started before the UI.
pub const SERVICE_EXE: &str = "mhservice.exe";
/// File name of the shipped GUI-subsystem stem.
pub const LAUNCHER_EXE: &str = "mhlaunch.exe";
/// Named pipe shared with the C# client and the Rust service.
pub const PIPE_NAME: &str = "\\\\.\\pipe\\mhrecorder.v1";
/// Seconds to wait for the backend pipe before giving up and launching anyway.
pub const PIPE_TIMEOUT_SECS: u64 = 20;

/// Where the payload lives inside this file.
#[derive(Debug, Clone, Copy)]
pub struct Payload {
    pub offset: u64,
    pub length: u64,
    pub sha256: [u8; 32],
}

// ---------------------------------------------------------------------------
// Payload access
// ---------------------------------------------------------------------------

/// Read the trailer of an arbitrary packaged file.
pub fn payload_from_path(path: &Path) -> anyhow::Result<(std::fs::File, Payload)> {
    let mut file = std::fs::File::open(path)?;
    let total = file.metadata()?.len();

    if total < TRAILER_LEN {
        anyhow::bail!("{} 不是有效的单文件包（文件过小）", path.display());
    }

    file.seek(SeekFrom::End(-(TRAILER_LEN as i64)))?;
    let mut trailer = [0u8; TRAILER_LEN as usize];
    file.read_exact(&mut trailer)?;

    let magic = &trailer[TRAILER_LEN as usize - 8..];
    if magic != MAGIC {
        anyhow::bail!(
            "{} 不是 mhpack 单文件包（magic 不匹配）\n\
             如果你拿到的是解压后的目录，请直接运行 {ENTRY_EXE}。",
            path.display()
        );
    }

    let offset = u64::from_le_bytes(trailer[0..8].try_into().unwrap());
    let length = u64::from_le_bytes(trailer[8..16].try_into().unwrap());
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&trailer[16..48]);

    if offset > total || offset.saturating_add(length) > total.saturating_sub(TRAILER_LEN) {
        anyhow::bail!("{} 的内置载荷长度非法（文件可能已损坏）", path.display());
    }

    Ok((file, Payload { offset, length, sha256 }))
}

/// Read the trailer of the file this executable was loaded from.
pub fn read_self_payload() -> anyhow::Result<(std::fs::File, Payload)> {
    let exe = std::env::current_exe()?;
    payload_from_path(&exe)
}

/// Hash the embedded payload region of `file`.
pub fn hash_payload(mut file: std::fs::File, payload: &Payload) -> anyhow::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(payload.offset))?;
    let mut reader = std::io::Read::take(file, payload.length);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish())
}

/// Integrity report for one packaged file.
pub struct VerifyReport {
    pub length: u64,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
}

/// Verify the embedded payload of `path` (or of this executable when `None`).
pub fn verify_file(path: Option<&Path>) -> anyhow::Result<VerifyReport> {
    let (file, payload) = match path {
        Some(p) => payload_from_path(p)?,
        None => read_self_payload()?,
    };
    let digest = hash_payload(file, &payload)?;
    Ok(VerifyReport {
        length: payload.length,
        matches: digest == payload.sha256,
        expected: hex(&payload.sha256),
        actual: hex(&digest),
    })
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Where the payload gets extracted.
///
/// Per-user, version-stamped, and not `%TEMP%` so the install survives reboot.
pub fn install_dir(payload: &Payload) -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    install_dir_under(&base, &payload.sha256)
}

/// 给定根目录时的安装路径。
///
/// 目录名取载荷哈希前 16 位：**每个构建一个目录**。这样升级时新版本解到新
/// 目录，旧版本的文件不会和新版本混在一起，也不存在"覆盖到一半失败"的中间
/// 状态——要么新目录完整，要么继续用旧的。
pub fn install_dir_under(base: &Path, sha256: &[u8; 32]) -> PathBuf {
    base.join("MhRecorder").join("app").join(&hex(sha256)[..16])
}

pub fn ensure_extracted(_quiet: bool) -> anyhow::Result<PathBuf> {
    let (file, payload) = read_self_payload()?;
    let target = install_dir(&payload);
    let stamp = target.join(STAMP_FILE);
    let expected = hex(&payload.sha256);

    if stamp.exists() {
        // Fast path: already unpacked for this exact build.
        if let Ok(prev) = std::fs::read_to_string(&stamp) {
            if prev.trim() == expected && target.join(ENTRY_EXE).exists() {
                return Ok(target);
            }
        }
    }

    extract_payload(file, &payload, &target)?;

    std::fs::write(&stamp, &expected)?;
    Ok(target)
}

pub fn extract_to(dir: &Path) -> anyhow::Result<()> {
    let (file, payload) = read_self_payload()?;
    extract_payload(file, &payload, dir)?;
    std::fs::write(dir.join(STAMP_FILE), hex(&payload.sha256))?;
    Ok(())
}

pub fn extract_payload(
    mut file: std::fs::File,
    payload: &Payload,
    target: &Path,
) -> anyhow::Result<()> {
    // Validate the file we were actually handed, before writing anything.
    //
    // 以前这里开的是 `current_exe()`：正常双击运行时两者是同一个文件，所以
    // 一直没暴露；但 `mhpack.exe --extract <别的包>` 会拿自己的哈希去比别人的
    // 载荷，永远失败。完整性检查必须针对"正在解的这个文件"。
    let digest = hash_payload(file.try_clone()?, payload)?;
    if digest != payload.sha256 {
        anyhow::bail!("内置载荷校验失败（文件不完整或已损坏），请重新下载");
    }

    std::fs::create_dir_all(target)?;

    file.seek(SeekFrom::Start(payload.offset))?;
    // Take ownership of the limited reader so the archive's element type is
    // `Take<File>` (passing `&mut reader` would make it `&mut Take<File>`).
    let reader = std::io::Read::take(file, payload.length);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| anyhow::anyhow!("无法读取内置压缩包: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            anyhow::bail!("压缩包内含非法路径，已中止");
        };
        let dest = target.join(&rel);

        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // A previously running instance may hold the exe/DLLs open. Retry once
        // after clearing the read-only bit, and report clearly if still locked.
        match write_entry(&mut entry, &dest) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let _ = std::fs::remove_file(&dest);
                write_entry(&mut entry, &dest).map_err(|e2| {
                    anyhow::anyhow!(
                        "无法写入 {}：{}\n请关闭正在运行的「梦幻手游记录助手」后重试。",
                        dest.display(),
                        e2
                    )
                })?;
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

/// Concrete reader type used when extracting the payload.
type PayloadReader = std::io::Take<std::fs::File>;
type PayloadZipFile<'a> = zip::read::ZipFile<'a, PayloadReader>;

fn write_entry(entry: &mut PayloadZipFile<'_>, dest: &Path) -> std::io::Result<()> {
    let mut out = std::fs::File::create(dest)?;
    std::io::copy(entry, &mut out)?;
    out.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Launch
// ---------------------------------------------------------------------------

/// Ensure the app is unpacked, start the backend, then start the UI.
///
/// Called by the GUI-subsystem stem (`mhlaunch.exe`). Returning `Err` is fatal:
/// the caller shows a native message box, because a windowless process that
/// exits silently looks like "the app does not start".
pub fn launch() -> anyhow::Result<()> {
    let dir = ensure_extracted(false)?;
    let app = dir.join(ENTRY_EXE);
    let service = dir.join(SERVICE_EXE);

    if !app.exists() {
        anyhow::bail!("解压后未找到 {ENTRY_EXE}（目录：{}）", dir.display());
    }

    // The UI talks to the backend over the `mhrecorder.v1` named pipe. Start it
    // first and wait for the pipe to appear.
    //
    // 等不到就**报错退出**，不再把界面拉起来让它自己说"离线演示数据"：那种
    // 情况下用户以为在记录，其实什么都没落库。启动器是唯一知道"后端没起来"
    // 这件事的地方，所以由它弹窗说清楚。
    if !service.exists() {
        anyhow::bail!(
            "缺少后端程序 {SERVICE_EXE}（目录：{}）。发布包不完整，请重新解压或重新下载。",
            dir.display()
        );
    }

    if !win::pipe_ready(PIPE_NAME) {
        start_service(&service, &dir)?;
        if !wait_for_pipe(std::time::Duration::from_secs(PIPE_TIMEOUT_SECS)) {
            anyhow::bail!(
                "后端 {SERVICE_EXE} 启动失败：等了 {PIPE_TIMEOUT_SECS} 秒仍未建立命名管道 \
                 mhrecorder.v1。\n\n请尝试：\n\
                 1. 关闭本提示后重新双击启动；\n\
                 2. 把程序目录加入杀毒软件白名单（它会创建命名管道，可能被拦截）；\n\
                 3. 查看日志 %LOCALAPPDATA%\\MhRecorder\\mhservice.log。"
            );
        }
    }

    let mut cmd = std::process::Command::new(&app);
    cmd.current_dir(&dir);
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("无法启动 {}：{e}", app.display()))?;

    // The launcher's job is done; the app owns the session.
    Ok(())
}

/// Start the backend detached so it outlives the launcher.
fn start_service(exe: &Path, dir: &Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new(exe);
    cmd.current_dir(dir);
    cmd.creation_flags(0x0800_0000 | 0x0000_0008); // CREATE_NO_WINDOW | DETACHED_PROCESS
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("无法启动后端 {}：{e}", exe.display()))?;
    Ok(())
}

/// 等后端把管道建起来。返回 `false` 表示等到超时——调用方必须把这件事报出去。
fn wait_for_pipe(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if win::pipe_ready(PIPE_NAME) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    win::pipe_ready(PIPE_NAME)
}

// ---------------------------------------------------------------------------
// Pack (build-time side is driven by scripts\package_single.ps1)
// ---------------------------------------------------------------------------

/// Build a single file from a directory, using `stem` as the executable image.
///
/// The stem must be a **GUI-subsystem** binary (`mhlaunch.exe`), otherwise the
/// distributed file keeps allocating a console on every double-click.
pub fn pack(dir: &Path, out: &Path, stem: &Path) -> anyhow::Result<()> {
    if !dir.is_dir() {
        anyhow::bail!("{} 不是目录", dir.display());
    }
    if !stem.is_file() {
        anyhow::bail!(
            "缺少启动器 stem：{}\n请先运行 cargo build --release -p mhpack 生成 mhlaunch.exe",
            stem.display()
        );
    }
    if already_packaged(stem)? {
        anyhow::bail!(
            "{} 已经是打包后的产物，不能再作为 stem（请用构建出的 mhlaunch.exe）",
            stem.display()
        );
    }

    // Build the zip in an adjacent temp file (payload can be ~100 MB).
    //
    // 文件名必须唯一：同一进程里并发打包（测试并行、或将来批量出包）会写到
    // 同一个 `mhpack-<pid>.zip`，互相截断，产物随即损坏。
    let tmp_zip = std::env::temp_dir().join(format!("mhpack-{}-{}.zip", std::process::id(), unique_suffix()));
    write_zip(dir, &tmp_zip)?;

    let mut stem_file = std::fs::File::open(stem)?;
    let stem_len = stem_file.metadata()?.len();

    let mut payload = std::fs::File::open(&tmp_zip)?;
    let payload_len = payload.metadata()?.len();

    let mut hasher = Sha256::new();
    {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = payload.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    let digest = hasher.finish();

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut dest = std::fs::File::create(out)?;
    std::io::copy(&mut stem_file, &mut dest)?;

    let payload_offset = stem_len;
    payload.seek(SeekFrom::Start(0))?;
    std::io::copy(&mut payload, &mut dest)?;

    dest.write_all(&payload_offset.to_le_bytes())?;
    dest.write_all(&payload_len.to_le_bytes())?;
    dest.write_all(&digest)?;
    dest.write_all(MAGIC)?;
    dest.flush()?;

    let _ = std::fs::remove_file(&tmp_zip);

    println!("已生成单文件：{}", out.display());
    println!(
        "  载荷 {} 字节，总大小 {} MB",
        payload_len,
        (payload_offset + payload_len + TRAILER_LEN) / 1_048_576
    );
    println!("  SHA-256 {}", hex(&digest));
    Ok(())
}

/// 进程内单调递增的序号，用来拼唯一的临时文件名。
fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    (nanos << 16) | (n & 0xFFFF)
}

/// True when `path` already carries a payload trailer.
fn already_packaged(path: &Path) -> anyhow::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let total = file.metadata()?.len();
    if total < TRAILER_LEN {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-8))?;
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    Ok(&magic == MAGIC)
}

/// Default stem location: beside the build tool in `target\release`.
pub fn default_stem() -> anyhow::Result<PathBuf> {
    let dir = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("无法定位 mhpack.exe 所在目录"))?;
    Ok(dir.join(LAUNCHER_EXE))
}

fn write_zip(dir: &Path, out: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::create(out)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(dir, dir, &mut entries)?;
    entries.sort();

    for rel in entries {
        let full = dir.join(&rel);
        let name = rel.to_string_lossy().replace('\\', "/");
        zip.start_file(name, opts)?;
        let mut f = std::fs::File::open(&full)?;
        std::io::copy(&mut f, &mut zip)?;
    }

    zip.finish()?;
    Ok(())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if path.is_file() {
            let rel = path.strip_prefix(root)?.to_path_buf();
            out.push(rel);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Minimal Win32 bindings
// ---------------------------------------------------------------------------

// Pull in `user32.lib` for the MessageBoxW used on fatal errors. A
// GUI-subsystem launcher has no console, so a failure must surface as a native
// dialog rather than a silent exit.
#[cfg(windows)]
#[link(name = "user32")]
extern "system" {}

/// Minimal Win32 bindings for probing a named pipe and reporting failures.
///
/// Using the loader's own `CreateFileW` against `\\.\pipe\...` is both instant
/// and accurate. An earlier version shelled out to PowerShell once per poll,
/// which cost ~1s per call and left the launcher spinning long enough that the
/// UI looked like it never started.
#[cfg(windows)]
pub mod win {
    use std::ffi::c_void;

    type Handle = *mut c_void;

    extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: Handle,
        ) -> Handle;
        fn CreateMutexW(
            security_attributes: *mut c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> Handle;
        fn CloseHandle(h: Handle) -> i32;
        fn GetLastError() -> u32;
        fn MessageBoxW(hwnd: Handle, text: *const u16, caption: *const u16, flags: u32) -> i32;
    }

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE_VALUE: isize = -1;
    const ERROR_ALREADY_EXISTS: u32 = 183;

    const MB_ICONERROR: u32 = 0x0000_0010;

    /// Become the only running launcher instance, or report that one exists.
    ///
    /// First run unpacks ~210 MB with no window of any kind (the launcher is
    /// GUI-subsystem on purpose), so a user who sees nothing happening will
    /// double-click again. Without this guard the second copy would unpack into
    /// the same directory concurrently and fail on locked files.
    ///
    /// The handle is deliberately leaked: it must stay open for the lifetime of
    /// the process, and the process is short-lived.
    pub fn acquire_single_instance(name: &str) -> bool {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null_mut(), 0, wide.as_ptr());
            if handle.is_null() {
                // Fail open: a mutex problem must never block startup.
                return true;
            }
            GetLastError() != ERROR_ALREADY_EXISTS
        }
    }

    /// Show a native error dialog. Used by the windowless launcher when startup
    /// fails and there is no console to report into — otherwise the app would
    /// vanish with no explanation.
    pub fn message_box_error(caption: &str, text: &str) {
        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }
        let caption = wide(caption);
        let text = wide(text);
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_ICONERROR,
            );
        }
    }

    /// True when a pipe with this full name is currently creatable/openable.
    pub fn pipe_ready(name: &str) -> bool {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let handle = CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
                return false;
            }
            CloseHandle(handle);
            true
        }
    }
}

#[cfg(not(windows))]
pub mod win {
    pub fn pipe_ready(_name: &str) -> bool {
        false
    }
    pub fn acquire_single_instance(_name: &str) -> bool {
        true
    }
    pub fn message_box_error(caption: &str, text: &str) {
        eprintln!("{caption}: {text}");
    }
}

// ---------------------------------------------------------------------------
// Minimal SHA-256 (avoid pulling a crypto crate into the launcher)
// ---------------------------------------------------------------------------

pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0; 64],
            buf_len: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        while !data.is_empty() {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let total = self.total;

        // Padding must be APPENDED to the partially-filled block, not sent as a
        // separate block: 0x80, zeros, then the 64-bit message length, such that
        // the final block is exactly 64 bytes. Getting this wrong is what made
        // "abc" hash incorrectly before the NIST vectors caught it.
        let mut tail: Vec<u8> = Vec::with_capacity(72);
        tail.push(0x80u8);
        while (self.buf_len + tail.len()) % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&(total * 8).to_be_bytes());

        for byte in tail {
            self.buf[self.buf_len] = byte;
            self.buf_len += 1;
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];

        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        // NIST test vectors; guards the hand-rolled digest used for payload
        // integrity against subtle padding/endianness mistakes.
        let mut h = Sha256::new();
        h.update(b"");
        assert_eq!(
            hex(&h.finish()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            hex(&h.finish()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let mut h = Sha256::new();
        h.update(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        assert_eq!(
            hex(&h.finish()),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_handles_multi_chunk_updates() {
        // Feeding byte-by-byte must equal feeding the whole buffer at once.
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut whole = Sha256::new();
        whole.update(data);
        let expected = whole.finish();

        let mut piecewise = Sha256::new();
        for chunk in data.chunks(7) {
            piecewise.update(chunk);
        }
        assert_eq!(piecewise.finish(), expected);
    }

    #[test]
    fn trailer_constants_are_consistent() {
        assert_eq!(MAGIC.len(), 8);
        assert_eq!(TRAILER_LEN, 56);
    }

    // -----------------------------------------------------------------------
    // 产物完整性与升级
    //
    // 这些测试真的打包、真的解包、真的改坏一个字节，而不是只对着常量断言。
    // -----------------------------------------------------------------------

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mhpack-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 造一个"像 dist 目录"的东西：入口 exe、后端、一个模型文件。
    fn make_dist(root: &Path, marker: &str) -> PathBuf {
        let dist = root.join("dist");
        std::fs::create_dir_all(dist.join("models")).unwrap();
        std::fs::write(dist.join(ENTRY_EXE), format!("stub-ui-{marker}")).unwrap();
        std::fs::write(dist.join(SERVICE_EXE), format!("stub-service-{marker}")).unwrap();
        std::fs::write(dist.join("models").join("det.onnx"), format!("model-{marker}")).unwrap();
        dist
    }

    fn make_stem(root: &Path, tag: &str) -> PathBuf {
        // stem 只需要是一个不以 magic 结尾的文件；pack 只做拼接。
        let stem = root.join(format!("stem-{tag}.exe"));
        std::fs::write(&stem, b"MZ fake gui launcher").unwrap();
        stem
    }

    /// `pack` → 结构可解析 → 哈希对得上 → 解出来和原件逐字节一致。
    #[test]
    fn pack_extract_roundtrip_is_byte_identical() {
        let root = temp_dir("roundtrip");
        let dist = make_dist(&root, "v1");
        let pkg = root.join("MhRecorder-test.exe");
        pack(&dist, &pkg, &make_stem(&root, "v1")).unwrap();

        let report = verify_file(Some(&pkg)).unwrap();
        assert!(report.matches, "刚打出来的包必须自校验通过");
        assert!(report.length > 0);

        let out = root.join("install");
        let (file, payload) = payload_from_path(&pkg).unwrap();
        extract_payload(file, &payload, &out).unwrap();

        for rel in [ENTRY_EXE, SERVICE_EXE, "models/det.onnx"] {
            let name = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
            let a = std::fs::read(dist.join(&name)).unwrap();
            let b = std::fs::read(out.join(&name)).unwrap();
            assert_eq!(a, b, "{rel} 解出来必须和原文件一致");
        }
    }

    /// 载荷里改一个字节，`verify_file` 必须报不匹配。
    #[test]
    fn tampered_payload_fails_verification() {
        let root = temp_dir("tamper");
        let dist = make_dist(&root, "v1");
        let pkg = root.join("MhRecorder-test.exe");
        pack(&dist, &pkg, &make_stem(&root, "v1")).unwrap();

        let (_, payload) = payload_from_path(&pkg).unwrap();
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(&pkg).unwrap();
            f.seek(SeekFrom::Start(payload.offset + 8)).unwrap();
            f.write_all(&[0xFF]).unwrap();
        }

        let report = verify_file(Some(&pkg)).unwrap();
        assert!(!report.matches, "改过字节的包不能再通过校验");
        assert_ne!(report.expected, report.actual);
    }

    /// 哈希不对时，解包必须在**写任何文件之前**就中止。
    #[test]
    fn corrupted_package_is_refused_before_writing() {
        let root = temp_dir("corrupt");
        let dist = make_dist(&root, "v1");
        let pkg = root.join("MhRecorder-test.exe");
        pack(&dist, &pkg, &make_stem(&root, "v1")).unwrap();

        let (file, payload) = payload_from_path(&pkg).unwrap();
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(&pkg).unwrap();
            f.seek(SeekFrom::Start(payload.offset)).unwrap();
            f.write_all(&[0x00; 4]).unwrap();
        }

        let out = root.join("install-should-not-exist");
        let (file2, payload2) = (file, payload);
        let err = extract_payload(file2, &payload2, &out).unwrap_err();
        assert!(err.to_string().contains("校验失败"), "{err}");
        assert!(!out.exists(), "校验失败时一个字节都不该落盘");
    }

    /// 升级：不同构建得到不同安装目录，互不覆写；旧目录内容原样保留。
    #[test]
    fn upgrade_uses_a_fresh_install_dir_and_keeps_the_old_one() {
        let root = temp_dir("upgrade");
        let base = root.join("localappdata");

        let v1 = make_dist(&root.join("v1"), "v1");
        let pkg1 = root.join("v1.exe");
        pack(&v1, &pkg1, &make_stem(&root, "v1")).unwrap();
        let (file1, payload1) = payload_from_path(&pkg1).unwrap();
        let dir1 = install_dir_under(&base, &payload1.sha256);
        extract_payload(file1, &payload1, &dir1).unwrap();

        // 第二个版本：内容不同 → 载荷哈希不同 → 目录不同。
        let v2 = make_dist(&root.join("v2"), "v2");
        let pkg2 = root.join("v2.exe");
        pack(&v2, &pkg2, &make_stem(&root, "v2")).unwrap();
        let (file2, payload2) = payload_from_path(&pkg2).unwrap();
        let dir2 = install_dir_under(&base, &payload2.sha256);
        extract_payload(file2, &payload2, &dir2).unwrap();

        assert_ne!(dir1, dir2, "每个构建一个目录，升级不覆盖旧版本");
        assert_eq!(std::fs::read_to_string(dir1.join(ENTRY_EXE)).unwrap(), "stub-ui-v1");
        assert_eq!(std::fs::read_to_string(dir2.join(ENTRY_EXE)).unwrap(), "stub-ui-v2");
    }

    /// 同一个包重复解到同一个目录必须幂等（用户连点两次、或更新后首次启动）。
    #[test]
    fn extracting_twice_to_the_same_dir_is_idempotent() {
        let root = temp_dir("idempotent");
        let dist = make_dist(&root, "v1");
        let pkg = root.join("pkg.exe");
        pack(&dist, &pkg, &make_stem(&root, "v1")).unwrap();
        let out = root.join("install");

        for _ in 0..2 {
            let (file, payload) = payload_from_path(&pkg).unwrap();
            extract_payload(file, &payload, &out).unwrap();
        }
        assert_eq!(std::fs::read_to_string(out.join(ENTRY_EXE)).unwrap(), "stub-ui-v1");
    }

    /// 不是 mhpack 产物时，报错要说人话（用户最常撞到的就是"拿目录当 exe"）。
    #[test]
    fn a_plain_file_is_rejected_with_a_readable_message() {
        let root = temp_dir("plain");
        let plain = root.join("random.exe");
        std::fs::write(&plain, b"not a package at all, definitely too short").unwrap();
        let err = payload_from_path(&plain).unwrap_err().to_string();
        assert!(err.contains("不是有效的单文件包") || err.contains("magic"), "{err}");

        // 长度够但没有 magic 的情况也要拒绝。
        let big = root.join("big.bin");
        std::fs::write(&big, vec![7u8; TRAILER_LEN as usize + 16]).unwrap();
        let err = payload_from_path(&big).unwrap_err().to_string();
        assert!(err.contains("magic"), "{err}");
    }
}
