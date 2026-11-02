//! Build script: make `mhcli.exe` runnable by merely double-clicking it (same native DLL/model placement as `mhservice`).
//!
//! `mhservice` is the shipped backend. Its native dependencies are resolved by
//! the Windows loader at process start:
//!   * `opencv_world4120.dll` — linked at load time by the `opencv` crate
//!   * `onnxruntime.dll`      — dlopen'd by `ort`'s `load-dynamic` feature
//!
//! The loader searches the executable's directory first, so copying both DLLs
//! next to the produced exe makes the backend self-contained. Previously it
//! only worked when a wrapper had exported PATH, and a direct double-click
//! died with "找不到 opencv_world4120.dll" (STATUS_DLL_NOT_FOUND / 0xC0000135).
//!
//! It also pins `ORT_DYLIB_PATH` at compile time is NOT possible, so runtime
//! discovery is handled in main.rs by preferring the DLL beside the exe.

use std::path::{Path, PathBuf};

const NATIVE_FILES: &[(&str, &str)] = &[
    ("native/opencv/bin", "opencv_world4120.dll"),
    ("native/onnxruntime/bin", "onnxruntime.dll"),
];

/// OCR models that must sit in `models/` beside the executable so a bare
/// double-click can actually recognise screenshots.
const MODEL_FILES: &[&str] = &[
    "ch_PP-OCRv4_det_infer.onnx",
    "ch_PP-OCRv4_rec_infer.onnx",
    "ppocr_keys_v1.txt",
];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // src/crates/mhservice -> <repo>
    let repo = manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("repo root above src/crates/<crate>")
        .to_path_buf();

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("derive target/<profile> from OUT_DIR")
        .to_path_buf();
    let deps_dir = profile_dir.join("deps");

    for (rel, name) in NATIVE_FILES {
        let src = repo.join(rel).join(name);
        println!("cargo:rerun-if-changed={}", src.display());

        if !src.exists() {
            println!(
                "cargo:warning=vendored {} missing at {} — mhservice.exe will need it on PATH",
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

    // Ship the OCR models too: `resolve_models_dir` in main.rs looks for a
    // `models` folder beside the exe, so this makes recognition work with no
    // environment set up at all.
    for name in MODEL_FILES {
        let src = repo.join("native/models").join(name);
        println!("cargo:rerun-if-changed={}", src.display());

        if !src.exists() {
            println!(
                "cargo:warning=vendored OCR model missing at {} — recognition will be unavailable",
                src.display()
            );
            continue;
        }

        for dest_dir in [&profile_dir, &deps_dir] {
            if let Err(e) = copy_if_newer(&src, &dest_dir.join("models").join(name)) {
                println!(
                    "cargo:warning=could not place model {} ({}): {e}",
                    name,
                    dest_dir.display()
                );
            }
        }
    }
}

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
