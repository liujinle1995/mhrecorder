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
        // 日志必须走 stderr：`recognize` / `db-stats` 的 stdout 是给脚本
        // `ConvertFrom-Json` 吃的，混进一行 INFO 就整段解析失败。
        //
        // 默认级别压到 warn 也是同理：调用方（验收脚本）用 `2>$null` 收尾，
        // 在 `$ErrorActionPreference='Stop'` 下任何 stderr 输出都会变成终止
        // 错误。需要细节时 `RUST_LOG=info` 打开。
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,ort=warn")),
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
    println!("  mhcli features <image>            Print dHash / ORB / HSV features");
    println!("  mhcli compare <imageA> <imageB>   Print similarity scores");
    println!("  mhcli classify <image>            Print the detected screen kind");
    println!("  mhcli panel <image>               Print the panel box locate_panel() would crop to");
    println!("  mhcli appearance <image> [attr|bag]  Print the 发色 / 武器 signature");
    println!("  mhcli ocr <image>                 Print every recognised text line");
    println!("  mhcli ocr-bench [--det legacy|db|both] [--batch N] [--threads N] [--repeat K] <image>...");
    println!("                                    A/B the detection post-processing and batching");
    println!("  mhcli recognize <image>...        Run the full pipeline, print JSON");
    println!("  mhcli db-init                     Create/open the database and print its path");
    println!("  mhcli db-list                     List stored characters");
    println!("  mhcli db-stats                    Print the statistics payload");
    println!("  mhcli save [--to <id>] <image>... Recognize and persist a batch");
    println!("  mhcli eval <dataset-dir> [--json <out>]");
    println!("                                    Run the pipeline over a labelled dataset and score it");
    println!("  mhcli export-json <dir>           Export all data as JSON");
    println!("  mhcli backup <file.zip> [--no-db] Full backup: JSON + database + manifest");
    println!("  mhcli db-tasks [limit]            Show the recognition task history");
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
            println!("dhash       = {:016x}", f.dhash);
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
            println!(
                "hsv         = {} hue bins + {} sat + {} val (sum {:.2})",
                mhvision::HUE_BINS,
                mhvision::SAT_BINS,
                mhvision::VAL_BINS,
                f.hsv_hist.iter().sum::<f32>()
            );
            println!("hue peaks   = {}", non_zero.join(" "));
            Ok(())
        }
        "compare" => {
            let a = load_image(&args[1])?;
            let b = load_image(&args[2])?;
            let fa = mhvision::extract_features(&a)?;
            let fb = mhvision::extract_features(&b)?;
            let s = mhvision::compare(&fa, &fb);
            println!("dhash  = {:.4}", s.dhash);
            println!("orb    = {:.4} ({} 个几何内点)", s.orb, s.orb_inliers);
            println!("hsv    = {:.4}", s.hsv);
            println!("blend  = {:.4}", s.combined());
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
            for (k, v) in engine.config().describe().split(' ').filter_map(|kv| kv.split_once('=')) {
                println!("{k:<10} = {v}");
            }
            let lines = engine.recognize(&img)?;
            println!("{} lines", lines.len());
            for l in lines {
                println!("{:>6.3}  {:?}  {}", l.confidence, l.rect, l.text);
            }
            Ok(())
        }
        // A/B 对照：同一张图跑两套检测后处理 / 不同批大小 / 不同线程数，
        // 把"感觉更快"变成可比较的数字与文本差异。
        "ocr-bench" => ocr_bench(&args[1..]),
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
            let result =
                pipeline.recognize_batch_with(&images, &known, &refs, &mhvision::NoControl)?;
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
            let mut result =
                pipeline.recognize_batch_with(&images, &known, &refs, &mhvision::NoControl)?;
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
            match store.save_recognition(&result, &files)? {
                mhstore::SaveOutcome::Saved { character_id, created } => println!(
                    "saved character {character_id}{} (identity={:?}, nickname={:?}, evidence={:?})",
                    if created { " [new]" } else { "" },
                    result.identity_source,
                    result.character.nickname,
                    result.identity_evidence
                ),
                // 命令行没有"点一下选归属"这条路：打出来让人知道数据在哪儿。
                mhstore::SaveOutcome::Pending { pending_id, reason } => println!(
                    "saved as pending {pending_id}（{reason}）；可在界面的「待归属」里指定角色"
                ),
            }
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
        "backup" => {
            let dest = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("backup needs a path"))?);
            let include_db = !args.iter().any(|a| a == "--no-db");
            let store = mhstore::Store::open_in_data_dir(&data_dir())?;
            let summary = store.export_backup(&dest, include_db)?;
            println!(
                "wrote {} ({:.1} MB, {} entries, database={})",
                summary.path,
                summary.bytes as f64 / 1_048_576.0,
                summary.entries.len(),
                summary.includes_database
            );
            for e in &summary.entries {
                println!("  - {e}");
            }
            Ok(())
        }
        "db-tasks" => {
            let limit = args.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(20);
            let store = mhstore::Store::open_in_data_dir(&data_dir())?;
            let tasks = store.list_tasks(limit)?;
            println!("{} tasks", tasks.len());
            for t in tasks {
                println!(
                    "  {:<22} {:<10} {:>7} ms  ok={:?}  {}",
                    t.id,
                    t.state,
                    t.duration_ms.unwrap_or(0),
                    t.ok,
                    t.error.unwrap_or_default()
                );
            }
            Ok(())
        }
        "eval" => eval(args),
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

// ---------------------------------------------------------------------------
// OCR A/B 基准
// ---------------------------------------------------------------------------
//
// 换检测后处理、改批大小、改线程数，都必须在真实截图上先量一遍再改默认值。
// 这个子命令把"哪套更准、快多少"变成同一张表上的数字，并且逐行比对两套
// 后处理读出的文字——行数一样但内容不同，是最容易漏掉的回归。

fn ocr_bench(args: &[String]) -> Result<()> {
    let mut det = "both".to_string();
    let mut batch: Option<usize> = None;
    let mut threads: Option<usize> = None;
    let mut repeat = 3usize;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--det" => {
                det = args.get(i + 1).context("--det 需要一个值")?.clone();
                i += 2;
            }
            "--batch" => {
                batch = Some(
                    args.get(i + 1)
                        .context("--batch 需要一个数字")?
                        .parse()
                        .context("--batch 不是数字")?,
                );
                i += 2;
            }
            "--threads" => {
                threads = Some(
                    args.get(i + 1)
                        .context("--threads 需要一个数字")?
                        .parse()
                        .context("--threads 不是数字")?,
                );
                i += 2;
            }
            "--repeat" => {
                repeat = args
                    .get(i + 1)
                    .context("--repeat 需要一个数字")?
                    .parse::<usize>()
                    .context("--repeat 不是数字")?
                    .max(1);
                i += 2;
            }
            other => {
                files.push(other.to_string());
                i += 1;
            }
        }
    }
    if files.is_empty() {
        return Err(anyhow!("ocr-bench 需要至少一张图片"));
    }

    let modes: Vec<mhvision::DetPostprocess> = if det.eq_ignore_ascii_case("both") {
        vec![mhvision::DetPostprocess::Legacy, mhvision::DetPostprocess::Db]
    } else {
        vec![mhvision::DetPostprocess::parse(&det)
            .ok_or_else(|| anyhow!("--det 只接受 legacy / db / both"))?]
    };

    let base = mhvision::OcrConfig::from_env();
    let effective_batch = batch.unwrap_or(base.batch_size).max(1);
    let effective_threads = threads.unwrap_or(base.intra_threads);
    println!(
        "# batch={effective_batch} threads={effective_threads} repeat={repeat} mods={}",
        modes.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(",")
    );

    // 每个模式只建一次引擎：建会话本身几百毫秒，不能算进单图耗时。
    let mut engines: Vec<(mhvision::DetPostprocess, mhvision::OcrEngine)> = Vec::new();
    for m in &modes {
        let cfg = mhvision::OcrConfig {
            det: *m,
            batch_size: effective_batch,
            intra_threads: effective_threads,
            ..base.clone()
        };
        let engine = mhvision::OcrEngine::with_config(&models_dir(), cfg)
            .context("loading OCR models")?;
        engines.push((*m, engine));
    }

    let mut diff_total = 0usize;
    for f in &files {
        let img = load_image(f)?;
        let name = Path::new(f)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| f.clone());
        println!("\n== {name} ({}x{})", img.width(), img.height());

        let mut texts: Vec<(mhvision::DetPostprocess, Vec<String>)> = Vec::new();
        for (mode, engine) in engines.iter_mut() {
            let (mut best, mut sum) = (f64::MAX, 0.0f64);
            let (mut det_best, mut rec_best) = (f64::MAX, f64::MAX);
            let (mut lines_n, mut conf, mut last) = (0usize, 0.0f64, Vec::new());
            for _ in 0..repeat {
                let started = std::time::Instant::now();
                let (lines, timing) = engine.recognize_with_timing(&img)?;
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                best = best.min(ms);
                sum += ms;
                det_best = det_best.min(timing.det_ms);
                rec_best = rec_best.min(timing.rec_ms);
                lines_n = lines.len();
                conf = if lines.is_empty() {
                    0.0
                } else {
                    lines.iter().map(|l| l.confidence).sum::<f64>() / lines.len() as f64
                };
                last = lines.into_iter().map(|l| l.text).collect();
            }
            println!(
                "   {:<7} lines={:<4} conf={:.3}  min={:>6.0}ms (det={:>5.0} rec={:>5.0})  avg={:>6.0}ms",
                mode.as_str(),
                lines_n,
                conf,
                best,
                det_best,
                rec_best,
                sum / repeat as f64
            );
            texts.push((*mode, last));
        }

        if texts.len() == 2 {
            let a: std::collections::BTreeSet<&String> = texts[0].1.iter().collect();
            let b: std::collections::BTreeSet<&String> = texts[1].1.iter().collect();
            let only_a: Vec<&&String> = a.difference(&b).collect();
            let only_b: Vec<&&String> = b.difference(&a).collect();
            println!(
                "   text: {} only={}  {} only={}",
                texts[0].0.as_str(),
                only_a.len(),
                texts[1].0.as_str(),
                only_b.len()
            );
            for t in only_a.iter().take(6) {
                println!("     - {t}");
            }
            for t in only_b.iter().take(6) {
                println!("     + {t}");
            }
            diff_total += only_a.len() + only_b.len();
        }
    }

    println!("\n# 文本差异合计 {diff_total} 条（0 = 两套后处理在同一批图上读出完全相同的文字）");
    Ok(())
}


// ---------------------------------------------------------------------------
// 批量评估
// ---------------------------------------------------------------------------
//
// 识别质量最怕的是"改了一处启发式，只有那几张手测过的图还记得测"。这里把
// 数据集做成目录约定，一条命令跑全量，任何一个字段退化都会让退出码变成 1，
// 可以直接挂到 CI 或发版前的检查里。
//
// 目录结构：
//   dataset/
//     case-001/
//       expect.json      { "nickname": "莫莫莫", "gameId": "429277361",
//                          "gold": 2625812, "screen": "bag" }
//       1.png  2.png     这一批会被当成同一角色的多张截图
//
// 只写关心的字段即可；没写的字段不参与比较，也就不会因为"没标"而失败。

/// 一条用例的判定结果。
struct CaseOutcome {
    name: String,
    failures: Vec<String>,
    observed: serde_json::Value,
}

/// 数据集里一个目录算一例；每个目录的截图合成一次批次。
fn eval(args: &[String]) -> Result<()> {
    let dir_arg = args
        .get(1)
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| anyhow!("eval needs a dataset directory"))?;
    let root = PathBuf::from(dir_arg);
    let json_out = args
        .iter()
        .position(|a| a == "--json")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root)
        .with_context(|| format!("reading dataset dir {}", root.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    if cases.is_empty() {
        return Err(anyhow!(
            "{} 里没有用例目录（每个子目录放 expect.json + 截图）",
            root.display()
        ));
    }

    let mut pipeline =
        mhvision::RecognitionPipeline::new(&models_dir()).context("loading pipeline")?;
    let store = mhstore::Store::open_in_data_dir(&data_dir())?;
    let known: Vec<mhcore::Character> = store
        .list_characters()?
        .into_iter()
        .map(|d| d.character)
        .collect();
    let refs = store.list_identity_refs()?;

    let mut outcomes: Vec<CaseOutcome> = Vec::new();
    let mut field_stats: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();

    for dir in &cases {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let expect_path = dir.join("expect.json");
        let expect: serde_json::Value = match std::fs::read_to_string(&expect_path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", expect_path.display()))?,
            Err(_) => serde_json::json!({}),
        };

        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_image_path(p))
            .collect();
        files.sort();
        if files.is_empty() {
            outcomes.push(CaseOutcome {
                name,
                failures: vec!["目录里没有截图".into()],
                observed: serde_json::Value::Null,
            });
            continue;
        }

        let images: Vec<image::RgbaImage> = files
            .iter()
            .map(|p| load_image(&p.to_string_lossy()))
            .collect::<Result<_>>()?;
        let result = pipeline.recognize_batch_with(&images, &known, &refs, &mhvision::NoControl)?;

        let mut failures: Vec<String> = Vec::new();
        let mut note = |field: &str, ok: bool, detail: String, failures: &mut Vec<String>| {
            let entry = field_stats.entry(field.to_string()).or_insert((0, 0));
            entry.1 += 1;
            if ok {
                entry.0 += 1;
            } else {
                failures.push(format!("{field}: {detail}"));
            }
        };

        if let Some(want) = expect.get("nickname").and_then(|v| v.as_str()) {
            let got = result.character.nickname.clone().unwrap_or_default();
            let ok = got.trim() == want.trim();
            note("nickname", ok, format!("期望 {want}，实际 {got}"), &mut failures);
        }
        if let Some(want) = expect.get("gameId").and_then(|v| v.as_str()) {
            let got = result.character.game_id.clone().unwrap_or_default();
            let ok = got == want;
            note("gameId", ok, format!("期望 {want}，实际 {got}"), &mut failures);
        }
        for (field, got) in [
            ("gold", result.currencies.gold.as_ref().map(|v| v.value)),
            ("silver", result.currencies.silver.as_ref().map(|v| v.value)),
        ] {
            if let Some(want) = expect.get(field).and_then(|v| v.as_i64()) {
                note(field, got == Some(want), format!("期望 {want}，实际 {got:?}"), &mut failures);
            }
        }
        if let Some(want) = expect.get("screen").and_then(|v| v.as_str()) {
            let expected = match want {
                "bag" | "inventory" => mhcore::ScreenKind::Inventory,
                "attr" | "attributes" => mhcore::ScreenKind::CharacterAttributes,
                _ => mhcore::ScreenKind::Unknown,
            };
            let ok = result.screens.contains(&expected);
            note("screen", ok, format!("期望 {want}，实际 {:?}", result.screens), &mut failures);
        }

        let observed = serde_json::json!({
            "nickname": result.character.nickname,
            "gameId": result.character.game_id,
            "level": result.character.level,
            "gold": result.currencies.gold.as_ref().map(|v| v.value),
            "silver": result.currencies.silver.as_ref().map(|v| v.value),
            "screens": result.screens,
            "identitySource": result.identity_source,
            "conflicts": result.conflicts,
            "warnings": result.warnings,
        });
        outcomes.push(CaseOutcome { name, failures, observed });
    }

    // ---- 报告 ----
    println!("dataset: {} ({} 例)", root.display(), outcomes.len());
    let mut failed = 0usize;
    for o in &outcomes {
        if o.failures.is_empty() {
            println!("  PASS  {}", o.name);
        } else {
            failed += 1;
            println!("  FAIL  {}", o.name);
            for f in &o.failures {
                println!("          {f}");
            }
        }
    }

    println!("\n字段准确率:");
    for (field, (ok, total)) in &field_stats {
        let pct = if *total == 0 { 0.0 } else { *ok as f64 * 100.0 / *total as f64 };
        println!("  {field:<10} {ok}/{total}  {pct:.1}%");
    }
    println!("\n合计 {}/{} 例通过", outcomes.len() - failed, outcomes.len());

    if let Some(path) = json_out {
        let fields: serde_json::Map<String, serde_json::Value> = field_stats
            .iter()
            .map(|(k, (ok, total))| {
                (k.clone(), serde_json::json!({ "ok": ok, "total": total }))
            })
            .collect();
        let report = serde_json::json!({
            "dataset": root.to_string_lossy(),
            "total": outcomes.len(),
            "passed": outcomes.len() - failed,
            "cases": outcomes.iter().map(|o| serde_json::json!({
                "name": o.name,
                "passed": o.failures.is_empty(),
                "failures": o.failures,
                "observed": o.observed,
            })).collect::<Vec<_>>(),
            "fields": fields,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
        println!("report: {}", path.display());
    }

    if failed > 0 {
        return Err(anyhow!("{failed} 例未通过"));
    }
    Ok(())
}

fn is_image_path(p: &Path) -> bool {
    match p.extension().and_then(|s| s.to_str()) {
        Some(ext) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "bmp" | "webp"
        ),
        None => false,
    }
}
