//! mhcli — standalone backend harness.
//!
//! Deliberately independent of the WinUI front end so the whole recognition +
//! storage stack can be tested from a terminal, exactly as required by the
//! "backend must be independently testable" design principle.
//!
//! Run `mhcli --help` for usage.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ort=warn")),
        )
        .with_ansi(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print_help();
        return;
    }

    let code = match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn print_help() {
    println!("mhcli {} — mhrecorder backend harness\n", env!("CARGO_PKG_VERSION"));
    println!("USAGE:");
    println!("  mhcli features <image>            Print pHash / ORB / HSV features");
    println!("  mhcli compare <imageA> <imageB>   Print similarity scores");
    println!("  mhcli classify <image>            Print the detected screen kind");
    println!("  mhcli panel <image>               Print the panel box locate_panel() would crop to");
    println!("  mhcli appearance <image> [attr|bag]  Print the 发色 / 武器 signature");
    println!("  mhcli ocr <image>                 Print every recognised text line");
    println!("  mhcli recognize <image>...        Run the full pipeline, print JSON");
    println!("  mhcli db-init                     Create/open the database and print its path");
    println!("  mhcli db-list                     List stored characters");
    println!("  mhcli db-stats                    Print the statistics payload");
    println!("  mhcli save [--to <id>] <image>... Recognize and persist a batch");
    println!("  mhcli export-json <dir>           Export all data as JSON");
    println!("\nModels dir comes from MH_MODELS_DIR; data dir from MH_DATA_DIR");
    println!("(both default to sensible values, see scripts/env.ps1).");
}

/// Resolve the OCR models directory.
///
/// Priority: env override → `models/` beside the executable (placed there by
/// this crate's build.rs) → the repo's `native/models` for in-tree dev runs.
fn models_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MH_MODELS_DIR") {
        return PathBuf::from(p);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        let beside = dir.join("models");
        if beside.join("ch_PP-OCRv4_det_infer.onnx").exists() {
            return beside;
        }
        // Running from target/release: walk back up to the repo's native/models.
        for up in 1..=4 {
            let mut probe = dir.clone();
            for _ in 0..up {
                match probe.parent() {
                    Some(p) => probe = p.to_path_buf(),
                    None => break,
                }
            }
            let candidate = probe.join("native").join("models");
            if candidate.join("ch_PP-OCRv4_det_infer.onnx").exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("native/models")
}

fn data_dir() -> String {
    std::env::var("MH_DATA_DIR").unwrap_or_else(|_| {
        // Keep CLI test runs out of the user's real data directory.
        let base = std::env::var("TEMP").unwrap_or_else(|_| ".".into());
        format!("{base}\\mhrecorder-cli")
    })
}

fn run(args: &[String]) -> Result<()> {
    match args[0].as_str() {
        "features" => {
            let img = load_image(&args[1])?;
            let f = mhvision::extract_features(&img)?;
            println!("phash       = {:016x}", f.phash);
            println!("dimensions  = {}x{}", f.width, f.height);
            println!("orb         = {} keypoints, {} descriptor bytes",
                f.orb_keypoints.len(), f.orb.len());
            let non_zero: Vec<String> = f
                .hsv_hist
                .iter()
                .enumerate()
                .filter(|(_, v)| **v > 0.02)
                .map(|(i, v)| format!("h{i}:{v:.3}"))
                .collect();
            println!("hsv(32 bin) = {}", non_zero.join(" "));
            Ok(())
        }
        "compare" => {
            let a = load_image(&args[1])?;
            let b = load_image(&args[2])?;
            let fa = mhvision::extract_features(&a)?;
            let fb = mhvision::extract_features(&b)?;
            let s = mhvision::compare(&fa, &fb);
            println!("phash = {:.4}", s.phash);
            println!("orb   = {:.4}", s.orb);
            println!("hsv   = {:.4}", s.hsv);
            println!("blend = {:.4}", s.combined());
            Ok(())
        }
        "classify" => {
            let img = load_image(&args[1])?;
            println!("{:?}", mhvision::classify_screen(&img));
            Ok(())
        }
        // Debug helper: what the window-capture path would crop to before OCR.
        "panel" => {
            let img = load_image(&args[1])?;
            match mhvision::locate_panel(&img) {
                Some(b) => println!(
                    "panel {}x{} at ({}, {}) — {:.0}% of the frame",
                    b.width,
                    b.height,
                    b.x,
                    b.y,
                    100.0 * (b.width * b.height) as f64
                        / (img.width() * img.height()) as f64
                ),
                None => println!("no panel (frame is used as-is)"),
            }
            Ok(())
        }
        "ocr" => {
            let img = load_image(&args[1])?;
            let mut engine = mhvision::OcrEngine::new(&models_dir())
                .context("loading OCR models")?;
            let lines = engine.recognize(&img)?;
            println!("{} lines", lines.len());
            for l in lines {
                println!("{:>6.3}  {:?}  {}", l.confidence, l.rect, l.text);
            }
            Ok(())
        }
        // Debug helper: 发色 / 武器 the pipeline would record for a capture.
        // Second argument is the panel kind (attr | bag), because the head sits
        // in a different place on each panel.
        "appearance" => {
            let img = load_image(&args[1])?;
            let kind = match args.get(2).map(|s| s.as_str()) {
                Some("attr") => mhcore::ScreenKind::CharacterAttributes,
                _ => mhcore::ScreenKind::Inventory,
            };
            match mhvision::appearance_for_capture(&img, kind) {
                Some(a) => {
                    println!("hair   = {} rgb={:?}", a.hair_name, a.hair_rgb);
                    match (&a.weapon_name, a.weapon_rgb) {
                        (Some(n), Some(rgb)) => println!("weapon = {} rgb={:?}", n, rgb),
                        _ => println!("weapon = (none)"),
                    }
                    println!("{}", serde_json::to_string_pretty(&a)?);
                }
                None => println!("no character model found (no signature)"),
            }
            Ok(())
        }
        "recognize" => {
            let images = load_batch(&args[1..])?;
            let mut pipeline = mhvision::RecognitionPipeline::new(&models_dir())
                .context("loading pipeline")?;
            // Same inputs the service path uses, including the stored
            // fingerprints that link a 包裹 capture to its character.
            let store = mhstore::Store::open_in_data_dir(&data_dir())?;
            let known: Vec<mhcore::Character> = store
                .list_characters()?
                .into_iter()
                .map(|d| d.character)
                .collect();
            let refs = store.list_identity_refs()?;
            let result = pipeline.recognize_batch_with(&images, &known, &refs)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        "db-init" => {
            let store = mhstore::Store::open_in_data_dir(&data_dir())?;
            println!("database: {}", store.db_path().display());
            Ok(())
        }
        "db-list" => {
            let store = mhstore::Store::open_in_data_dir(&data_dir())?;
            let chars = store.list_characters()?;
            let refs = store.list_identity_refs()?;
            println!("{} characters", chars.len());
            for c in chars {
                let look = refs
                    .iter()
                    .find(|r| r.character_id == c.character.id)
                    .map(|r| r.appearance.as_ref().map(|a| a.describe()))
                    .flatten()
                    .unwrap_or_else(|| "—".to_string());
                println!(
                    "  {:<12} id={:<10} Lv{:<4} conf={:.2} snapshots={} look={} cid={}",
                    c.character.nickname,
                    c.character.game_id.as_deref().unwrap_or("-"),
                    c.character.level.unwrap_or(0),
                    c.character.identity_confidence,
                    c.snapshot_count,
                    look,
                    c.character.id
                );
            }
            Ok(())
        }
        "db-stats" => {
            let store = mhstore::Store::open_in_data_dir(&data_dir())?;
            let stats = store.statistics(5)?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
            Ok(())
        }
        "save" => {
            // `save [--to <character-id>] [--as <昵称>:<游戏ID>] <image>...`
            //
            // `--to` mirrors what the UI does when the user attributes a capture
            // to one of the characters already in the library (a 包裹 screenshot
            // has no 游戏ID at all). `--as` has no UI counterpart any more; it is
            // kept for fixtures and for repairing rows whose identity was lost,
            // since the backend resolves identity from these two fields anyway.
            let mut target: Option<String> = None;
            let mut as_owner: Option<(String, String)> = None;
            let mut files: Vec<String> = Vec::new();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--to" {
                    target = it.next().cloned();
                } else if a == "--as" {
                    let spec = it.next().cloned().unwrap_or_default();
                    let (nick, gid) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
                    as_owner = Some((nick.to_string(), gid.to_string()));
                } else {
                    files.push(a.clone());
                }
            }
            if files.is_empty() {
                return Err(anyhow!("save needs at least one image"));
            }

            let images = load_batch(&files)?;
            let mut pipeline = mhvision::RecognitionPipeline::new(&models_dir())?;
            let mut store = mhstore::Store::open_in_data_dir(&data_dir())?;
            let known: Vec<mhcore::Character> = store
                .list_characters()?
                .into_iter()
                .map(|d| d.character)
                .collect();
            let refs = store.list_identity_refs()?;
            let mut result = pipeline.recognize_batch_with(&images, &known, &refs)?;
            if let Some(id) = target {
                result.character_id = Some(id);
            }
            if let Some((nick, gid)) = as_owner {
                if !nick.is_empty() {
                    result.character.nickname = Some(nick);
                }
                if !gid.is_empty() {
                    result.character.game_id = Some(gid);
                }
            }
            let id = store.save_recognition(&result, &files)?;
            println!(
                "saved character {id} (identity={:?}, nickname={:?}, evidence={:?})",
                result.identity_source, result.character.nickname, result.identity_evidence
            );
            Ok(())
        }
        "export-json" => {
            let dir = Path::new(&args[1]);
            std::fs::create_dir_all(dir)?;
            let store = mhstore::Store::open_in_data_dir(&data_dir())?;
            let doc = store.export_document()?;
            let path = dir.join("data.json");
            std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
            println!("wrote {}", path.display());
            Ok(())
        }
        other => Err(anyhow!("unknown command '{other}' (try --help)")),
    }
}

fn load_image(path: &str) -> Result<image::RgbaImage> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    mhvision::decode_image(&bytes).map_err(|e| anyhow!("{e}"))
}

fn load_batch(paths: &[String]) -> Result<Vec<image::RgbaImage>> {
    if paths.is_empty() {
        return Err(anyhow!("no images given"));
    }
    paths.iter().map(|p| load_image(p)).collect()
}

