//! 识别流水线。
//!
//! 把解码、定位、分类、OCR、字段提取、身份关联串成一次调用，并在每张图
//! 之间检查取消 / 超时。

use crate::prelude::*;
use crate::appearance::{appearance_signature, crop_rgba};
use crate::classify::{classify_from_lines, classify_screen, locate_panel};
use crate::extract::{
    crop_avatar_png, crop_avatar_thumb, extract_character_score, extract_currencies, extract_equipment, extract_game_id,
    extract_inventory, extract_level, extract_nickname, extract_school, extract_stats,
    looks_like_bag, merge_adjacent_fragments,
};
use crate::features::{extract_features, ImageFeatures};
use crate::identity::{
    associate_by_evidence, decide_identity, resolve_identity, IdentityDecision, MatchPolicy,
};
use crate::ocr::OcrEngine;
use crate::thumbnail::panel_thumb;
// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// High-level pipeline: decode -> classify -> OCR -> field extraction.
pub struct RecognitionPipeline {
    engine: OcrEngine,
}

impl RecognitionPipeline {
    pub fn new(models_dir: &Path) -> Result<Self> {
        Ok(Self { engine: OcrEngine::new(models_dir)? })
    }

    /// Recognize a batch of screenshots belonging to one character.
    ///
    /// `known` carries the characters already in the database so identity
    /// association can match an existing row when the 游戏ID is not visible.
    pub fn recognize_batch(
        &mut self,
        images: &[image::RgbaImage],
        known: &[Character],
    ) -> Result<RecognitionResult> {
        self.recognize_batch_with(images, known, &[], &NoControl)
    }

    /// Recognize a batch, using stored fingerprints to resolve the identity of
    /// captures that carry no identity field (the 包裹 panel).
    pub fn recognize_batch_with(
        &mut self,
        images: &[image::RgbaImage],
        known: &[Character],
        refs: &[IdentityRef],
        control: &dyn JobControl,
    ) -> Result<RecognitionResult> {
        if images.is_empty() {
            return Err(VisionError::Ocr("empty batch".into()));
        }

        let mut result = RecognitionResult::new(IdentitySource::VisualMatch);
        result.screenshot_count = images.len();

        let mut all_lines: Vec<OcrLine> = Vec::new();
        let mut stats_merged = CombatStats::default();
        let mut currency_merged = Currencies::default();
        let mut confidences: Vec<f64> = Vec::new();

        for img in images {
            // 每张图之间是最自然的检查点：单张 OCR 一两秒，取消延迟最多一张。
            if control.cancelled() {
                return Err(VisionError::Cancelled);
            }
            if control.deadline_passed() {
                return Err(VisionError::Timeout);
            }

            // A window capture holds the whole client area, so crop to the game
            // panel before anything else looks at it. Everything below assumes
            // panel-only input; the HUD clock, chat and quest tracker otherwise
            // leak in as currency, inventory items and labels.
            let cropped = locate_panel(img)
                .and_then(|b| crop_rgba(img, b.x, b.y, b.width, b.height));
            let work: &image::RgbaImage = cropped.as_ref().unwrap_or(img);

            let lines = match self.engine.recognize(work) {
                Ok(l) => l,
                Err(e) => {
                    result.screens.push(classify_screen(work));
                    result.warnings.push(format!("OCR 失败：{e}"));
                    continue;
                }
            };
            if lines.is_empty() {
                result.warnings.push("未在截图中识别到文字".to_string());
            }

            // The real game panels render labels with wide letter spacing and
            // the detector emits one box per glyph ("气" + "血"). Re-join those
            // fragments into words before any extractor sees them.
            let lines = merge_adjacent_fragments(&lines);

            // Classify from the panel's own labels. The pixel heuristic below
            // was tuned on panel-only images and called a perfectly readable
            // window capture `Unknown`, which the UI then reported as failure.
            let kind = classify_from_lines(&lines).unwrap_or_else(|| classify_screen(work));
            result.screens.push(kind);

            // Fingerprint the panel itself. A 包裹 screenshot carries no
            // 游戏ID and no nickname, so this thumbnail (plus the 人物评分 when
            // the bag shows one) is the only way a later capture can be linked
            // back to its character.
            {
                use base64::Engine as _;
                let score = extract_character_score(&lines);
                result.fingerprints.push(PanelFingerprint {
                    screen: kind,
                    thumb: base64::engine::general_purpose::STANDARD
                        .encode(panel_thumb(work)),
                    score,
                });
            }

            // Crop the framed portrait once, from the panel that carries an
            // identity header. This is the avatar the UI shows instead of an
            // initial-letter placeholder.
            if result.avatar_png_base64.is_none() {
                if let Some(png) = crop_avatar_png(work, &lines) {
                    result.avatar_png_base64 = Some(png);
                }
            }
            // A 16x16 grayscale of the same portrait: the UI shows the PNG, the
            // matcher compares the thumb. Colour is discarded so a change of
            // dye or lighting does not make the same face score differently.
            if result.avatar_thumb.is_none() {
                result.avatar_thumb = crop_avatar_thumb(work, &lines);
            }

            // 发色 / 武器 of the character on this panel. Recorded once per
            // batch — every screenshot in a batch is the same character, and a
            // later capture whose items changed is linked by this instead of by
            // the panel thumbnail.
            if result.appearance.is_none() {
                result.appearance = appearance_signature(work, kind);
            }

            // Classification only labels the screenshot for the UI. Every
            // extractor runs on every screenshot because field presence is a
            // far more reliable discriminator than a layout guess: a 背包
            // capture can still expose 金币/银币, and a 人物属性 capture may
            // list equipment. Merging by field keeps both paths correct.
            let stats = extract_stats(&lines);
            merge_stats(&mut stats_merged, &stats);
            let currencies = extract_currencies(&lines);
            merge_currencies(&mut currency_merged, &currencies);
            let equipment = extract_equipment(&lines);
            if !equipment.is_empty() {
                for e in equipment {
                    if let Some(existing) =
                        result.equipment.iter_mut().find(|x| x.slot == e.slot && x.name == e.name)
                    {
                        existing.confidence = existing.confidence.max(e.confidence);
                    } else {
                        result.equipment.push(e);
                    }
                }
            }
            // Inventory pairing is per-screenshot: names and counts only make
            // sense relative to each other within one capture. It also needs a
            // 包裹-class panel to be present — on a 人物属性 frame every short
            // CJK run next to a number looks like "item + stack count", which is
            // how a quest tracker became an inventory item.
            let items = if looks_like_bag(&lines) {
                extract_inventory(&lines)
            } else {
                Vec::new()
            };
            for item in items {
                if let Some(existing) =
                    result.inventory.iter_mut().find(|x| x.name == item.name)
                {
                    existing.count += item.count;
                } else {
                    result.inventory.push(item);
                }
            }

            for l in &lines {
                confidences.push(l.confidence);
            }
            all_lines.extend(lines);
        }

        result.stats = stats_merged;
        result.currencies = currency_merged;

        for line in &all_lines {
            if let Some((gid, conf)) = extract_game_id(&[line.clone()].as_slice()) {
                result.character.game_id = Some(gid);
                confidences.push(conf);
                break;
            }
        }
        if result.character.game_id.is_none() {
            if let Some((gid, conf)) = extract_game_id(&all_lines) {
                result.character.game_id = Some(gid);
                confidences.push(conf);
            }
        }
        if let Some((lvl, conf)) = extract_level(&all_lines) {
            result.character.level = Some(lvl);
            confidences.push(conf);
        }
        result.character.school = extract_school(&all_lines);
        // Only trust a nickname when the panel actually carries an identity
        // header. A bare 包裹 capture has no name at all, so guessing one from
        // stray panel chrome would invent a character.
        if result.character.game_id.is_some()
            || all_lines.iter().any(|l| l.text.contains("级") || l.text.contains("Lv"))
        {
            result.character.nickname = extract_nickname(
                &all_lines,
                &result.stats,
                result.character.game_id.as_deref(),
            );
        }

        let resolution = resolve_identity(
            result.character.game_id.as_deref(),
            result.character.nickname.as_deref(),
            result.character.server.as_deref(),
            known,
        );
        let mut character_id = resolution.character_id;
        let mut source = resolution.source;
        let mut identity_conf = resolution.confidence;
        result.candidates = resolution.candidates;
        result.warnings.extend(resolution.notes);

        // Nothing on the panel identifies the character — but we may still know
        // whose picture this is. A 包裹 capture shows no 游戏ID, no nickname and
        // no level; the evidence below is all there is.
        //
        // The evidence is evaluated even when the panel *did* name itself, so a
        // visual model that has drifted shows up as a conflict instead of
        // silently disagreeing with an authoritative 游戏ID.
        let names_itself =
            result.character.game_id.is_some() || result.character.nickname.is_some();

        if !refs.is_empty() {
            let assoc = associate_by_evidence(
                &result.fingerprints,
                refs,
                result.appearance.as_ref(),
                result.avatar_thumb.as_deref(),
                now_ms(),
                &MatchPolicy::default(),
            );

            let label = |id: &str| -> String {
                known
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.nickname.clone())
                    .unwrap_or_else(|| id.to_string())
            };

            match decide_identity(character_id.as_deref(), names_itself, source, &assoc, &label) {
                IdentityDecision::Conflict(note) => result.conflicts.push(note),
                IdentityDecision::Adopt { character_id: id, confidence } => {
                    // Bring the character's name and level along so the result
                    // reads like a real record instead of "未命名角色". The
                    // 游戏ID is left alone on purpose: it is the authoritative
                    // key and must not be promoted from a visual guess.
                    if let Some(c) = known.iter().find(|c| c.id == id) {
                        if result.character.nickname.is_none() {
                            result.character.nickname = Some(c.nickname.clone());
                        }
                        if result.character.level.is_none() {
                            result.character.level = c.level;
                        }
                    }
                    character_id = Some(id);
                    source = IdentitySource::VisualMatch;
                    identity_conf = confidence.max(0.6);
                    result.identity_evidence = Some(evidence_summary(&assoc.evidence));
                }
                IdentityDecision::Unresolved => {
                    // 有候选但没有定论：把候选交给用户，不猜。
                    if !assoc.candidates.is_empty() {
                        result.candidates = assoc.candidates.clone();
                        result.identity_evidence = Some(evidence_summary(&assoc.evidence));
                        result.warnings.push(
                            "证据不足以确定归属，请从右侧角色列表中确认（下方会显示候选及其依据）"
                                .to_string(),
                        );
                    }
                }
            }
        }

        result.character_id = character_id;
        result.identity_source = source;
        confidences.push(identity_conf);

        // The 包裹 panel carries currency but no identity. If we could not
        // resolve a 游戏ID (or a known character), tell the user to import the
        // 人物属性 first — otherwise the currencies would be attached to
        // nobody and silently lost.
        if result.character.game_id.is_none() && result.character_id.is_none() {
            if result.currencies.gold.is_some() || result.currencies.silver.is_some() {
                result.warnings.push(
                    "请先识别并录入「人物属性」截图获取游戏ID，才能把包裹中的金币/银币归到该角色"
                        .to_string(),
                );
            }
        }

        result.confidence = if confidences.is_empty() {
            0.0
        } else {
            let mean = confidences.iter().sum::<f64>() / confidences.len() as f64;
            mean.clamp(0.0, 1.0)
        };

        Ok(result)
    }

    /// Feature extraction for caching, exposed for the service layer.
    pub fn features(&self, img: &image::RgbaImage) -> Result<ImageFeatures> {
        extract_features(img)
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// "包裹面板相似度 0.98；人物评分 20214 一致" — shown next to the confidence so
/// a wrong attribution can be traced to the signal that caused it.
pub(crate) fn evidence_summary(evidence: &[MatchEvidence]) -> String {
    evidence
        .iter()
        .take(3)
        .map(|e| e.detail.clone())
        .collect::<Vec<_>>()
        .join("；")
}

pub(crate) fn merge_stats(dst: &mut CombatStats, src: &CombatStats) {    macro_rules! take {
        ($field:ident) => {
            if dst.$field.is_none() {
                dst.$field = src.$field.clone();
            }
        };
    }
    take!(hp);
    take!(mp);
    take!(physical_damage);
    take!(magic_damage);
    take!(physical_defense);
    take!(magic_defense);
    take!(speed);
    take!(healing_power);
}

pub(crate) fn merge_currencies(dst: &mut Currencies, src: &Currencies) {
    if dst.gold.is_none() {
        dst.gold = src.gold.clone();
    }
    if dst.silver.is_none() {
        dst.silver = src.silver.clone();
    }
}

