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

/// Where the character's head sits, as fractions of the panel, per panel kind.
///
/// Measured off the real 1265x711 captures (panel 752x498): the 人物属性 panel
/// puts the standing model's head at x 143..203, y 105..169, the 包裹 panel
/// puts the mounted model's head at x 164..200, y 168..204. They are not in the
/// same place — the 包裹 panel's left column is shifted down by the equipment
/// slots — so a single relative box cannot serve both.
pub(crate) fn head_zone(kind: ScreenKind) -> ((f32, f32), (f32, f32)) {
    match kind {
        ScreenKind::CharacterAttributes => ((0.190, 0.270), (0.210, 0.340)),
        _ => ((0.215, 0.275), (0.335, 0.415)),
    }
}

/// Where the character model (and therefore any held weapon) is.
pub(crate) fn model_zone(kind: ScreenKind) -> ((f32, f32), (f32, f32)) {
    match kind {
        ScreenKind::CharacterAttributes => ((0.030, 0.380), (0.180, 0.720)),
        _ => ((0.130, 0.450), (0.260, 0.700)),
    }
}

/// Names for 发色. Only the buckets a game character actually uses; the name is
/// for the user, the stored RGB is what the comparison uses.
pub(crate) const HAIR_PALETTE: [(&str, [u8; 3]); 10] = [
    ("墨黑", [30, 30, 34]),
    // Navy is the commonest dye in this game, and its measured values slide
    // between (66,69,71) and (84,82,94) depending on the pose. A tight "深紫"
    // entry next door would relabel the same head of hair two different ways;
    // a real purple sits far enough away to stay its own bucket.
    ("深蓝", [55, 58, 82]),
    ("紫", [130, 80, 160]),
    ("深棕", [74, 52, 36]),
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
    // 发色: mean of the dark pixels around the head. Shadows on the robe are
    // dark too, but the head zone is small enough that hair dominates it.
    let hair = mean_colour(img, head_zone(kind), |p| {
        p.iter().copied().max().unwrap_or(0) <= 150
    })
    .filter(|(_, n)| *n >= 120)?;

    // 武器: the game's cyan/teal weapon glow. Deliberately narrow — a generic
    // "bright and saturated" mask picks up the parchment and the gold trim
    // instead, which is what made an earlier version report a weapon colour of
    // "银" for a character holding a cyan spear.
    let weapon = mean_colour(img, model_zone(kind), |p| {
        let mx = p.iter().copied().max().unwrap_or(0);
        let mn = p.iter().copied().min().unwrap_or(0);
        mx >= 165 && (mx - mn) as u32 * 100 >= mx as u32 * 35 && (140.0..=205.0).contains(&hue_deg(p))
    })
    .filter(|(_, n)| *n >= 40);

    Some(mhcore::AppearanceSignature {
        hair_name: nearest_name(&HAIR_PALETTE, hair.0),
        hair_rgb: hair.0,
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
    let d = rgb_distance(current.hair_rgb, stored.hair_rgb);
    if d >= policy.hair_gate {
        return None;
    }
    let hair = (1.0 - d / policy.hair_gate).clamp(0.0, 1.0);
    let mut strength = hair;
    let mut detail = if stored.hair_name == current.hair_name {
        format!("发色 {} 一致", current.hair_name)
    } else if d <= policy.hair_close {
        format!("发色与记录的 {} 相近", stored.hair_name)
    } else {
        format!("发色与记录的 {} 有差异（本次 {}）", stored.hair_name, current.hair_name)
    };

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
