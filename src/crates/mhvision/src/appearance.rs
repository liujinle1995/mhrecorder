//! 外观特征：发色与武器。
//!
//! 面板会骗人（换装备、换时装、骑坐骑），发色和武器不会。包裹截图认不出
//! 人时，靠的就是这两项。

use crate::prelude::*;
use crate::classify::locate_panel;
use crate::identity::MatchPolicy;
// ---------------------------------------------------------------------------
// Appearance signature (发色 / 武器)
// ---------------------------------------------------------------------------

/// Horizontal span of the character model column, as fractions of the panel.
///
/// Only x is fixed. The model always stands in the left column; what moves
/// between panel kinds is the vertical position (the 包裹 panel pushes the
/// model down past the equipment slots) *and*, on a desktop window capture,
/// the panel frame itself. y is therefore measured per capture by
/// [`character_band`] instead of being hard-coded here.
///
/// The upper bound matters: on 包裹 the equipment slots sit at x 0.10..0.22 and
/// are full of saturated icon colours, so the window has to start to their
/// right or every 包裹 capture would report "发色 蓝" from a helmet icon.
pub(crate) fn column_zone(kind: ScreenKind) -> (f32, f32) {
    match kind {
        ScreenKind::CharacterAttributes => (0.155, 0.320),
        ScreenKind::Inventory => (0.200, 0.340),
        _ => (0.185, 0.335),
    }
}

/// Where the head sits *inside* the model block, as `(top offset, height)` in
/// fractions of the block.
///
/// These are relative to the block rather than the panel because the block's
/// top is a different thing on each panel: on 人物属性 it is the portrait
/// frame's border, ~18% above the head; on 包裹 it is the character's own head
/// or the mount's antlers, so the window has to start almost immediately.
/// Measured on four captures at three different panel sizes (752x498 share,
/// 790x679 and 807x679 window captures).
pub(crate) fn head_window(kind: ScreenKind) -> (f32, f32) {
    match kind {
        // Height matters as much as top: the robe starts right under the hair,
        // and a window that reaches into it averages gold with red and lands
        // ~60 away from the same head on the 包裹 panel.
        ScreenKind::CharacterAttributes => (0.20, 0.13),
        _ => (0.05, 0.28),
    }
}

fn pixel_luma(p: [u8; 3]) -> u32 {
    (p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8
}

fn pixel_sat(p: [u8; 3]) -> u32 {
    let mx = p.iter().copied().max().unwrap_or(0) as u32;
    let mn = p.iter().copied().min().unwrap_or(0) as u32;
    mx - mn
}

/// Vertical extent of the character model, in pixels.
///
/// Replaces the old hard-coded `head_zone` fractions. Those were measured on a
/// single 1265x711 share capture where the panel filled the frame; on a
/// 1337x796 window capture the located panel also swallows ~150 px of game
/// background, every fraction shifts, and the "hair box" landed on the
/// 人物属性 title scroll — which is how two screenshots of the same character
/// ended up with hair 78 apart in RGB.
///
/// The model is the one tall block of "ink" (dark or saturated pixels) in the
/// left column. A head on its own is small — on a black-haired character the
/// robe is the tallest block and the head sits above it — so the block is grown
/// upward across short gaps, which picks the head back up without dragging in
/// the title bar (that is separated by a much larger gap).
pub(crate) fn character_band(img: &image::RgbaImage, kind: ScreenKind) -> Option<(u32, u32)> {
    let (w, h) = (img.width(), img.height());
    if w < 64 || h < 64 {
        return None;
    }
    let (x0f, x1f) = column_zone(kind);
    let x0 = (x0f * w as f32) as u32;
    let x1 = ((x1f * w as f32) as u32).min(w);
    if x1 <= x0 + 8 {
        return None;
    }
    let step = ((x1 - x0) / 64).max(1);
    let samples = ((x1 - x0 + step - 1) / step).max(1);

    // Start below the top fifth of the frame. A window capture keeps ~150 px of
    // game background above the panel, and a snowy mountainside is both dark
    // and saturated enough to read as one long "ink" block — longer than the
    // character, so it won the longest-run contest on a 包裹 capture and the
    // hair window landed in the sky. No character model ever starts that high.
    let scan_from = ((h as f32 * 0.18) as u32).min(h.saturating_sub(24));
    let mut ink = vec![0u32; (h - scan_from) as usize];
    for y in scan_from..h {
        let mut n = 0u32;
        let mut x = x0;
        while x < x1 {
            let p = img.get_pixel(x, y).0;
            let rgb = [p[0], p[1], p[2]];
            if pixel_luma(rgb) < 110 || pixel_sat(rgb) >= 80 {
                n += 1;
            }
            x += step;
        }
        ink[(y - scan_from) as usize] = n;
    }

    // 10% of the sampled width: a robe easily clears it, a one-pixel outline
    // does not.
    let floor = ((samples as f64 * 0.10).round() as u32).max(3);
    let rows: Vec<bool> = ink.iter().map(|&n| n >= floor).collect();
    let on_at = |y: u32| rows[(y - scan_from) as usize];

    // Longest run, allowing 2 blank rows so a waistline does not split the body.
    let mut best: Option<(u32, u32)> = None;
    let mut start: Option<u32> = None;
    let mut last = scan_from;
    for (i, &on) in rows.iter().enumerate() {
        let y = scan_from + i as u32;
        if on {
            if start.is_none() {
                start = Some(y);
            }
            last = y;
        } else if let Some(s) = start {
            if y - last > 2 {
                if best.map_or(true, |(bs, be)| last - s > be - bs) {
                    best = Some((s, last));
                }
                start = None;
            }
        }
    }
    if let Some(s) = start {
        if best.map_or(true, |(bs, be)| last - s > be - bs) {
            best = Some((s, last));
        }
    }
    let (mut top, bottom) = best?;
    if bottom <= top + 8 {
        return None;
    }

    // Grow upward: a head that reads as its own short run joins the body when
    // the gap between them is small relative to the model.
    let gap_allow = ((bottom - top) as f32 * 0.06).max(6.0) as u32;
    let mut y = top;
    while y > scan_from {
        let mut gap = 0u32;
        let mut probe = y;
        while probe > scan_from && !on_at(probe - 1) && gap <= gap_allow {
            probe -= 1;
            gap += 1;
        }
        if probe <= scan_from || !on_at(probe - 1) {
            break;
        }
        // Walk to the start of that upper run.
        let mut up = probe;
        while up > scan_from && on_at(up - 1) {
            up -= 1;
        }
        top = up;
        y = up;
    }

    Some((top, bottom))
}

/// Bumped whenever the pixel the extractor reports changes meaning.
///
/// 0 — fixed relative box around a head position measured on one capture.
/// 1 — model column located per capture ([`character_band`]), then the
///     dominant colour cluster inside the head band.
///
/// Stored rows carry the number they were written with, because two signatures
/// taken different ways cannot be compared by RGB distance.
pub const APPEARANCE_METHOD: u8 = 1;

/// Names for 发色. Only the buckets a game character actually uses; the name is
/// for the user, the stored RGB is what the comparison uses.
pub(crate) const HAIR_PALETTE: [(&str, [u8; 3]); 11] = [
    ("墨黑", [30, 30, 34]),
    // Navy is the commonest dye in this game, and its measured values slide
    // between (66,69,71) and (84,82,94) depending on the pose. A tight "深紫"
    // entry next door would relabel the same head of hair two different ways;
    // a real purple sits far enough away to stay its own bucket.
    ("深蓝", [55, 58, 82]),
    ("紫", [130, 80, 160]),
    ("深棕", [74, 52, 36]),
    ("金", [222, 186, 112]),
    ("亚麻", [168, 128, 84]),
    ("银白", [208, 210, 216]),
    ("灰白", [150, 150, 154]),
    ("赤红", [150, 52, 48]),
    ("酒红", [96, 40, 44]),
    ("青碧", [52, 120, 116]),
];

/// Names for the bright accent that reads as the held weapon / its glow.
pub(crate) const ACCENT_PALETTE: [(&str, [u8; 3]); 8] = [
    ("青碧", [80, 190, 180]),
    ("金", [214, 172, 72]),
    ("赤", [190, 62, 58]),
    ("紫", [150, 100, 200]),
    ("银", [214, 218, 222]),
    ("蓝", [82, 132, 210]),
    ("翠", [92, 180, 110]),
    ("玄", [70, 70, 78]),
];

pub(crate) fn nearest_name(palette: &[(&str, [u8; 3])], rgb: [u8; 3]) -> String {
    let mut best = palette[0];
    let mut best_d = u32::MAX;
    for (name, c) in palette {
        let d = (rgb[0] as i32 - c[0] as i32).pow(2)
            + (rgb[1] as i32 - c[1] as i32).pow(2)
            + (rgb[2] as i32 - c[2] as i32).pow(2);
        if (d as u32) < best_d {
            best_d = d as u32;
            best = (*name, *c);
        }
    }
    best.0.to_string()
}

/// Mean colour of the pixels in `zone` that satisfy `keep`.
///
/// The mean, not the modal colour: a modal histogram over a 30x30 head crop
/// picks whichever single shadow happens to be largest (the hat's black brim
/// won out on the mounted model), while the mean of all the dark pixels lands
/// within ~30 of the same character's portrait. That is the difference between
/// a feature that links and one that only looks plausible.
pub(crate) fn mean_colour(
    img: &image::RgbaImage,
    zone: ((f32, f32), (f32, f32)),
    keep: impl Fn([u8; 3]) -> bool,
) -> Option<([u8; 3], usize)> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x0 = (zone.0 .0 * w) as u32;
    let x1 = ((zone.0 .1 * w) as u32).min(img.width());
    let y0 = (zone.1 .0 * h) as u32;
    let y1 = ((zone.1 .1 * h) as u32).min(img.height());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let (mut n, mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32, 0u32);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = img.get_pixel(x, y).0;
            if p[3] < 128 {
                continue;
            }
            let rgb = [p[0], p[1], p[2]];
            if !keep(rgb) {
                continue;
            }
            n += 1;
            sr += p[0] as u32;
            sg += p[1] as u32;
            sb += p[2] as u32;
        }
    }
    if n == 0 {
        return None;
    }
    Some((
        [
            (sr / n) as u8,
            (sg / n) as u8,
            (sb / n) as u8,
        ],
        n as usize,
    ))
}

/// The dominant colour cluster inside `zone`, weighted towards its centre.
///
/// A mean over the whole box is what the old extractor did and it is wrong for
/// hair: blonde hair has `max(r,g,b) ≈ 220`, the old dark-only mask dropped it
/// entirely and averaged the frame border instead, so the *same* blonde
/// character read as 深棕 on one panel and 赤红 on the other. Averaging all
/// pixels is no better — the face and the parchment background pull it apart.
///
/// Instead: quantise to 16 levels per channel, keep the largest buckets, and
/// pick the one that is both big and near the centre of the box. The centre
/// weight is what separates "the character's head" from "the decorative border
/// running along the edge of the crop".
pub(crate) fn dominant_colour(
    img: &image::RgbaImage,
    zone: ((f32, f32), (f32, f32)),
    keep: impl Fn([u8; 3]) -> bool,
) -> Option<([u8; 3], usize)> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x0 = (zone.0 .0 * w) as u32;
    let x1 = ((zone.0 .1 * w) as u32).min(img.width());
    let y0 = (zone.1 .0 * h) as u32;
    let y1 = ((zone.1 .1 * h) as u32).min(img.height());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cx, cy) = (
        (x0 + x1) as f32 / 2.0,
        (y0 + y1) as f32 / 2.0,
    );
    let half_diag = (((x1 - x0).pow(2) + (y1 - y0).pow(2)) as f32).sqrt() / 2.0;

    use std::collections::HashMap;
    let mut buckets: HashMap<(u8, u8, u8), [u32; 5]> = HashMap::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let p = img.get_pixel(x, y).0;
            if p[3] < 128 {
                continue;
            }
            let rgb = [p[0], p[1], p[2]];
            if !keep(rgb) {
                continue;
            }
            let key = (p[0] >> 4, p[1] >> 4, p[2] >> 4);
            let e = buckets.entry(key).or_insert([0; 5]);
            e[0] += p[0] as u32;
            e[1] += p[1] as u32;
            e[2] += p[2] as u32;
            e[3] += 1;
            let d = (((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / half_diag.max(1.0))
                .min(1.0);
            // Quantised to 0..1000 so the term stays in integer arithmetic.
            e[4] += (1000.0 * (1.0 - d * 0.85)) as u32;
        }
    }
    if buckets.is_empty() {
        return None;
    }

    // Rank by area, then let the centre weight break ties between neighbours.
    let mut ranked: Vec<([u8; 3], u32, u32)> = buckets
        .values()
        .map(|e| {
            (
                [
                    (e[0] / e[3]) as u8,
                    (e[1] / e[3]) as u8,
                    (e[2] / e[3]) as u8,
                ],
                e[3],
                e[4],
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.truncate(8);

    let score = |area: u32, centre: u32| (area as f64) * (0.35 + centre as f64 / 1000.0);
    let winner = ranked
        .iter()
        .max_by(|a, b| {
            score(a.1, a.2)
                .partial_cmp(&score(b.1, b.2))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()?;

    // Pull in the buckets that are visually the same colour as the winner.
    let (mut n, mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32, 0u32);
    for (rgb, area, _) in &ranked {
        if rgb_distance(*rgb, winner.0) < 48.0 {
            n += area;
            sr += rgb[0] as u32 * area;
            sg += rgb[1] as u32 * area;
            sb += rgb[2] as u32 * area;
        }
    }
    if n == 0 {
        return None;
    }
    Some((
        [(sr / n) as u8, (sg / n) as u8, (sb / n) as u8],
        n as usize,
    ))
}

/// Hue in degrees, 0..360.
pub(crate) fn hue_deg(p: [u8; 3]) -> f32 {
    let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    if d <= 0.0 {
        return 0.0;
    }
    let h = if (mx - r).abs() < f32::EPSILON {
        60.0 * (((g - b) / d) % 6.0)
    } else if (mx - g).abs() < f32::EPSILON {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h + 360.0
    } else {
        h
    }
}

pub(crate) fn rgb_distance(a: [u8; 3], b: [u8; 3]) -> f64 {
    let dr = a[0] as f64 - b[0] as f64;
    let dg = a[1] as f64 - b[1] as f64;
    let db = a[2] as f64 - b[2] as f64;
    (dr * dr + dg * dg + db * db).sqrt()
}

/// 发色 / 武器 of the character rendered on this panel.
///
/// Returns `None` when the panel shows no character model at all (an
/// equipment-only screenshot, a cropped bag grid), which is the honest answer:
/// inventing a signature from the parchment background would poison the
/// comparison for every character.
pub fn appearance_signature(
    img: &image::RgbaImage,
    kind: ScreenKind,
) -> Option<mhcore::AppearanceSignature> {
    let h = img.height() as f32;
    let (xa, xb) = column_zone(kind);
    let (top, bottom) = character_band(img, kind)
        .unwrap_or((((h * 0.25) as u32), ((h * 0.75) as u32).max(1)));
    let band_h = (bottom.saturating_sub(top)).max(1) as f32;

    // The head sits just below the top of that block; how far below depends on
    // the panel, see [`head_window`].
    let (top_frac, height_frac) = head_window(kind);
    let head_top = top as f32 + band_h * top_frac;
    let head_h = (band_h * height_frac).clamp(24.0, band_h * 0.55);
    let head_bottom = (head_top + head_h).min(bottom as f32).min(h);
    let head = (
        (xa, xb),
        (
            (head_top / h).min(1.0),
            (head_bottom / h).min(1.0),
        ),
    );
    if std::env::var("MH_DEBUG_APPEARANCE").is_ok() {
        eprintln!(
            "[appearance] kind={kind:?} img={}x{} zone=({xa},{xb}) band={top}..{bottom} head={head_top:.0}..{head_bottom:.0}",
            img.width(),
            img.height()
        );
    }

    // 发色, in two passes:
    //
    //  1. saturated clusters — 金发, 银白, 红发, 粉发. The old dark-only mask
    //     (`max(r,g,b) <= 150`) threw these away, which is why a blonde
    //     character could read as 深棕 on one panel and 赤红 on the next.
    //  2. dark mean — 墨黑 / 深蓝 hair has almost no saturation, so it needs
    //     the opposite mask.
    //
    // Whatever came out of pass 1 must cover enough pixels to be a head of
    // hair; a few specular highlights on a black-haired model must not win.
    let hair = dominant_colour(img, head, |p| {
        pixel_sat(p) >= 70 && p.iter().copied().max().unwrap_or(0) >= 80
    })
    .filter(|(_, n)| *n >= 60)
    .or_else(|| {
        mean_colour(img, head, |p| p.iter().copied().max().unwrap_or(0) <= 150)
            .filter(|(_, n)| *n >= 60)
    })?;

    // 武器: the game's cyan/teal weapon glow. Deliberately narrow — a generic
    // "bright and saturated" mask picks up the parchment and the gold trim
    // instead, which is what made an earlier version report a weapon colour of
    // "银" for a character holding a cyan spear.
    let model = (
        (0.02f32, 0.44f32),
        ((top as f32 / h).min(1.0), (bottom as f32 / h).min(1.0)),
    );
    let weapon = mean_colour(img, model, |p| {
        let mx = p.iter().copied().max().unwrap_or(0);
        let mn = p.iter().copied().min().unwrap_or(0);
        mx >= 165 && (mx - mn) as u32 * 100 >= mx as u32 * 35 && (140.0..=205.0).contains(&hue_deg(p))
    })
    .filter(|(_, n)| *n >= 40);

    Some(mhcore::AppearanceSignature {
        hair_name: nearest_name(&HAIR_PALETTE, hair.0),
        hair_rgb: hair.0,
        method: APPEARANCE_METHOD,
        weapon_name: weapon.map(|(rgb, _)| nearest_name(&ACCENT_PALETTE, rgb)),
        weapon_rgb: weapon.map(|(rgb, _)| rgb),
    })
}

/// Appearance signature for a whole window capture (panel located + cropped
/// first). Debug helper for `mhcli appearance`; `kind` picks which head zone to
/// use, since the zones differ between panels.
pub fn appearance_for_capture(
    img: &image::RgbaImage,
    kind: ScreenKind,
) -> Option<mhcore::AppearanceSignature> {
    let cropped = locate_panel(img).and_then(|b| crop_rgba(img, b.x, b.y, b.width, b.height));
    let work: &image::RgbaImage = cropped.as_ref().unwrap_or(img);
    appearance_signature(work, kind)
}

/// How well a stored signature explains the current capture, 0.0..=1.0.
///
/// Graded rather than pass/fail: the game lets a character re-dye their hair,
/// swap weapons and change outfits, so "发色 slightly different" must lower the
/// score instead of ruling the character out. Only a clearly different head of
/// hair rejects outright; a different weapon only dampens, because weapon swaps
/// are the commoner change.
pub(crate) fn appearance_strength(
    current: &mhcore::AppearanceSignature,
    stored: &mhcore::AppearanceSignature,
    policy: &MatchPolicy,
) -> Option<(f64, String)> {
    // Two signatures read by different extractors are not comparable: the old
    // one measured the dark pixels of a fixed box that, on a window capture,
    // sat on the panel's title bar. Comparing them would reject every character
    // captured before the upgrade, so say so and keep the signal weak instead.
    if current.method != stored.method {
        return Some((
            policy.method_mismatch_strength,
            "外观取样方式已更新，本次与旧记录不能直接比对".to_string(),
        ));
    }

    let d = rgb_distance(current.hair_rgb, stored.hair_rgb);

    // A large distance damps hard but no longer vetoes. The old code returned
    // None here, which silently deleted the whole signal — one bad hair sample
    // (a title bar, a dye, a lighting change) was enough to make two captures
    // of the same character look unrelated. Dyeing hair and re-shooting a panel
    // are both normal, so the score should drop, not the candidate.
    //
    // The curve is flatter than linear on purpose: the same character on two
    // panels still lands 40-70 apart because the two panels light the model
    // differently, and a linear ramp would leave that at ~0.4 — below the
    // threshold where the signal can decide anything on its own.
    let ramp = (1.0 - d / policy.hair_gate).clamp(0.0, 1.0);
    let mut strength = ramp.powf(policy.hair_curve);
    let mut detail = if stored.hair_name == current.hair_name {
        format!("发色 {} 一致", current.hair_name)
    } else if d <= policy.hair_close {
        format!("发色与记录的 {} 相近", stored.hair_name)
    } else if d >= policy.hair_gate {
        format!(
            "发色与记录的 {} 差异较大（本次 {}），可能是换染或取样偏差",
            stored.hair_name, current.hair_name
        )
    } else {
        format!("发色与记录的 {} 有差异（本次 {}）", stored.hair_name, current.hair_name)
    };
    if d >= policy.hair_gate {
        strength = 0.0;
    }

    if let (Some(a), Some(b)) = (current.weapon_rgb, stored.weapon_rgb) {
        let dw = rgb_distance(a, b);
        if dw >= policy.weapon_gate {
            // 换武器不否决，但要显著压低——否则两个发色相近的角色分不开。
            strength *= 0.3;
            detail.push_str("，武器不同");
        } else {
            let w = 1.0 - dw / policy.weapon_gate;
            strength = (strength * 0.6 + w * 0.4).clamp(0.0, 1.0);
            let same = dw <= 30.0;
            detail.push_str(&format!(
                "，武器{}",
                if same {
                    "同为同一色".to_string()
                } else {
                    format!("颜色相近（距离 {dw:.0}）")
                }
            ));
        }
    }
    Some((strength, detail))
}

/// Legacy single-threshold constants were replaced by [`MatchPolicy`], which
/// keeps the same idea but as named, tunable weights instead of magic numbers.

/// Extract a sub-image, clamped to the source bounds.
pub(crate) fn crop_rgba(
    img: &image::RgbaImage,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Option<image::RgbaImage> {
    let x = x.max(0) as u32;
    let y = y.max(0) as u32;
    let w = w.max(1) as u32;
    let h = h.max(1) as u32;
    if x >= img.width() || y >= img.height() {
        return None;
    }
    let w = w.min(img.width() - x);
    let h = h.min(img.height() - y);
    if w < 32 || h < 32 {
        return None;
    }
    Some(image::imageops::crop_imm(img, x, y, w, h).to_image())
}
