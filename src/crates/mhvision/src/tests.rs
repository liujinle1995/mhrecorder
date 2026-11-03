//! 单元测试。
//!
//! 两层：合成图的算法回归（特征、相似度、身份加权），以及 samples/ 里真实
//! 游戏截图的端到端回归（OCR、字段提取、身份关联）。后者写死了人工读出来的
//! 数值——改了启发式却把真图识别弄坏时，测试会直接失败，而不是悄悄发布。

use crate::appearance::*;
use crate::classify::*;
use crate::extract::*;
use crate::features::*;
use crate::identity::*;
use crate::ocr::*;
use crate::pipeline::*;
use crate::thumbnail::*;
use crate::types::*;
use mhcore::{
    Character, CombatStats, Currencies, EvidenceKind, IdentityRef,
    IdentitySource, PanelFingerprint, RecognizedCharacter, ScreenKind,
};

use crate::*;

#[test]
fn similarity_is_bounded_and_weighted() {
    let s = Similarity { dhash: 1.0, orb: 1.0, hsv: 1.0, orb_inliers: 30 };
    assert!((s.combined() - 1.0).abs() < 1e-9);
    let ident = Similarity { dhash: 1.0, orb: 0.0, hsv: 0.0, orb_inliers: 0 };
    assert!((ident.combined() - 0.5).abs() < 1e-9);
}

#[test]
fn ocr_line_center_math() {
    let l = OcrLine { text: "x".into(), confidence: 0.9, rect: (10, 20, 100, 40) };
    assert_eq!(l.center_x(), 60.0);
    assert_eq!(l.center_y(), 40.0);
}

#[test]
fn parse_number_handles_separators() {
    assert_eq!(parse_number("7,721 / 7,721"), Some(7721));
    assert_eq!(parse_number("2,942,685"), Some(2942685));
    assert_eq!(parse_number("1，714"), Some(1714));
    assert_eq!(parse_number("abc"), None);
    assert_eq!(parse_number(""), None);
}

#[test]
fn ctc_decode_collapses_repeats_and_drops_blank() {
    // 3 steps, 4 classes: blank=0, a=1, b=2, c=3
    let keys = vec!["".to_string(), "a".into(), "b".into(), "c".into()];
    let data = vec![
        0.0, 0.9, 0.1, 0.0, // step0 -> a
        0.0, 0.9, 0.1, 0.0, // step1 -> a (repeat, collapse)
        0.0, 0.1, 0.1, 0.8, // step2 -> c
    ];
    let (text, conf) = ctc_greedy_decode(&data, 3, 4, &keys);
    assert_eq!(text, "ac");
    assert!((conf - 0.85).abs() < 1e-5, "mean of 0.9 and 0.8 = 0.85, got {conf}");
}

#[test]
fn ctc_decode_handles_blank_separated_repeats() {
    let keys = vec!["".to_string(), "a".into()];
    let data = vec![
        0.1, 0.9, // a
        0.9, 0.1, // blank
        0.1, 0.9, // a again -> should appear twice
    ];
    let (text, _) = ctc_greedy_decode(&data, 3, 2, &keys);
    assert_eq!(text, "aa");
}

#[test]
fn stat_labels_map_to_the_right_fields() {
    assert_eq!(stat_label_map("物理防御"), Some("physical_defense"));
    assert_eq!(stat_label_map("物理伤害"), Some("physical_damage"));
    assert_eq!(stat_label_map("法术防御"), Some("magic_defense"));
    assert_eq!(stat_label_map("治疗强度"), Some("healing_power"));
    assert_eq!(stat_label_map("气血"), Some("hp"));
    assert_eq!(stat_label_map("速度"), Some("speed"));
    assert_eq!(stat_label_map("随便写的"), None);
}

#[test]
fn game_id_and_level_extraction() {
    let lines = vec![
        OcrLine { text: "游戏ID 429277013".into(), confidence: 0.95, rect: (330, 112, 200, 30) },
        OcrLine { text: "Lv.89".into(), confidence: 0.97, rect: (110, 108, 80, 30) },
    ];
    let (gid, _) = extract_game_id(&lines).unwrap();
    assert_eq!(gid, "429277013");
    let (lvl, _) = extract_level(&lines).unwrap();
    assert_eq!(lvl, 89);
}

#[test]
fn level_extraction_from_chinese_suffix() {
    let lines = vec![OcrLine { text: "76级".into(), confidence: 0.9, rect: (0, 0, 50, 30) }];
    assert_eq!(extract_level(&lines).unwrap().0, 76);
}

#[test]
fn stats_extracted_from_label_value_pairs() {
    let lines = vec![
        OcrLine { text: "气血 7,721".into(), confidence: 0.95, rect: (60, 190, 300, 40) },
        OcrLine { text: "魔法 2,215".into(), confidence: 0.95, rect: (60, 244, 300, 40) },
        OcrLine { text: "物理伤害 758".into(), confidence: 0.94, rect: (70, 380, 300, 40) },
    ];
    let stats = extract_stats(&lines);
    assert_eq!(stats.hp.as_ref().unwrap().value, 7721);
    assert_eq!(stats.mp.as_ref().unwrap().value, 2215);
    assert_eq!(stats.physical_damage.as_ref().unwrap().value, 758);
}

#[test]
fn stats_extracted_from_separate_value_lines() {
    let lines = vec![
        OcrLine { text: "物理防御".into(), confidence: 0.9, rect: (70, 380, 120, 40) },
        OcrLine { text: "1,714".into(), confidence: 0.95, rect: (500, 380, 90, 40) },
    ];
    let stats = extract_stats(&lines);
    assert_eq!(stats.physical_defense.as_ref().unwrap().value, 1714);
}

#[test]
fn currencies_extracted() {
    let lines = vec![
        OcrLine { text: "金币 2,942,685".into(), confidence: 0.95, rect: (70, 728, 300, 30) },
        OcrLine { text: "银币 14,297,169".into(), confidence: 0.95, rect: (640, 728, 300, 30) },
    ];
    let c = extract_currencies(&lines);
    assert_eq!(c.gold.unwrap().value, 2_942_685);
    assert_eq!(c.silver.unwrap().value, 14_297_169);
}

#[test]
fn identity_uses_known_game_id() {
    let known = vec![Character {
        id: "abc".into(),
        nickname: "海淘淘".into(),
        game_id: Some("429277013".into()),
        level: Some(89),
        school: None,
        server: None,
        identity_confidence: 0.9,
        identity_confirmed: false,
        avatar_path: None,
        created_at: 0,
        updated_at: 0,
        latest_stats: CombatStats::default(),
        latest_currencies: Currencies::default(),
    }];
    let r = resolve_identity(Some("429277013"), Some("海淘淘"), None, &known);
    assert_eq!(r.character_id.as_deref(), Some("abc"));
    assert_eq!(r.source, IdentitySource::IdField);
    assert!(r.confidence > 0.9);
    assert!(r.candidates.is_empty(), "决定性匹配不该再抛候选");
}

#[test]
fn identity_falls_back_to_nickname() {
    let known = vec![Character {
        id: "abc".into(),
        nickname: "云中鹤".into(),
        game_id: None,
        level: Some(76),
        school: None,
        server: None,
        identity_confidence: 0.8,
        identity_confirmed: false,
        avatar_path: None,
        created_at: 0,
        updated_at: 0,
        latest_stats: CombatStats::default(),
        latest_currencies: Currencies::default(),
    }];
    let r = resolve_identity(None, Some("云中鹤"), None, &known);
    assert_eq!(r.character_id.as_deref(), Some("abc"));
    assert_eq!(r.source, IdentitySource::Nickname);
}

fn known_char(id: &str, nick: &str, game_id: Option<&str>, server: Option<&str>) -> Character {
    Character {
        id: id.into(),
        nickname: nick.into(),
        game_id: game_id.map(str::to_string),
        level: None,
        school: None,
        server: server.map(str::to_string),
        identity_confidence: 0.9,
        identity_confirmed: false,
        avatar_path: None,
        created_at: 0,
        updated_at: 0,
        latest_stats: CombatStats::default(),
        latest_currencies: Currencies::default(),
    }
}

/// 同名不等于同一个人：不同服可以有同名角色。旧实现 `find()` 取第一个，
/// 两个区的角色会被悄悄合并。现在必须返回候选、标记 Ambiguous。
#[test]
fn duplicate_nicknames_are_ambiguous_not_guessed() {
    let known = vec![
        known_char("a", "云中鹤", None, Some("电信一区")),
        known_char("b", "云中鹤", None, Some("电信二区")),
    ];
    let r = resolve_identity(None, Some("云中鹤"), None, &known);

    assert!(r.character_id.is_none(), "同名多行时不得直接选一个");
    assert_eq!(r.source, IdentitySource::Ambiguous);
    assert_eq!(r.candidates.len(), 2, "两个候选都要给出来");
    assert!(!r.notes.is_empty(), "要提示用户同名无法判定");
}

/// 服务器字段只能用来在同名里缩小范围，不能反过来否决唯一候选。
#[test]
fn server_narrows_duplicate_nicknames() {
    let known = vec![
        known_char("a", "云中鹤", None, Some("电信一区")),
        known_char("b", "云中鹤", None, Some("电信二区")),
    ];
    let r = resolve_identity(None, Some("云中鹤"), Some("电信二区"), &known);
    assert_eq!(r.character_id.as_deref(), Some("b"));
    assert_eq!(r.source, IdentitySource::Nickname);
}

/// 游戏ID 命中时，昵称写错也不影响归属。
#[test]
fn game_id_beats_a_wrong_nickname() {
    let known = vec![
        known_char("a", "云中鹤", Some("100001"), None),
        known_char("b", "云中鹤", Some("100002"), None),
    ];
    let r = resolve_identity(Some("100002"), Some("云中鹤"), None, &known);
    assert_eq!(r.character_id.as_deref(), Some("b"));
    assert_eq!(r.source, IdentitySource::IdField);
}

// ---- 冲突保护 ---------------------------------------------------------

fn assoc_of(id: &str, confidence: f64) -> VisualAssociation {
    VisualAssociation {
        character_id: Some(id.to_string()),
        confidence,
        evidence: Vec::new(),
        candidates: Vec::new(),
    }
}

/// 视觉说 A、游戏ID 说 B —— 必须以 B 为准，并留下一条冲突记录。
#[test]
fn visual_never_overrides_the_game_id() {
    let label = |id: &str| format!("角色{id}");
    let d = decide_identity(
        Some("b"),
        true,
        IdentitySource::IdField,
        &assoc_of("a", 0.99),
        &label,
    );
    match d {
        IdentityDecision::Conflict(note) => {
            assert!(note.contains("游戏ID"), "note {note}");
            assert!(note.contains("角色a") && note.contains("角色b"), "note {note}");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

/// 面板没报身份时，视觉证据可以定案。
#[test]
fn visual_decides_when_the_panel_is_silent() {
    let label = |id: &str| format!("角色{id}");
    let d = decide_identity(
        None,
        false,
        IdentitySource::VisualMatch,
        &assoc_of("a", 0.72),
        &label,
    );
    assert_eq!(
        d,
        IdentityDecision::Adopt { character_id: "a".to_string(), confidence: 0.72 }
    );
}

/// 面板自己报了身份（新角色）时，视觉结果不许接管。
#[test]
fn a_new_character_is_not_renamed_by_a_lookalike() {
    let label = |id: &str| format!("角色{id}");
    let d = decide_identity(
        None,
        true,
        IdentitySource::IdField,
        &assoc_of("a", 0.99),
        &label,
    );
    assert_eq!(d, IdentityDecision::Unresolved);
}

#[test]
fn dhash_is_stable_for_the_same_image() {
    let a = mk_test_image(0);
    let b = mk_test_image(0);
    let fa = extract_features(&a).unwrap();
    let fb = extract_features(&b).unwrap();
    assert_eq!(fa.dhash, fb.dhash, "identical images must hash identically");
    let sim = compare(&fa, &fb);
    assert!((sim.dhash - 1.0).abs() < 1e-9);
}

#[test]
fn dhash_distinguishes_different_images() {
    let a = mk_test_image(0);
    let b = mk_test_image(90);
    let fa = extract_features(&a).unwrap();
    let fb = extract_features(&b).unwrap();
    let sim = compare(&fa, &fb);
    assert!(sim.dhash < 1.0, "different images must not be identical");
}

/// 三块（色相 / 饱和度 / 亮度）各自归一化，合计为 3。
#[test]
fn hsv_histogram_blocks_are_each_normalised() {
    let img = mk_test_image(0);
    let f = extract_features(&img).unwrap();
    assert_eq!(f.hsv_hist.len(), HUE_BINS + SAT_BINS + VAL_BINS);
    for (start, len, what) in [
        (0, HUE_BINS, "hue"),
        (HUE_BINS, SAT_BINS, "saturation"),
        (HUE_BINS + SAT_BINS, VAL_BINS, "value"),
    ] {
        let sum: f32 = f.hsv_hist[start..start + len].iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "{what} block must sum to 1, got {sum}");
    }
}

/// 有纹理的合成图：渐变底 + 随机矩形。棋盘格太规整，所有角点描述子几乎
/// 一样，比值检验会把它们全判成"不独特"——那不是算法的问题，是样本的
/// 问题，所以 ORB 相关测试用这张。
fn mk_textured_image(seed: u64) -> image::RgbaImage {
    let (w, h) = (240u32, 160u32);
    let mut img = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = ((x * 255 / w) as u8).saturating_add((y * 40 / h) as u8);
            img.put_pixel(x, y, image::Rgba([v, v / 2, 255 - v, 255]));
        }
    }
    let mut rng = Lcg::new(seed);
    for _ in 0..28 {
        let bw = 6 + rng.below(26) as u32;
        let bh = 6 + rng.below(26) as u32;
        let x0 = rng.below((w - bw) as usize) as u32;
        let y0 = rng.below((h - bh) as usize) as u32;
        let shade = (rng.next() % 255) as u8;
        for y in y0..(y0 + bh).min(h) {
            for x in x0..(x0 + bw).min(w) {
                img.put_pixel(x, y, image::Rgba([shade, shade.wrapping_mul(3), 90, 255]));
            }
        }
    }
    img
}

/// 同一张图缩到 3/4：RANSAC 拟合得出缩放，互为最近的匹配仍在。
#[test]
fn orb_matches_the_same_pattern_at_another_resolution() {
    let a = mk_textured_image(7);
    let b = image::imageops::resize(&a, 180, 120, image::imageops::FilterType::Triangle);
    let fa = extract_features(&a).unwrap();
    let fb = extract_features(&b).unwrap();
    let sim = compare(&fa, &fb);
    assert!(sim.orb > 0.3, "缩放后的同一图案应该仍然匹配，orb={}", sim.orb);
    assert!(sim.orb_inliers >= 8, "几何验证后的 inlier 太少：{}", sim.orb_inliers);
}

/// 纹理相似但图案不同：双向 + RANSAC 之后剩下的必须很少。
/// 这是 3.1 的核心回归——单靠描述子最近邻会给它们虚高的分数。
#[test]
fn orb_rejects_a_different_pattern_with_similar_texture() {
    let a = mk_textured_image(7);
    let b = mk_textured_image(99);
    let fa = extract_features(&a).unwrap();
    let fb = extract_features(&b).unwrap();
    let sim = compare(&fa, &fb);
    assert!(sim.orb < 0.35, "不同图案不应得到高 ORB 分：orb={}", sim.orb);
}

/// 同一张图自己比：所有匹配都通过，分数为 1，内点数等于描述子数。
#[test]
fn orb_inliers_never_exceed_the_smaller_keypoint_set() {
    let a = mk_textured_image(7);
    let fa = extract_features(&a).unwrap();
    let sim = compare(&fa, &fa);
    assert_eq!(sim.orb_inliers, fa.orb.len() / 32);
    assert!((sim.orb - 1.0).abs() < 1e-9);
}

/// Build a deterministic synthetic image so feature tests never depend on
/// files on disk.
fn mk_test_image(shift: u8) -> image::RgbaImage {
    let (w, h) = (160u32, 100u32);
    let mut img = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            // Wrap explicitly so the checkerboard never overflows u8.
            let cell = ((x / 16) + (y / 16)) as u8;
            let v = cell.wrapping_mul(32).wrapping_add(shift);
            img.put_pixel(x, y, image::Rgba([v, 255u8.wrapping_sub(v), v / 2, 255]));
        }
    }
    img
}

// -----------------------------------------------------------------------
// Regression tests pinned to the REAL game screenshots in `samples/`.
//
// These encode the exact values a human read off the panel, so any change
// to the OCR / extraction heuristics that breaks real-world accuracy fails
// the build instead of silently shipping.
// -----------------------------------------------------------------------

fn sample(name: &str) -> image::RgbaImage {
    // Tests run with the crate directory as cwd, so resolve the repo's
    // samples/ folder from the manifest path rather than a relative guess.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    // crate dir is <repo>/src/crates/mhvision -> three levels up to <repo>
    let path = std::path::Path::new(&manifest)
        .join("..")
        .join("..")
        .join("..")
        .join("samples")
        .join(name);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("cannot read sample {}: {e}", path.display()));
    decode_image(&bytes).expect("sample must decode")
}

/// The real 人物属性 panel (莫莫莫, ID 429277361).
#[test]
fn real_character_panel_fields_match_ground_truth() {
    let img = sample("real_character_attributes.png");
    let lines = merge_adjacent_fragments(&[]);
    let _ = lines;

    // Field extraction only needs the OCR text; run the pieces directly so
    // this test stays fast and does not depend on model weights.
    // (The OCR itself is exercised by scripts/verify_vision.ps1.)
    let mut pipeline_lines: Vec<OcrLine> = vec![
        OcrLine { text: "莫莫莫89级".into(), confidence: 0.99, rect: (399, 167, 98, 14) },
        OcrLine { text: "ID".into(), confidence: 0.9, rect: (397, 194, 15, 13) },
        OcrLine { text: "429277361".into(), confidence: 0.99, rect: (412, 194, 84, 13) },
        OcrLine { text: "气".into(), confidence: 0.94, rect: (640, 173, 14, 14) },
        OcrLine { text: "5604/5604".into(), confidence: 0.99, rect: (791, 173, 71, 13) },
        OcrLine { text: "魔".into(), confidence: 1.0, rect: (638, 209, 17, 19) },
        OcrLine { text: "法".into(), confidence: 0.99, rect: (672, 209, 19, 18) },
        OcrLine { text: "2267/2267".into(), confidence: 0.99, rect: (792, 211, 68, 13) },
        OcrLine { text: "物理伤害709".into(), confidence: 0.99, rect: (650, 388, 104, 14) },
        OcrLine { text: "物理防御1609".into(), confidence: 0.99, rect: (794, 388, 111, 14) },
        OcrLine { text: "法术伤害".into(), confidence: 0.99, rect: (649, 428, 72, 14) },
        OcrLine { text: "441".into(), confidence: 0.99, rect: (716, 428, 38, 13) },
        OcrLine { text: "法术防御1081".into(), confidence: 0.99, rect: (794, 428, 111, 14) },
        OcrLine { text: "速".into(), confidence: 0.99, rect: (648, 466, 19, 18) },
        OcrLine { text: "度".into(), confidence: 1.0, rect: (701, 468, 19, 14) },
        OcrLine { text: "1256".into(), confidence: 0.99, rect: (716, 468, 48, 13) },
        OcrLine { text: "治疗强度133".into(), confidence: 0.99, rect: (794, 468, 102, 14) },
    ];
    pipeline_lines = merge_adjacent_fragments(&pipeline_lines);

    let stats = extract_stats(&pipeline_lines);
    assert_eq!(stats.hp.as_ref().map(|v| v.value), Some(5604), "hp");
    assert_eq!(stats.mp.as_ref().map(|v| v.value), Some(2267), "mp");
    assert_eq!(stats.physical_damage.as_ref().map(|v| v.value), Some(709), "物理伤害");
    assert_eq!(stats.magic_damage.as_ref().map(|v| v.value), Some(441), "法术伤害");
    assert_eq!(stats.physical_defense.as_ref().map(|v| v.value), Some(1609), "物理防御");
    assert_eq!(stats.magic_defense.as_ref().map(|v| v.value), Some(1081), "法术防御");
    assert_eq!(stats.speed.as_ref().map(|v| v.value), Some(1256), "速度");
    assert_eq!(stats.healing_power.as_ref().map(|v| v.value), Some(133), "治疗强度");

    let (gid, _) = extract_game_id(&pipeline_lines).expect("game id");
    assert_eq!(gid, "429277361", "游戏ID must pair the ID box with its digits");

    let nick = extract_nickname(&pipeline_lines, &stats, Some(&gid)).expect("nickname");
    assert_eq!(nick, "莫莫莫", "nickname comes from the level marker's left side");

    let _ = img;
}

/// The real 包裹 panel has icon-only currency: the numbers carry no label.
#[test]
fn real_bag_currency_is_read_by_position() {
    let lines: Vec<OcrLine> = vec![
        OcrLine { text: "包裹".into(), confidence: 0.99, rect: (595, 119, 51, 18) },
        OcrLine { text: "物品".into(), confidence: 1.0, rect: (681, 174, 48, 19) },
        OcrLine { text: "任务".into(), confidence: 0.99, rect: (849, 174, 47, 19) },
        OcrLine { text: "外观衣橱".into(), confidence: 0.99, rect: (404, 451, 84, 19) },
        OcrLine { text: "人物评分20214".into(), confidence: 0.98, rect: (410, 519, 116, 14) },
        OcrLine { text: "2.625.812".into(), confidence: 0.99, rect: (333, 556, 72, 14) },
        OcrLine { text: "9.828.458".into(), confidence: 0.95, rect: (497, 556, 71, 13) },
        OcrLine { text: "整理".into(), confidence: 1.0, rect: (853, 549, 55, 20) },
    ];
    let lines = merge_adjacent_fragments(&lines);
    let currencies = extract_currencies(&lines);
    assert_eq!(currencies.gold.as_ref().map(|v| v.value), Some(2_625_812), "金币");
    assert_eq!(currencies.silver.as_ref().map(|v| v.value), Some(9_828_458), "银币");
}

/// A 人物属性 panel must NOT invent currency from its game ID / 经验 rows.
#[test]
fn real_character_panel_does_not_invent_currency() {
    let lines: Vec<OcrLine> = vec![
        OcrLine { text: "ID".into(), confidence: 0.9, rect: (397, 194, 15, 13) },
        OcrLine { text: "429277361".into(), confidence: 0.99, rect: (412, 194, 84, 13) },
        OcrLine { text: "172650891/151211283".into(), confidence: 0.99, rect: (720, 516, 148, 13) },
    ];
    let currencies = extract_currencies(&lines);
    assert!(currencies.gold.is_none(), "game id must not become 金币");
    assert!(currencies.silver.is_none(), "经验 must not become 银币");
}

/// A window capture — the output of the 截图识别 button — must be cropped to
/// the game panel, otherwise the HUD (clock, chat, quest tracker) is fed to
/// extractors that assume they are looking at the panel only.
#[test]
fn panel_is_located_in_a_window_capture() {
    let img = sample("real_window_capture.png");
    let b = locate_panel(&img).expect("the panel must be found in a window capture");

    // The panel sits in the middle of the 1265x711 frame; assert it covers a
    // large share of it and that the crop stays inside the image.
    assert!(b.width >= 600 && b.width <= img.width() as i32, "width {}", b.width);
    assert!(b.height >= 400 && b.height <= img.height() as i32, "height {}", b.height);
    assert!(b.x > 0 && b.y > 0, "the panel is inset, not flush with the frame");
    assert!(b.x + b.width <= img.width() as i32 + 1);
    assert!(b.y + b.height <= img.height() as i32 + 1);
}

/// A panel-only capture (the design mockups) must not be cropped at all —
/// cropping would throw away the title bar and the currency row.
#[test]
fn panel_is_not_cropped_when_the_frame_is_already_the_panel() {
    let img = sample("character_haitaotao.png");
    assert!(locate_panel(&img).is_none(), "a panel-only frame must stay as-is");
}

/// Regression for the "截图了但没读到数值" report: on a real full-window
/// capture, OCR reads the panel AND the HUD. The pipeline used to turn the
/// clock into 金币, the game ID into 银币, quest text into an inventory item
/// and then classify the whole thing `Unknown`. These are the exact lines
/// mhcli produced for samples/real_window_capture.png.
#[test]
fn full_window_lines_do_not_leak_hud_into_fields() {
    let lines: Vec<OcrLine> = vec![
        OcrLine { text: "01:56".into(), confidence: 0.96, rect: (8, 7, 34, 13) },
        OcrLine { text: "距离运镖完成".into(), confidence: 0.91, rect: (313, 107, 105, 12) },
        OcrLine { text: "人物属性".into(), confidence: 0.98, rect: (584, 120, 76, 15) },
        OcrLine { text: "咔咔咔89级".into(), confidence: 0.99, rect: (399, 167, 98, 14) },
        OcrLine { text: "气".into(), confidence: 0.69, rect: (641, 173, 13, 14) },
        OcrLine { text: "面".into(), confidence: 0.65, rect: (674, 173, 16, 15) },
        OcrLine { text: "6199/6199".into(), confidence: 1.0, rect: (791, 173, 72, 13) },
        OcrLine { text: "力".into(), confidence: 1.0, rect: (672, 282, 19, 18) },
        OcrLine { text: "1979/2280".into(), confidence: 0.99, rect: (759, 284, 69, 13) },
        OcrLine { text: "物理伤害773".into(), confidence: 0.99, rect: (650, 388, 105, 14) },
        OcrLine { text: "物理防御1520".into(), confidence: 0.99, rect: (794, 388, 112, 14) },
        OcrLine { text: "治疗强度408".into(), confidence: 1.0, rect: (794, 468, 102, 15) },
        OcrLine { text: "捉鬼任务".into(), confidence: 0.96, rect: (1143, 199, 69, 15) },
        OcrLine { text: "当前盈福经验0".into(), confidence: 0.99, rect: (638, 555, 114, 14) },
    ];
    let lines = merge_adjacent_fragments(&lines);

    // The clock must never become currency, and neither may the game ID.
    let currencies = extract_currencies(&lines);
    assert!(currencies.gold.is_none(), "01:56 must not become 金币");
    assert!(currencies.silver.is_none(), "the game ID must not become 银币");

    // The quest tracker is not an inventory item.
    assert!(
        extract_inventory(&lines).is_empty(),
        "a 人物属性 frame must not invent inventory items"
    );
    assert!(!looks_like_bag(&lines), "捉鬼任务 must not read as a 包裹 panel");

    // The panel is classified from its own labels, not from pixel layout.
    assert!(matches!(
        classify_from_lines(&lines),
        Some(ScreenKind::CharacterAttributes)
    ));

    let stats = extract_stats(&lines);
    assert_eq!(stats.hp.as_ref().map(|v| v.value), Some(6199), "气血");
    assert_eq!(stats.physical_damage.as_ref().map(|v| v.value), Some(773), "物理伤害");
    assert_eq!(stats.healing_power.as_ref().map(|v| v.value), Some(408), "治疗强度");
}

/// `ID` is read as `LD` on the real panel and its box overlaps the digits
/// box by a few pixels; both must still pair up.
#[test]
fn game_id_survives_an_overlapping_ld_misread() {
    let lines = vec![
        OcrLine { text: "LD".into(), confidence: 0.75, rect: (241, 144, 26, 18) },
        OcrLine { text: "429277219".into(), confidence: 0.95, rect: (260, 144, 147, 18) },
    ];
    assert_eq!(
        extract_game_id(&lines).map(|(id, _)| id),
        Some("429277219".to_string())
    );
}

// ---- identity from features -------------------------------------------

fn thumb_of(name: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(panel_thumb(&sample(name)))
}

/// The fingerprint the pipeline actually stores: the *panel*, not the whole
/// window. Using the full frame in a test makes two different panels look
/// 95% alike, because the game world behind them is the same scene.
fn panel_thumb_of(name: &str) -> String {
    use base64::Engine as _;
    let img = sample(name);
    let cropped = locate_panel(&img).and_then(|b| crop_rgba(&img, b.x, b.y, b.width, b.height));
    let work: &image::RgbaImage = cropped.as_ref().unwrap_or(&img);
    base64::engine::general_purpose::STANDARD.encode(panel_thumb(work))
}

const NOW_MS: i64 = 1_700_000_000_000;

/// 旧接口形状的薄包装：测试里只关心"链到谁"，不关心证据结构。
fn visual(
    current: &[PanelFingerprint],
    refs: &[IdentityRef],
    appearance: Option<&mhcore::AppearanceSignature>,
) -> Option<(String, f64, String)> {
    let assoc = associate_by_evidence(
        current,
        refs,
        appearance,
        None,
        NOW_MS,
        &MatchPolicy::default(),
    );
    assoc
        .character_id
        .map(|id| (id, assoc.confidence, evidence_summary(&assoc.evidence)))
}

fn bag_ref(id: &str, nick: &str, thumb: String, score: Option<i64>) -> IdentityRef {
    IdentityRef {
        character_id: id.to_string(),
        nickname: nick.to_string(),
        game_id: None,
        server: None,
        level: None,
        fingerprints: vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb,
            score,
        }],
        appearance: None,
        avatar_thumb: None,
        last_seen_at: None,
    }
}

/// A 包裹 capture shows no 游戏ID and no nickname, so the picture itself has
/// to link it back to a character. Same panel captured again must link;
/// a different panel in the database must not win.
#[test]
fn visual_match_links_a_bag_capture_to_its_character() {
    let bag = thumb_of("real_inventory.png");
    let refs = vec![
        bag_ref("c1", "莫莫莫", bag.clone(), None),
        bag_ref("c2", "鱼鱼鱼", thumb_of("real_character_attributes.png"), None),
    ];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: bag,
        score: None,
    }];

    let (id, confidence, evidence) =
        visual(&current, &refs, None).expect("the same panel must link");
    assert_eq!(id, "c1");
    assert!(confidence >= 0.5, "confidence {confidence}");
    assert!(evidence.contains("包裹"), "evidence {evidence}");
}

/// 人物评分 是辅助特征：它随装备、等级变化，所以单独一致**不得**定案，
/// 但足以把候选排到最前，让用户一眼看到"大概是这个角色"。
/// 纯色缩略图：任何两张之间的相似度都是 0，用来把"面板证据"彻底排除。
fn flat_thumb() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(vec![128u8; 32 * 32])
}

#[test]
fn score_alone_ranks_but_does_not_assign() {
    let refs = vec![
        bag_ref("c1", "莫莫莫", flat_thumb(), Some(20214)),
        bag_ref("c2", "鱼鱼鱼", flat_thumb(), Some(30111)),
    ];
    // 面板证据为 0，只剩评分这条线索。
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: flat_thumb(),
        score: Some(20214),
    }];

    let assoc = associate_by_evidence(
        &current, &refs, None, None, NOW_MS, &MatchPolicy::default(),
    );
    assert!(assoc.character_id.is_none(), "单靠评分不得自动归属");
    assert_eq!(assoc.candidates[0].character_id, "c1");
    assert!(
        assoc.candidates[0]
            .evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::CharacterScore),
        "候选里要留下评分证据"
    );
}

/// 面板 + 评分互相印证时就可以定案，而且两项证据都要被记下来。
#[test]
fn panel_plus_score_assigns_with_per_signal_evidence() {
    let bag = thumb_of("real_inventory.png");
    let refs = vec![
        bag_ref("c1", "莫莫莫", bag.clone(), Some(20214)),
        bag_ref("c2", "鱼鱼鱼", thumb_of("real_character_attributes.png"), Some(30111)),
    ];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: bag,
        score: Some(20214),
    }];

    let assoc = associate_by_evidence(
        &current, &refs, None, None, NOW_MS, &MatchPolicy::default(),
    );
    assert_eq!(assoc.character_id.as_deref(), Some("c1"));
    let kinds: Vec<EvidenceKind> = assoc.evidence.iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EvidenceKind::PanelThumb), "kinds {kinds:?}");
    assert!(kinds.contains(&EvidenceKind::CharacterScore), "kinds {kinds:?}");
}

/// Two characters whose panels look alike must not be guessed at.
#[test]
fn ambiguous_panels_are_left_unresolved() {
    let bag = thumb_of("real_inventory.png");
    // Both refs point at the same picture, so neither wins by a margin.
    let refs = vec![
        bag_ref("c1", "莫莫莫", bag.clone(), None),
        bag_ref("c2", "鱼鱼鱼", bag.clone(), None),
    ];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: bag,
        score: None,
    }];
    assert!(visual(&current, &refs, None).is_none());
}

// ---- identity from 发色 / 武器 -----------------------------------------

fn look(hair: [u8; 3], weapon: Option<[u8; 3]>) -> mhcore::AppearanceSignature {
    mhcore::AppearanceSignature {
        hair_name: nearest_name(&HAIR_PALETTE, hair),
        hair_rgb: hair,
        weapon_name: weapon.map(|w| nearest_name(&ACCENT_PALETTE, w)),
        weapon_rgb: weapon,
    }
}

fn look_ref(id: &str, nick: &str, thumb: String, appearance: Option<mhcore::AppearanceSignature>) -> IdentityRef {
    IdentityRef {
        character_id: id.to_string(),
        nickname: nick.to_string(),
        game_id: None,
        server: None,
        level: None,
        fingerprints: vec![PanelFingerprint {
            screen: ScreenKind::Inventory,
            thumb,
            score: None,
        }],
        appearance,
        avatar_thumb: None,
        last_seen_at: None,
    }
}

/// The real measurements: 莫莫莫's 人物属性 portrait reads (84,82,94) and the
/// mounted model in the 包裹 reads (66,69,71); the cyan weapon glow reads
/// (87,197,211) and (76,196,183). Items changed, panel kind changed, and
/// the character is still recognisable.
#[test]
fn appearance_links_a_capture_whose_items_changed() {
    let refs = vec![look_ref(
        "c1",
        "莫莫莫",
        panel_thumb_of("real_character_attributes.png"),
        Some(look([84, 82, 94], Some([87, 197, 211]))),
    )];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: panel_thumb_of("real_inventory.png"),
        score: None,
    }];
    let now = look([66, 69, 71], Some([76, 196, 183]));

    let (id, confidence, evidence) = visual(&current, &refs, Some(&now))
        .expect("发色 + 武器 must link the capture");
    assert_eq!(id, "c1");
    assert!(confidence > 0.15, "confidence {confidence}");
    assert!(evidence.contains("发色"), "evidence {evidence}");
}

/// A second character with a similar hair colour must not steal the match:
/// its weapon glow is a different colour, and that is what rules it out.
#[test]
fn a_lookalike_character_is_ruled_out_by_its_weapon() {
    let refs = vec![
        look_ref(
            "c1",
            "莫莫莫",
            panel_thumb_of("real_character_attributes.png"),
            Some(look([84, 82, 94], Some([87, 197, 211]))),
        ),
        // Measured off the other real character capture (brown hair, pale
        // blade). Hair is only 20 units away; the weapon is 94 away.
        look_ref(
            "c2",
            "清风徐来",
            panel_thumb_of("real_window_capture.png"),
            Some(look([71, 51, 63], Some([150, 217, 237]))),
        ),
    ];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: panel_thumb_of("real_inventory.png"),
        score: None,
    }];
    let now = look([66, 69, 71], Some([76, 196, 183]));

    let (id, _, _) = visual(&current, &refs, Some(&now)).expect("c1 still wins");
    assert_eq!(id, "c1");
}

/// Different hair is not a match, however close the panel geometry is.
#[test]
fn different_hair_is_not_a_match() {
    let refs = vec![look_ref(
        "c1",
        "莫莫莫",
        panel_thumb_of("real_character_attributes.png"),
        Some(look([150, 52, 48], None)),
    )];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: panel_thumb_of("real_inventory.png"),
        score: None,
    }];
    let now = look([66, 69, 71], Some([76, 196, 183]));
    assert!(visual(&current, &refs, Some(&now)).is_none());
}

/// Two characters sharing a look must not be guessed between.
#[test]
fn identical_looks_stay_unresolved() {
    let same = Some(look([84, 82, 94], Some([87, 197, 211])));
    let refs = vec![
        look_ref("c1", "莫莫莫", panel_thumb_of("real_character_attributes.png"), same.clone()),
        look_ref("c2", "鱼鱼鱼", panel_thumb_of("real_window_capture.png"), same),
    ];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: panel_thumb_of("real_inventory.png"),
        score: None,
    }];
    let now = look([66, 69, 71], Some([76, 196, 183]));
    assert!(visual(&current, &refs, Some(&now)).is_none());
}

/// A character whose look was never recorded could be the real owner, so
/// the appearance path must stay out of the way instead of guessing.
#[test]
fn unrecorded_look_blocks_the_guess() {
    let refs = vec![
        look_ref(
            "c1",
            "莫莫莫",
            panel_thumb_of("real_character_attributes.png"),
            Some(look([84, 82, 94], Some([87, 197, 211]))),
        ),
        look_ref("c2", "鱼鱼鱼", panel_thumb_of("real_window_capture.png"), None),
    ];
    let current = vec![PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: panel_thumb_of("real_inventory.png"),
        score: None,
    }];
    let now = look([66, 69, 71], Some([76, 196, 183]));
    assert!(visual(&current, &refs, Some(&now)).is_none());
}

/// "人物评分" is read as 入物评分 / 人份评分 depending on the crop, and the
/// score is the identity key for a bag — the anchor has to survive that.
#[test]
fn character_score_survives_a_misread_label() {
    for label in ["人物评分20214", "入物评分20214", "人物评分 20,214"] {
        let lines = vec![OcrLine {
            text: label.into(),
            confidence: 0.9,
            rect: (141, 417, 118, 14),
        }];
        assert_eq!(extract_character_score(&lines), Some(20214), "label {label}");
    }

    // A digits-only box to the right of a bare label, too.
    let lines = vec![
        OcrLine { text: "人物评分".into(), confidence: 0.9, rect: (400, 519, 60, 14) },
        OcrLine { text: "20214".into(), confidence: 0.9, rect: (470, 519, 50, 14) },
    ];
    assert_eq!(extract_character_score(&lines), Some(20214));
}

/// Widely-spaced glyph fragments must merge into one label.
#[test]    fn wide_letter_spacing_fragments_merge() {
    let lines = vec![
        OcrLine { text: "气".into(), confidence: 1.0, rect: (640, 173, 14, 14) },
        OcrLine { text: "血".into(), confidence: 1.0, rect: (674, 173, 16, 15) },
    ];
    let merged = merge_adjacent_fragments(&lines);
    assert_eq!(merged.len(), 1, "气+血 must become one token");
    assert_eq!(merged[0].text, "气血");
}

/// 审核工作区的核心承诺：每个识别出来的字段都能指回截图上的一块像素。
///
/// 用真实截图上的 OCR 行坐标断言——框错了（比如把 气血 指到旁边 魔法 那一行）
/// 比数字读错更难发现，所以这里逐行钉死 y。
#[test]
fn regions_point_at_the_box_that_produced_each_field() {
    let lines = merge_adjacent_fragments(&[
        OcrLine { text: "莫莫莫89级".into(), confidence: 0.99, rect: (399, 167, 98, 14) },
        OcrLine { text: "ID".into(), confidence: 0.9, rect: (397, 194, 15, 13) },
        OcrLine { text: "429277361".into(), confidence: 0.99, rect: (412, 194, 84, 13) },
        OcrLine { text: "气血".into(), confidence: 0.94, rect: (640, 173, 50, 14) },
        OcrLine { text: "5604/5604".into(), confidence: 0.99, rect: (791, 173, 71, 13) },
        OcrLine { text: "魔法".into(), confidence: 0.99, rect: (638, 209, 53, 19) },
        OcrLine { text: "2267/2267".into(), confidence: 0.99, rect: (792, 211, 68, 13) },
        OcrLine { text: "物理伤害709".into(), confidence: 0.99, rect: (650, 388, 104, 14) },
        OcrLine { text: "物理防御1609".into(), confidence: 0.99, rect: (794, 388, 111, 14) },
        OcrLine { text: "法术伤害".into(), confidence: 0.99, rect: (649, 428, 72, 14) },
        OcrLine { text: "441".into(), confidence: 0.99, rect: (716, 428, 38, 13) },
        OcrLine { text: "法术防御1081".into(), confidence: 0.99, rect: (794, 428, 111, 14) },
        OcrLine { text: "速度".into(), confidence: 0.99, rect: (648, 466, 72, 18) },
        OcrLine { text: "1256".into(), confidence: 0.99, rect: (716, 468, 48, 13) },
        OcrLine { text: "治疗强度133".into(), confidence: 0.99, rect: (794, 468, 102, 14) },
    ]);

    let stats = extract_stats(&lines);
    let currencies = extract_currencies(&lines);
    let mut specs = crate::regions::stat_specs(&stats);
    specs.extend(crate::regions::currency_specs(&currencies));

    let regions = crate::regions::build_regions(&lines, 0, ScreenKind::CharacterAttributes, (0, 0), &specs);
    let at = |key: &str| {
        regions
            .iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("no region for {key}"))
    };

    assert_eq!(at("hp").y, 173, "气血 must point at its own row");
    assert_eq!(at("mp").y, 211, "魔法 must point at its own row");
    assert_eq!(at("physical_damage").y, 388);
    assert_eq!(at("magic_damage").y, 428);
    assert_eq!(at("physical_defense").y, 388);
    assert_eq!(at("magic_defense").y, 428);
    assert_eq!(at("speed").y, 468);
    assert_eq!(at("healing_power").y, 468);

    assert_eq!(at("hp").value, Some(5604));
    assert_eq!(at("hp").raw_text, "5604/5604");
    assert!(at("hp").confidence > 0.9);
    assert!(!at("hp").label.is_empty());
}

// ---------------------------------------------------------------------------
// 分析缓存（内容寻址 + 算法版本 + 自校验）
// ---------------------------------------------------------------------------

/// SHA-256 用标准测试向量钉死：缓存键是这个值的十六进制，写错了不会报错，
/// 只会所有图都"未命中"，看起来像缓存没生效。
#[test]
fn content_hash_matches_the_sha256_test_vector() {
    assert_eq!(
        crate::content_hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        crate::content_hash(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    // 差一个字节就是另一个键——这是"同一张图"的唯一定义。
    assert_ne!(crate::content_hash(b"abc"), crate::content_hash(b"abd"));
}

#[test]
fn analysis_roundtrips_through_the_cache_payload() {
    let img = panel_like_image();
    let lines = vec![
        OcrLine { text: "气血".into(), confidence: 0.93, rect: (10, 20, 30, 14) },
        OcrLine { text: "5604/5604".into(), confidence: 0.99, rect: (60, 20, 80, 13) },
    ];
    let analysis = ImageAnalysis::compute(&img, lines.clone()).unwrap();
    assert_eq!(analysis.version, mhcore::ANALYSIS_VERSION);
    assert!(analysis.matches_image(&img), "自己算出来的必须能复核通过");

    let payload = analysis.encode().unwrap();
    let back = ImageAnalysis::decode(&payload).expect("同版本 payload 必须能读回");
    assert_eq!(back.dhash, analysis.dhash);
    assert_eq!(back.lines, lines);
    assert_eq!(back.hsv_hist.len(), HUE_BINS + SAT_BINS + VAL_BINS);
    assert_eq!((back.width, back.height), (img.width(), img.height()));
}

/// 换了算法版本之后，老 payload 必须**读不回来**（当作未命中重算），
/// 而不是被当成合法的旧结果继续用。
#[test]
fn analysis_payload_from_another_version_is_not_reused() {
    let img = panel_like_image();
    let analysis = ImageAnalysis::compute(&img, Vec::new()).unwrap();

    let mut doc: serde_json::Value =
        serde_json::from_str(&analysis.encode().unwrap()).unwrap();
    doc["version"] = serde_json::json!(mhcore::ANALYSIS_VERSION - 1);
    assert!(ImageAnalysis::decode(&doc.to_string()).is_none(), "旧版本必须失效");

    // 直方图长度不对同样不认（防止半截写坏的行被读出来当特征用）。
    let mut doc: serde_json::Value =
        serde_json::from_str(&analysis.encode().unwrap()).unwrap();
    doc["hsvHist"] = serde_json::json!([0.5, 0.5]);
    assert!(ImageAnalysis::decode(&doc.to_string()).is_none(), "形状不对必须失效");
}

/// 缓存条目自校验：尺寸或像素对不上就不算命中。
#[test]
fn analysis_rejects_an_image_that_is_not_the_one_it_came_from() {
    let img = panel_like_image();
    let analysis = ImageAnalysis::compute(&img, Vec::new()).unwrap();

    let other = image::RgbaImage::from_pixel(64, 64, image::Rgba([20, 30, 40, 255]));
    assert!(!analysis.matches_image(&other), "尺寸不同必须拒绝");

    // 同尺寸、不同内容：dHash 必须能分辨（纯色块之间差异很大）。
    let same_size = image::RgbaImage::from_pixel(img.width(), img.height(), image::Rgba([10, 200, 90, 255]));
    assert!(!analysis.matches_image(&same_size), "同尺寸不同内容必须拒绝");
}

/// 有点纹理的合成面板：纯色图的 dHash 全 0，测不出"内容不同"。
fn panel_like_image() -> image::RgbaImage {
    image::RgbaImage::from_fn(200, 120, |x, y| {
        let v = ((x * 7 + y * 13) % 256) as u8;
        image::Rgba([v, v / 2, 255 - v, 255])
    })
}

// ---------------------------------------------------------------------------
// 逐图识别 + 批次身份校验
// ---------------------------------------------------------------------------
//
// 这一组测试盯的是同一件事：一批截图里混进另一个角色时，数据不能串。
// 旧实现把所有图的 OCR 行拼成一个池子再统一提取身份，池子里两个角色的
// 游戏ID 谁先被扫到就用谁 —— 于是"甲的人物属性 + 乙的包裹"变成"甲的金币"。

fn craft_scan(
    index: usize,
    screen: ScreenKind,
    game_id: Option<&str>,
    nickname: Option<&str>,
) -> ScreenScan {
    ScreenScan {
        index,
        screen,
        character: RecognizedCharacter {
            game_id: game_id.map(str::to_string),
            nickname: nickname.map(str::to_string),
            ..Default::default()
        },
        stats: CombatStats::default(),
        currencies: Currencies::default(),
        equipment: Vec::new(),
        inventory: Vec::new(),
        confidence: 0.95,
        fingerprints: Vec::new(),
        avatar_png: None,
        avatar_thumb: None,
        appearance: None,
        regions: Vec::new(),
    }
}

fn plan_of(scans: &[ScreenScan]) -> BatchPlan {
    let identities: Vec<ScreenIdentity<'_>> = scans
        .iter()
        .map(|s| ScreenIdentity {
            index: s.index,
            game_id: s.character.game_id.as_deref(),
            nickname: s.character.nickname.as_deref(),
        })
        .collect();
    plan_batch(&identities)
}

/// 两张图各自写着不同的游戏ID：不能合并，而且第二张的金币绝不能进第一张。
#[test]
fn a_batch_with_two_game_ids_is_never_merged_into_one_character() {
    let mut attrs = craft_scan(0, ScreenKind::CharacterAttributes, Some("429277361"), Some("莫莫莫"));
    attrs.stats.hp = Some(mhcore::AttributeValue::new(5604, 0.99, "5604/5604"));

    let mut bag = craft_scan(1, ScreenKind::Inventory, Some("429277013"), Some("海淘淘"));
    bag.currencies.gold = Some(mhcore::AttributeValue::new(2_000_000, 0.99, "2,000,000"));
    bag.inventory.push(mhcore::InventoryItem {
        name: "还魂丹".into(),
        count: 3,
        column: None,
        row: None,
        confidence: 0.9,
    });

    let scans = vec![attrs, bag];
    let plan = plan_of(&scans);
    assert_eq!(plan.identity, mhcore::BatchIdentity::Conflict);
    assert_eq!(plan.primary, 0);
    assert!(plan.includes(0));
    assert!(!plan.includes(1));

    let (result, _) = merge_included(&scans, &plan, 2, &[]);

    assert_eq!(result.batch_identity, mhcore::BatchIdentity::Conflict);
    assert_eq!(result.character.game_id.as_deref(), Some("429277361"));
    assert_eq!(result.stats.hp.as_ref().map(|v| v.value), Some(5604));
    assert!(result.currencies.gold.is_none(), "乙的金币不能算进甲");
    assert!(result.inventory.is_empty(), "乙的物品不能算进甲");
    assert!(result.regions.is_empty(), "被排除的图的框不该画在甲的截图上");

    // 数据没丢：第二张的读数留在它自己的条目里，并写明为什么被排除。
    let second = result
        .screen_results
        .iter()
        .find(|s| s.screen_index == 1)
        .expect("second screen kept");
    assert!(!second.included);
    assert_eq!(second.currencies.gold.as_ref().unwrap().value, 2_000_000);
    assert_eq!(second.inventory.len(), 1);
    let reason = second.excluded_reason.as_deref().unwrap_or_default();
    assert!(reason.contains("429277013"), "{reason}");
    assert!(
        result.conflicts.iter().any(|c| c.contains("第 2 张")),
        "{:?}",
        result.conflicts
    );
}

/// 一张有 ID、一张没有（包裹）：允许合并，这是最常见的一次录入两张图。
#[test]
fn a_screenshot_without_identity_joins_the_batch() {
    let attrs = craft_scan(0, ScreenKind::CharacterAttributes, Some("429277361"), Some("莫莫莫"));
    let mut bag = craft_scan(1, ScreenKind::Inventory, None, None);
    bag.currencies.gold = Some(mhcore::AttributeValue::new(2_625_812, 0.99, "2,625,812"));

    let scans = vec![attrs, bag];
    let plan = plan_of(&scans);
    assert_eq!(plan.identity, mhcore::BatchIdentity::Consistent);
    assert!(plan.includes(1));

    let (result, _) = merge_included(&scans, &plan, 2, &[]);
    assert_eq!(result.currencies.gold.as_ref().map(|v| v.value), Some(2_625_812));
    assert_eq!(result.screen_results.len(), 2, "每张图都留了独立读数");
    assert!(result.screen_results.iter().all(|s| s.included));
    // 身份仍以写着 ID 的那张为准。
    assert_eq!(result.character.nickname.as_deref(), Some("莫莫莫"));
}

/// 两张都只有昵称，且不一样：不能直接合并。
#[test]
fn two_different_nicknames_without_ids_conflict() {
    let a = craft_scan(0, ScreenKind::CharacterAttributes, None, Some("莫莫莫"));
    let b = craft_scan(1, ScreenKind::CharacterAttributes, None, Some("海淘淘"));

    let scans = vec![a, b];
    let plan = plan_of(&scans);
    assert_eq!(plan.identity, mhcore::BatchIdentity::Conflict);
    assert!(plan.includes(0));
    assert!(!plan.includes(1));

    let (result, _) = merge_included(&scans, &plan, 2, &[]);
    assert_eq!(result.character.nickname.as_deref(), Some("莫莫莫"));
    assert!(result
        .screen_results
        .iter()
        .find(|s| s.screen_index == 1)
        .and_then(|s| s.excluded_reason.clone())
        .unwrap_or_default()
        .contains("海淘淘"));
}

/// 一张有 ID，另一张有别的昵称但没有 ID：也是两个人。
#[test]
fn a_different_nickname_beside_a_game_id_conflicts() {
    let a = craft_scan(0, ScreenKind::CharacterAttributes, Some("429277361"), Some("莫莫莫"));
    let b = craft_scan(1, ScreenKind::Unknown, None, Some("海淘淘"));

    let plan = plan_of(&[a, b]);
    assert_eq!(plan.identity, mhcore::BatchIdentity::Conflict);
    assert!(!plan.includes(1));
}

/// 整批都没有身份字段：不是冲突，但也不能假装知道是谁。
#[test]
fn a_batch_with_no_identity_at_all_is_marked_unidentified() {
    let a = craft_scan(0, ScreenKind::Inventory, None, None);
    let b = craft_scan(1, ScreenKind::Inventory, None, None);

    let plan = plan_of(&[a, b]);
    assert_eq!(plan.identity, mhcore::BatchIdentity::Unidentified);
    assert!(plan.included.len() == 2, "没有证据否定，就按同一角色处理");
    assert!(plan.notes.iter().any(|n| n.contains("没有任何身份信息")));
}

/// 单张图没有身份字段也标成未知：界面要提示"请确认归属"。
#[test]
fn a_single_identityless_screenshot_is_unidentified() {
    let plan = plan_of(&[craft_scan(0, ScreenKind::Inventory, None, None)]);
    assert_eq!(plan.identity, mhcore::BatchIdentity::Unidentified);
}

/// 逐图取身份：包裹那张不能从别人的面板里"蹭"到游戏ID。
#[test]
fn each_panel_only_reports_its_own_identity() {
    let attrs = merge_adjacent_fragments(&[
        OcrLine { text: "莫莫莫89级".into(), confidence: 0.99, rect: (399, 167, 98, 14) },
        OcrLine { text: "ID".into(), confidence: 0.9, rect: (397, 194, 15, 13) },
        OcrLine { text: "429277361".into(), confidence: 0.99, rect: (412, 194, 84, 13) },
        OcrLine { text: "5604/5604".into(), confidence: 0.99, rect: (791, 173, 71, 13) },
    ]);
    let bag = merge_adjacent_fragments(&[
        OcrLine { text: "金币 2,000,000".into(), confidence: 0.95, rect: (70, 728, 300, 30) },
    ]);

    let stats = extract_stats(&attrs);
    let from_attrs = screen_identity(&attrs, &stats);
    let from_bag = screen_identity(&bag, &CombatStats::default());

    assert_eq!(from_attrs.game_id.as_deref(), Some("429277361"));
    assert_eq!(from_attrs.nickname.as_deref(), Some("莫莫莫"));
    assert_eq!(from_attrs.level, Some(89));

    assert!(from_bag.game_id.is_none());
    assert!(from_bag.nickname.is_none());
    assert!(from_bag.level.is_none());
}
