//! Build script: place the vendored native DLLs next to the built binaries.
//!
//! Both native dependencies are loaded by the OS loader at process start
//! (`opencv` links against `opencv_world4120.dll`, and `ort` with
//! `load-dynamic` dlopen's `onnxruntime.dll`). The Windows loader searches the
//! **executable's own directory first**, so simply having these DLLs beside the
//! exe makes the binary self-sufficient.
//!
//! Without this, `target\release\mhservice.exe` only runs when a wrapper script
//! has pre-populated PATH — double-clicking it fails with the classic
//! "找不到 opencv_world4120.dll (0xC0000135)" dialog.
//!
//! Layout this script understands:
//!   <repo>/native/opencv/bin/opencv_world4120.dll
//!   <repo>/native/onnxruntime/bin/onnxruntime.dll
//! where <repo> is three levels above this crate (src/crates/mhvision).

use std::path::{Path, PathBuf};

/// Native files that must sit beside every produced executable.
const NATIVE_FILES: &[(&str, &str)] = &[
    ("native/opencv/bin", "opencv_world4120.dll"),
    ("native/onnxruntime/bin", "onnxruntime.dll"),
];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // src/crates/mhvision -> <repo>
    let repo = manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("repo root above src/crates/<crate>")
        .to_path_buf();

    // OUT_DIR is <target>/<profile>/build/<pkg>-<hash>/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    // -> <target>/<profile>
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("derive target/<profile> from OUT_DIR")
        .to_path_buf();
    let deps_dir = profile_dir.join("deps");

    for (rel, name) in NATIVE_FILES {
        let src = repo.join(rel).join(name);
        // Make cargo rerun this script when the vendored binaries change.
        println!("cargo:rerun-if-changed={}", src.display());

        if !src.exists() {
            // Not fatal at build time: a source-only checkout can still compile
            // as long as the user provides the DLLs another way (env.ps1 PATH).
            println!(
                "cargo:warning=vendored {} missing at {} — binaries will need it on PATH",
                name,
                src.display()
            );
            continue;
        }

        for dest_dir in [&profile_dir, &deps_dir] {
            if let Err(e) = copy_if_newer(&src, &dest_dir.join(name)) {
                println!(
                    "cargo:warning=could not place {} next to build output ({}): {e}",
                    name,
                    dest_dir.display()
                );
            }
        }
    }
}

/// Copy `src` to `dest`, skipping the write when the destination is already
/// up to date (avoids relinking churn and fails cleanly if the DLL is locked).
fn copy_if_newer(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let (Ok(s), Ok(d)) = (src.metadata(), dest.metadata()) {
        if d.len() == s.len() {
            if let (Ok(st), Ok(dt)) = (s.modified(), d.modified()) {
                if dt >= st {
                    return Ok(());
                }
            }
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dest)?;
    Ok(())
}
