//! `mhpack` — console-subsystem **build tool** for the single-file artifact.
//!
//! Deliberately a console binary: `scripts/package_single.ps1` reads its stdout
//! and exit code. (PowerShell does not wait for GUI-subsystem programs, so a
//! windowless packer silently shipped a stale exe. The shipped launcher therefore
//! lives in its own GUI-subsystem binary, `mhlaunch.exe` — see `lib.rs`.)
//!
//! ## Modes
//!
//! * `mhpack pack <dir> -o <exe> [--stem <exe>]` — build a single file from a
//!   directory. `--stem` defaults to `mhlaunch.exe` beside this tool.
//! * `mhpack --verify [<exe>]` — check an embedded payload (defaults to this exe).
//! * `mhpack --extract-to <dir>` — unpack a packaged file for diagnostics.
//! * `mhpack --help` — usage.

use std::path::{Path, PathBuf};

fn main() {
    if let Err(e) = run() {
        eprintln!("mhpack: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("pack") => {
            let dir = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: mhpack pack <dist-dir> -o <output.exe> [--stem <exe>]"))?;
            let out = flag_value(&args, "-o")
                .ok_or_else(|| anyhow::anyhow!("pack 需要 -o <output.exe>"))?;
            let stem = match flag_value(&args, "--stem") {
                Some(p) => PathBuf::from(p),
                None => mhpack::default_stem()?,
            };
            mhpack::pack(Path::new(dir), Path::new(&out), &stem)
        }
        Some("--verify") | Some("verify") => {
            // `--verify <path>` checks a packaged file; bare `--verify` checks self.
            let target = args.get(1).map(PathBuf::from);
            let label = target
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "本文件".to_string());
            let report = mhpack::verify_file(target.as_deref())?;
            println!("{label} 内置载荷：{} 字节 ({} MB)", report.length, report.length / 1_048_576);
            if report.matches {
                println!("校验通过：SHA-256 {}", report.actual);
                Ok(())
            } else {
                anyhow::bail!(
                    "校验失败，载荷已损坏\n  期望: {}\n  实际: {}",
                    report.expected,
                    report.actual
                )
            }
        }
        Some("--extract-only") => {
            let target = mhpack::ensure_extracted(false)?;
            println!("extracted to {}", target.display());
            Ok(())
        }
        Some("--extract-to") => {
            let dir = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("--extract-to 需要目录参数"))?;
            mhpack::extract_to(Path::new(dir))?;
            println!("extracted to {dir}");
            Ok(())
        }
        Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!("未知参数 '{other}'（试试 --help）")),
    }
}

/// Value following `flag` in the argument list.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn print_help() {
    println!("mhpack — 梦幻手游记录助手 单文件打包工具\n");
    println!("USAGE:");
    println!("  mhpack pack <dir> -o <exe> [--stem <exe>]   从目录生成单文件");
    println!("  mhpack --verify [<exe>]                     校验内置载荷（默认本文件）");
    println!("  mhpack --extract-to <dir>                   解压到指定目录（排查用）");
    println!("  mhpack --help                               显示本帮助");
    println!();
    println!("说明: 分发用的单文件由 GUI 子系统的 mhlaunch.exe 作为外壳生成，");
    println!("      --stem 省略时自动使用同目录下的 mhlaunch.exe。");
    println!("      直接双击运行请使用生成出来的单文件，而不是本工具。");
}
