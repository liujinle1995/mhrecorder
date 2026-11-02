//! 从 OCR 行里抠字段。
//!
//! 昵称、游戏ID、等级、门派、属性、货币、背包、装备。全是纯文本启发式，
//! 改这里的唯一标准是真图回归测试还是绿的。

use crate::prelude::*;
// ---------------------------------------------------------------------------
// Field extraction
// ---------------------------------------------------------------------------

/// Labels we can map onto `CombatStats` fields.
pub(crate) fn stat_label_map(label: &str) -> Option<&'static str> {
    let l = label.replace([' ', '：', ':'], "");
    // Order matters: check the more specific labels first so that e.g.
    // 物理防御 does not get mistaken for 物理伤害.
    let pairs: &[(&str, &str)] = &[
        ("物理防御", "physical_defense"),
        ("法术防御", "magic_defense"),
        ("物理伤害", "physical_damage"),
        ("法术伤害", "magic_damage"),
        ("治疗强度", "healing_power"),
        ("盈福经验", "experience_bonus"),
        ("气血", "hp"),
        ("魔法", "mp"),
        ("速度", "speed"),
        // 愤怒 / 活力 also appear on the real 人物属性 panel.
        ("愤怒", "rage"),
        ("活力", "vigor"),
        ("经验", "experience"),
    ];
    if let Some(hit) = pairs.iter().find(|(k, _)| l.contains(*k)) {
        return Some(hit.1);
    }

    // PP-OCR misreads some widely-spaced glyphs on the real panel (血 -> 面).
    // Fall back on the FIRST glyph of the two-character stat labels, which is
    // far more reliable: 气 -> hp, 魔 -> mp, 速 -> speed.
    let first = l.chars().next()?;
    match first {
        '气' => Some("hp"),
        '魔' => Some("mp"),
        '速' => Some("speed"),
        '愤' => Some("rage"),
        '活' => Some("vigor"),
        _ => None,
    }
}

/// On the real game panels each label is rendered with wide letter spacing,
/// so PP-OCR's detector emits one box per GLYPH: 气 and 血 come back as two
/// separate lines. Merge horizontally adjacent single-character lines that
/// sit on the same row into one logical token.
///
/// This runs before every extractor so downstream code only ever sees
/// sensible words ("气血") instead of fragments ("气", "血").
pub(crate) fn merge_adjacent_fragments(lines: &[OcrLine]) -> Vec<OcrLine> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<OcrLine> = Vec::with_capacity(lines.len());
    for line in lines {
        let text = line.text.trim();
        // Only single-CJK-character boxes are candidates for merging.
        let is_single_cjk = text.chars().count() == 1
            && text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        if !is_single_cjk {
            out.push(line.clone());
            continue;
        }

        // Look for an existing fragment on the same visual row, close enough
        // horizontally that the pair reads as one word.
        let mut merged = false;
        for prev in out.iter_mut().rev() {
            // Only merge with a previous row: fragments are emitted left-to-right
            // by the OCR reading-order pass.
            let dy = (prev.center_y() - line.center_y()).abs();
            let max_dy = (prev.rect.3.max(line.rect.3) as f64) * 0.45;
            if dy > max_dy.max(4.0) {
                continue;
            }
            let gap = line.rect.0 - (prev.rect.0 + prev.rect.2);
            // The game draws glyphs ~a character-width apart; allow up to 2.2x
            // the glyph height so "速" + "度" merge but columns do not.
            if gap < 0 || (gap as f64) > (line.rect.3 as f64) * 3.0 + 6.0 {
                continue;
            }
            // Don't glue a fragment onto a long value/label that is already
            // complete (e.g. "5604/5604" + "气" must not merge).
            if prev.text.chars().count() > 2 {
                continue;
            }
            prev.text.push_str(text);
            let x1 = prev.rect.0;
            let y1 = prev.rect.1;
            let right = (prev.rect.0 + prev.rect.2).max(line.rect.0 + line.rect.2);
            let bottom = (prev.rect.1 + prev.rect.3).max(line.rect.1 + line.rect.3);
            prev.rect = (x1, y1, right - x1, bottom - y1);
            prev.confidence = prev.confidence.min(line.confidence);
            merged = true;
            break;
        }
        if !merged {
            out.push(line.clone());
        }
    }
    out
}


/// Parse a number that may contain thousands separators (`,`/`，`) and
/// OCR noise such as stray spaces.
///
/// Only the FIRST numeric token is parsed: OCR often returns "7,721 / 7,721"
/// or "758 1,714", and gluing every digit together would produce nonsense.
///
/// Thousands separators are ambiguous in OCR output: the recogniser
/// frequently renders `,` as `.` (e.g. "2.215/2.215"). Since every value in
/// this game's UI is an integer, `.` and `，` are treated as separators too.
pub(crate) fn parse_number(raw: &str) -> Option<i64> {
    // `01:56` is the HUD clock. Its leading digits parse as a plausible small
    // integer, and on a full-window capture that turned into 金币 = 1.
    let trimmed = raw.trim();
    if trimmed.contains(':')
        && trimmed.chars().all(|c| c.is_ascii_digit() || c == ':')
    {
        return None;
    }

    let mut token = String::new();
    let mut started = false;
    for c in raw.chars() {
        if c.is_ascii_digit() {
            token.push(c);
            started = true;
        } else if started && matches!(c, ',' | '，' | '.' | '·') {
            // A separator may only appear between digits; "7,721" is fine but
            // "2.215/2.215" must stop at the slash.
            token.push(',');
        } else if started {
            break;
        }
    }
    let cleaned: String = token.chars().filter(|c| c.is_ascii_digit()).collect();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<i64>().ok()
}

/// Find the numeric value that belongs to `label`: either the remainder of the
/// same line, or the nearest value to the right on the same row.
///
/// Selection is ranked by VERTICAL alignment first, then horizontal distance.
/// Ranking by horizontal distance alone picks the wrong value when several
/// stat rows share the same value column (e.g. 气血/魔法 on adjacent rows).
pub(crate) fn value_for_label(lines: &[OcrLine], label_line: &OcrLine) -> Option<(i64, f64, String)> {
    let after_label = strip_label(&label_line.text, &label_line.text);
    if let Some(v) = parse_number(after_label) {
        return Some((v, label_line.confidence, after_label.to_string()));
    }

    // Values must sit on the label's row, so allow roughly one row height.
    let row_tolerance = ((label_line.rect.3 as f64) * 1.5).max(20.0);
    let mut best: Option<(f64, f64, i64, f64, String)> = None;
    for other in lines {
        if other.rect.0 <= label_line.rect.0 {
            continue; // values are laid out to the right of their label
        }
        let dy = (other.center_y() - label_line.center_y()).abs();
        if dy > row_tolerance {
            continue;
        }
        let Some(v) = parse_number(&other.text) else { continue };
        let dx = other.center_x() - label_line.center_x();
        let better = match best.as_ref() {
            None => true,
            // Vertical alignment wins; horizontal distance only breaks ties.
            Some(b) => dy < b.0 - 2.0 || ((dy - b.0).abs() <= 2.0 && dx < b.1),
        };
        if better {
            best = Some((dy, dx, v, other.confidence, other.text.clone()));
        }
    }
    best.map(|(_, _, v, c, raw)| (v, c, raw))
}

pub(crate) fn strip_label<'a>(text: &'a str, _label: &str) -> &'a str {
    // OCR sometimes returns "气血 7,721 / 7,721" as one line.
    text.split_once(' ')
        .map(|(_, rest)| rest)
        .unwrap_or("")
}

/// Extract the game id.
///
/// Crop the character portrait out of a 人物属性 screenshot and return it as a
/// base64 PNG.
///
/// ## How the avatar is located
///
/// Hardcoding pixel coordinates would break on every resolution/DPI change, so
/// the crop is derived from the **game-ID text**, which we already recognise
/// reliably. On the real panel the layout is:
///
/// ```text
///   +--------+   ID 429277361
///   | avatar |   Lv.xx  门派...
///   +--------+
/// ```
///
/// Rules that hold across captures:
/// * the frame is **square**;
/// * its **right edge** sits a small gap left of the `ID` text;
/// * its **vertical centre** is near the `ID`/level header rows.
///
/// We therefore seed a square from the ID box, then verify it really looks like
/// a portrait (high colour variance inside, low variance on the border ring).
/// If the check fails we return `None` rather than shipping a wrong crop.
///
/// Returns `(x, y, size)` in image pixels, or `None` when no plausible portrait
/// square can be derived from this panel.
pub(crate) fn avatar_geometry(lines: &[OcrLine], img_w: i32, img_h: i32) -> Option<(i32, i32, i32)> {
    let (w, h) = (img_w, img_h);

    // Anchor: the "ID" token (or the line that carries the numeric game id).
    let anchor = lines
        .iter()
        .find(|l| {
            let t = l.text.trim();
            t.eq_ignore_ascii_case("id") || t == "ID" || t == "编号"
        })
        .or_else(|| {
            lines.iter().find(|l| {
                l.text.chars().filter(|c| c.is_ascii_digit()).count() >= 6
                    && !l.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            })
        })?;

    // A square whose right edge stops just left of the anchor, vertically
    // centred on the header block (ID row plus the level row beneath it).
    //
    // Geometry measured off the real 1265x711 capture:
    //   avatar frame   : x 292..378, y 158..248  (86x90, effectively square)
    //   "ID" text box  : x 397..412, y 194..207
    // so the frame's right edge sits ~19px left of the ID box and its top is
    // ~36px above the ID row centre.
    // Header glyph height. This must stay in a plausible range: a tall
    // vertical label elsewhere on the panel (e.g. the 战前检查 side tab is a
    // single 118px-tall box) would otherwise blow the avatar size up to
    // hundreds of pixels. Only consider boxes of roughly normal header
    // line-height and pick the median rather than the maximum.
    let mut heights: Vec<i32> = lines
        .iter()
        .filter(|l| {
            l.rect.1 < anchor.rect.1 + anchor.rect.3 * 3
                && l.rect.3 > 6
                && l.rect.3 < anchor.rect.3 * 2
        })
        .map(|l| l.rect.3)
        .collect();
    heights.sort_unstable();
    let header_h = if heights.is_empty() {
        anchor.rect.3 as f32
    } else {
        heights[heights.len() / 2] as f32
    };

    // Scale everything from the header text height so other resolutions work.
    //
    // Multipliers measured off the real 1265x711 capture (header_h=13):
    //   frame          : x 289..357, y 150..210  (66x60, drawn square + frame)
    //   "ID" text box  : x 397..412, y 194..207
    //   -> size = 66   = 5.08 * header_h
    //   -> gap  = 40   = 3.08 * header_h  (frame right edge -> ID box left)
    //   -> top  = ID top - 44 = ID top - 0.667 * size
    // An earlier 6.6x/0.42 estimate framed the side tab and the badge instead
    // of the portrait, which produced a visibly off-centre crop.
    let gap = header_h * 3.08;
    let size = header_h * 5.08;
    let right = anchor.rect.0 as f32 - gap;
    let left = right - size;
    let top = anchor.rect.1 as f32 - size * 0.667;

    if left < 0.0 || top < 0.0 || right > w as f32 || top + size > h as f32 {
        tracing::debug!(
            "avatar: out of bounds (header_h={header_h:.1} size={size:.1} left={left:.1} top={top:.1} right={right:.1} img={w}x{h})"
        );
        return None;
    }

    let s = size.round() as i32;
    if s < 32 {
        return None;
    }
    Some((left.round() as i32, top.round() as i32, s))
}

pub(crate) fn crop_avatar(img: &image::RgbaImage, lines: &[OcrLine]) -> Option<image::RgbaImage> {
    let (x, y, s) = avatar_geometry(lines, img.width() as i32, img.height() as i32)?;

    // The located square includes the frame plus a little neighbouring chrome.
    // Trim a small margin so the exported image is the portrait art itself
    // rather than art-with-background; the margin is proportional so it also
    // works at other resolutions.
    let inset = (s / 14).max(1);
    let ix = x + inset;
    let iy = y + inset;
    let is = (s - inset * 2).max(1);

    // Trust check: a portrait has strong colour variation inside; the area
    // immediately outside its frame does not.
    if !looks_like_portrait(img, ix, iy, is) {
        return None;
    }

    Some(image::imageops::crop_imm(img, ix as u32, iy as u32, is as u32, is as u32).to_image())
}

/// Crop the framed portrait out of a 人物属性 panel and encode it as a PNG
/// data URL for the UI.
pub(crate) fn crop_avatar_png(img: &image::RgbaImage, lines: &[OcrLine]) -> Option<String> {
    let cropped = crop_avatar(img, lines)?;
    // Encode as PNG and base64 so the UI can turn it straight into a data URL.
    let mut png_bytes = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        if cropped.write_to(&mut cursor, image::ImageFormat::Png).is_err() {
            return None;
        }
    }
    use base64::Engine as _;
    Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
}

/// 16x16 grayscale of the cropped portrait, used as a face fingerprint.
///
/// Colour is dropped on purpose: the game re-tints portraits with lighting and
/// equipment glow, while the *shape* of the face stays put. 256 bytes is small
/// enough to keep in the characters table and compare on every recognition.
pub(crate) fn crop_avatar_thumb(img: &image::RgbaImage, lines: &[OcrLine]) -> Option<String> {
    let cropped = crop_avatar(img, lines)?;
    let small = image::imageops::resize(
        &cropped,
        16,
        16,
        image::imageops::FilterType::Triangle,
    );
    let mut out = Vec::with_capacity(16 * 16);
    for p in small.pixels() {
        // Rec. 601 luma, same weighting the panel thumbnails use.
        let v = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
        out.push(v.clamp(0.0, 255.0) as u8);
    }
    use base64::Engine as _;
    Some(base64::engine::general_purpose::STANDARD.encode(&out))
}

/// Heuristic confidence check for a candidate avatar crop.
///
/// A real portrait is colourful and detailed; the panel background around it is
/// flat. Requiring both keeps us from returning a random slice of UI chrome.
pub(crate) fn looks_like_portrait(img: &image::RgbaImage, x: i32, y: i32, s: i32) -> bool {
    let (w, h) = (img.width() as i32, img.height() as i32);

    // Interior variance (sample a 16x16 grid to stay cheap).
    let step = (s / 16).max(1);
    let mut samples = Vec::new();
    let mut yy = y + s / 8;
    while yy < y + s - s / 8 && yy < h {
        let mut xx = x + s / 8;
        while xx < x + s - s / 8 && xx < w {
            let p = img.get_pixel(xx as u32, yy as u32);
            samples.push((p[0] as f32, p[1] as f32, p[2] as f32));
            xx += step;
        }
        yy += step;
    }
    if samples.len() < 16 {
        return false;
    }
    let mean = (
        samples.iter().map(|p| p.0).sum::<f32>() / samples.len() as f32,
        samples.iter().map(|p| p.1).sum::<f32>() / samples.len() as f32,
        samples.iter().map(|p| p.2).sum::<f32>() / samples.len() as f32,
    );
    let var = samples
        .iter()
        .map(|p| {
            let dr = p.0 - mean.0;
            let dg = p.1 - mean.1;
            let db = p.2 - mean.2;
            dr * dr + dg * dg + db * db
        })
        .sum::<f32>()
        / samples.len() as f32;
    // Detached portraits on coloured backgrounds easily exceed this; a flat
    // panel area does not.
    var > 700.0
}

/// Two layouts occur in practice:
///  * the synthetic/older layout puts it on one line: "游戏ID 429277013";
///  * the real 人物属性 panel renders it as two adjacent boxes: "ID" then
///    "429277361" immediately to its right (a small gap).
/// Both are handled here; the second path is the one that matters for real
/// captures.
pub(crate) fn extract_game_id(lines: &[OcrLine]) -> Option<(String, f64)> {
    // ---- path 1: marker and digits in the same line --------------------
    const MARKERS: [&str; 4] = ["游戏ID", "游戏id", "游戏ID", "ID"];
    for line in lines {
        let normalized = line.text.replace(' ', "");
        if let Some(pos) = MARKERS.iter().filter_map(|m| normalized.find(*m)).min() {
            let rest = &normalized[pos..];
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
                .filter(|c| c.is_ascii_digit())
                .collect();
            if digits.len() >= 5 {
                return Some((digits, line.confidence));
            }
        }
    }

    // ---- path 2: a standalone "ID" box, digits in the box to its right --
    for line in lines {
        let t = line.text.trim();
        if !is_id_marker(t) {
            continue;
        }
        let mut best: Option<(f64, String, f64)> = None;
        for other in lines {
            if other == line {
                continue;
            }
            // The id digits sit on the same row, just to the right.
            let dy = (other.center_y() - line.center_y()).abs();
            if dy > (line.rect.3 as f64).max(12.0) {
                continue;
            }
            // Compare CENTRES, not edges: the detector's unclip padding makes
            // the "ID" box and the digits box overlap by a few pixels on the
            // real panel, and an edge-based gap test rejected exactly that pair.
            if other.center_x() <= line.center_x() {
                continue;
            }
            let gap = other.rect.0 - (line.rect.0 + line.rect.2);
            if (gap as f64) < -(line.rect.3 as f64) || (gap as f64) > (line.rect.3 as f64) * 3.0 {
                continue;
            }
            let digits: String =
                other.text.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 6 {
                if best.as_ref().map(|b| gap as f64 > b.0).unwrap_or(true) {
                    best = Some((gap as f64, digits, other.confidence));
                }
            }
        }
        if let Some((_, digits, conf)) = best {
            return Some((digits, conf.min(line.confidence)));
        }
    }
    None
}

/// Is this token the "ID" label of the character header?
///
/// PP-OCR reads the real panel's `ID` as `LD` often enough that matching only
/// the literal string loses the 游戏ID — and with it the identity of every
/// capture that has no other anchor.
pub(crate) fn is_id_marker(t: &str) -> bool {
    let compact: String = t
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    matches!(compact.as_str(), "ID" | "LD" | "1D" | "I0" | "编号" | "ID号")
}

/// Extract level from "Lv.89" or "89级".
pub(crate) fn extract_level(lines: &[OcrLine]) -> Option<(i64, f64)> {
    for line in lines {
        let t = line.text.replace(' ', "");
        for prefix in ["Lv.", "LV.", "Lv", "lv"] {
            if let Some(idx) = t.find(prefix) {
                let digits: String =
                    t[idx + prefix.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = digits.parse::<i64>() {
                    return Some((v, line.confidence));
                }
            }
        }
        if let Some(idx) = t.find('级') {
            let start = t[..idx].char_indices().rev()
                .take_while(|(_, c)| c.is_ascii_digit())
                .last()
                .map(|(i, _)| i);
            if let Some(s) = start {
                if let Ok(v) = t[s..idx].parse::<i64>() {
                    return Some((v, line.confidence));
                }
            }
        }
    }
    None
}

/// Extract the school after "门派：".
pub(crate) fn extract_school(lines: &[OcrLine]) -> Option<String> {
    for line in lines {
        if let Some(idx) = line.text.find('门') {
            // Slice by char boundary, never by raw byte offset: '：' is 3 bytes.
            let rest = &line.text[idx..];
            if let Some((_, after)) = rest.split_once(['：', ':']) {
                let v = after.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// The nickname is usually the largest-text line in the header. We approximate
/// "largest" by picking the earliest short line that is not a label or number.
pub(crate) fn extract_nickname(lines: &[OcrLine], stats: &CombatStats, game_id: Option<&str>) -> Option<String> {
    // ---- Preferred: the real panel puts the name immediately left of the
    // level marker in one box, e.g. "莫莫莫89级" / "莫莫莫 89级". -------------
    for line in lines {
        let t = line.text.trim();
        for marker in ["级", "Lv.", "Lv", "lv"] {
            if let Some(idx) = t.find(marker) {
                // Walk back over the level digits that precede the marker.
                let before = &t[..idx];
                let digits_start = before
                    .char_indices()
                    .rev()
                    .take_while(|(_, c)| c.is_ascii_digit())
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(before.len());
                let name = before[..digits_start].trim().to_string();
                let cjk = name.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
                if cjk >= 2 && name.chars().count() <= 12 {
                    return Some(name);
                }
            }
        }
    }

    // ---- Fallback: a short standalone CJK line near the top of the panel.
    // Restricted to the header band so world-chat lines further down are
    // never mistaken for the character name.
    let top = lines
        .iter()
        .map(|l| l.rect.1)
        .min()
        .unwrap_or(0);
    let header_limit = top + 260; // header region of a ~700px-tall capture

    let label_words = [
        "气血", "魔法", "物理", "法术", "速度", "治疗", "门派", "游戏", "Lv", "等级",
        "战斗", "防御", "命中", "封印", "抵抗", "金币", "银币", "容量", "包裹", "物品",
        "任务", "属性", "世界", "系统", "帮派", "公告", "经验", "评分", "衣橱",
    ];
    let mut best: Option<(i32, String)> = None;
    for line in lines {
        let t = line.text.trim();
        if t.is_empty() || t.chars().count() > 12 {
            continue;
        }
        if t.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        if label_words.iter().any(|w| t.contains(w)) {
            continue;
        }
        if game_id.map(|g| t.contains(g)).unwrap_or(false) {
            continue;
        }
        // Header band only.
        if line.rect.1 > header_limit || line.rect.1 > header_limit {
            continue;
        }
        let cjk = t.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
        if cjk >= 2 && cjk <= 8 {
            // Prefer a left-aligned line: the name sits at the panel's left edge.
            match &best {
                Some((x, _)) if *x <= line.rect.0 => {}
                _ => best = Some((line.rect.0, t.to_string())),
            }
        }
    }
    let _ = stats;
    best.map(|(_, s)| s)
}

/// Labels, longest first so that a label which is a prefix of another
/// (物伤 vs 物理伤害) never wins by accident.
pub(crate) const STAT_LABELS: &[&str] = &[
    "物理防御", "法术防御", "物理伤害", "法术伤害", "治疗强度", "盈福经验", "抵抗封印",
    "封印命中", "气血", "魔法", "愤怒", "活力", "速度", "经验",
];

/// Split "物理伤害709" into (label, remainder). Returns the remainder only,
/// which is what carries the numeric value.
///
/// This is deliberately not a whitespace split: the real panels emit
/// label+value as ONE OCR box with no separator at all.
pub(crate) fn strip_stat_label(text: &str) -> Option<(&'static str, String)> {
    let compact = text.replace([' ', '：', ':'], "");
    for label in STAT_LABELS {
        if let Some(idx) = compact.find(label) {
            let rest = compact[idx + label.len()..].to_string();
            return Some((label, rest));
        }
    }
    None
}

pub(crate) fn extract_stats(lines: &[OcrLine]) -> CombatStats {
    let mut stats = CombatStats::default();
    for line in lines {
        let Some(field) = stat_label_map(&line.text) else { continue };

        // Case A: the label and its value share one OCR box ("物理伤害709").
        let inline = strip_stat_label(&line.text)
            .and_then(|(_, rest)| parse_number(&rest).map(|v| (v, rest)));

        let got = if let Some((v, rest)) = inline {
            Some((v, line.confidence, rest.trim().to_string()))
        } else {
            // Case B: the value is a separate box on the same row to the right.
            value_for_label(lines, line)
        };
        let Some((value, confidence, raw_text)) = got else { continue };
        let av = AttributeValue::new(value, confidence, raw_text);
        let slot: &mut Option<AttributeValue> = match field {
            "hp" => &mut stats.hp,
            "mp" => &mut stats.mp,
            "physical_damage" => &mut stats.physical_damage,
            "magic_damage" => &mut stats.magic_damage,
            "physical_defense" => &mut stats.physical_defense,
            "magic_defense" => &mut stats.magic_defense,
            "speed" => &mut stats.speed,
            "healing_power" => &mut stats.healing_power,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(av);
        }
    }
    stats
}

pub(crate) fn extract_currencies(lines: &[OcrLine]) -> Currencies {
    let mut currencies = Currencies::default();

    // ---- path 1: explicit text labels (synthetic panels / older UI) -------
    for line in lines {
        let is_gold = line.text.contains("金币");
        let is_silver = line.text.contains("银币");
        if !is_gold && !is_silver {
            continue;
        }
        let same_line = line.text.replace("金币", "").replace("银币", "");
        let got = parse_number(&same_line)
            .map(|v| (v, line.confidence, same_line.trim().to_string()))
            .or_else(|| value_for_label(lines, line));
        let Some((value, confidence, raw)) = got else { continue };
        let av = AttributeValue::new(value, confidence, raw);
        if is_gold && currencies.gold.is_none() {
            currencies.gold = Some(av);
        } else if is_silver && currencies.silver.is_none() {
            currencies.silver = Some(av);
        }
    }
    if currencies.gold.is_some() || currencies.silver.is_some() {
        return currencies;
    }

    // ---- path 2: icon-only currency strip (the real 包裹 panel) ----------
    // The real bag shows two coin icons with bare numbers beside them and no
    // textual label at all:  [gold icon] 2,625,812  [+]
    //                        [silver icon] 9,828,458  [+]
    //
    // This guess must only run on a panel we believe is a bag: running it on a
    // 人物属性 capture picked up the game ID and the 经验 figures as currency.
    if !looks_like_bag(lines) {
        return currencies;
    }

    let mut candidates: Vec<&OcrLine> = lines
        .iter()
        .filter(|l| {
            let digits: String = l.text.chars().filter(|c| c.is_ascii_digit()).collect();
            // The currency figures are plain integers with at least 4 digits
            // and no surrounding CJK text, and must not sit inside the
            // 经验/人物评分 rows.
            digits.len() >= 4
                && !l.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                && !l.text.contains('/')
        })
        .collect();

    // Prefer the lowest band of the screen: the currency row sits under the
    // inventory grid and the 人物评分 line.
    if candidates.len() >= 2 {
        let max_y = candidates.iter().map(|l| l.center_y()).fold(f64::MIN, f64::max);
        let band = 40.0;
        let mut row: Vec<&OcrLine> = candidates
            .iter()
            .copied()
            .filter(|l| (l.center_y() - max_y).abs() <= band)
            .collect();
        row.sort_by(|a, b| a.rect.0.cmp(&b.rect.0));
        if row.len() >= 2 {
            candidates = row;
        }
    }

    candidates.sort_by(|a, b| a.rect.0.cmp(&b.rect.0));
    let parsed: Vec<(i64, f64, String)> = candidates
        .iter()
        .filter_map(|l| parse_number(&l.text).map(|v| (v, l.confidence, l.text.clone())))
        .collect();

    if let Some((v, c, raw)) = parsed.first() {
        currencies.gold = Some(AttributeValue::new(*v, *c, raw.clone()));
    }
    if let Some((v, c, raw)) = parsed.get(1) {
        currencies.silver = Some(AttributeValue::new(*v, *c, raw.clone()));
    }

    currencies
}

/// Read the 人物评分 shown on the 包裹 panel.
///
/// This is the one character-specific number the bag renders, which makes it an
/// exact key for "whose bag is this" even after the item grid has changed.
pub(crate) fn extract_character_score(lines: &[OcrLine]) -> Option<i64> {
    // Anchor on 「评分」 rather than the full 「人物评分」: on a cropped panel
    // PP-OCR reads the label as 入物评分 / 人份评分 often enough that matching
    // the exact phrase loses the only identity key a 包裹 screenshot has.
    let label = "评分";
    for line in lines {
        let compact = line.text.replace(' ', "");
        if let Some(idx) = compact.find(label) {
            let rest = &compact[idx + label.len()..];
            if let Some(v) = parse_number(rest) {
                if v > 0 {
                    return Some(v);
                }
            }
        }
    }

    // Label and value in separate boxes on the same row.
    for line in lines {
        let compact = line.text.replace(' ', "");
        if !compact.contains(label) || parse_number(&compact).is_some() {
            continue;
        }
        let mut best: Option<i64> = None;
        for other in lines {
            if other == line {
                continue;
            }
            let dy = (other.center_y() - line.center_y()).abs();
            if dy > (line.rect.3 as f64).max(14.0) {
                continue;
            }
            if other.center_x() <= line.center_x() {
                continue;
            }
            if let Some(v) = parse_number(&other.text) {
                if v > 0 {
                    best = Some(v);
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

/// Heuristic: is this screenshot the 包裹 / inventory panel?
///
/// Only *titles and tab names of the bag itself* count. An earlier version also
/// matched 任务 / 整理 / 礼包 anywhere in the frame, which on a full window
/// capture matched the quest tracker (捉鬼任务) and made the currency and
/// inventory guesses run on a 人物属性 frame.
pub(crate) fn looks_like_bag(lines: &[OcrLine]) -> bool {
    lines.iter().any(|l| {
        let t = l.text.replace(' ', "");
        matches!(t.as_str(), "包裹" | "背包" | "行囊" | "物品" | "外观衣橱")
            || t.contains("人物评分")
            || t.contains("包裹")
            || t.contains("背包")
            || t.contains("容量")
    })
}

/// Recognise inventory items.
///
/// Inventory cells are laid out in a grid, and OCR returns the item name and
/// its stack count as SEPARATE lines (the count sits just below the name, in
/// the same column). So we pair each CJK name with the numeric line directly
/// beneath it.
pub(crate) fn extract_inventory(lines: &[OcrLine]) -> Vec<InventoryItem> {
    // Attribute-panel labels are also short CJK runs, so they must be
    // excluded explicitly; otherwise a 人物属性 screenshot "invents" items
    // called 气血 / 速度 / 物理伤害.
    let is_ui_label = |t: &str| -> bool {
        stat_label_map(t).is_some()
            || ["战斗属性", "防御属性", "门派", "游戏", "容量", "背包", "金币", "银币", "等级", "Lv"]
                .iter()
                .any(|w| t.contains(w))
    };

    let is_cjk_name = |t: &str| -> bool {
        let chars: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
        chars.len() >= 2
            && chars.len() <= 10
            && chars.iter().all(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
    };
    let is_pure_number = |t: &str| -> Option<i64> {
        let t = t.trim();
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            t.parse::<i64>().ok()
        } else {
            None
        }
    };

    let mut items = Vec::new();
    for name_line in lines {
        let name = name_line.text.trim();
        if !is_cjk_name(name) || is_ui_label(name) {
            continue;
        }

        // Find the count line: usually just below the name in the same
        // column, but tolerate a little vertical slack either way because
        // OCR boxes are not perfectly aligned to grid cells.
        let mut best: Option<(f64, i64, i32, i32)> = None;
        for count_line in lines {
            let Some(count) = is_pure_number(&count_line.text) else { continue };
            let dy = count_line.center_y() - name_line.center_y();
            let max_dy = (name_line.rect.3 as f64 * 3.0).max(40.0);
            if dy.abs() > max_dy {
                continue;
            }
            let dx = (count_line.center_x() - name_line.center_x()).abs();
            let max_dx = (name_line.rect.2 as f64).max(60.0);
            if dx > max_dx {
                continue;
            }
            let score = dx + dy.abs() * 0.3;
            if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
                best = Some((score, count, count_line.rect.0, count_line.rect.1));
            }
        }

        let Some((_score, count, cx, cy)) = best else { continue };

        // Merge the same item appearing in several documented cells.
        if let Some(existing) = items.iter_mut().find(|i: &&mut InventoryItem| i.name == name) {
            existing.count += count;
            continue;
        }
        items.push(InventoryItem {
            name: name.to_string(),
            count,
            column: Some(cx),
            row: Some(cy),
            confidence: name_line.confidence,
        });
    }
    items
}

/// Equipment entries, recognised by the slot vocabulary.
pub(crate) fn extract_equipment(lines: &[OcrLine]) -> Vec<EquipmentItem> {
    const SLOTS: [&str; 8] = ["武器", "头盔", "衣服", "项链", "腰带", "鞋子", "戒指", "饰品"];
    let mut out = Vec::new();
    for line in lines {
        for slot in SLOTS {
            if line.text.contains(slot) {
                let name = line.text.replace(slot, "").trim().to_string();
                if !name.is_empty() {
                    out.push(EquipmentItem {
                        slot: slot.to_string(),
                        name,
                        level: None,
                        confidence: line.confidence,
                    });
                }
            }
        }
    }
    out
}
