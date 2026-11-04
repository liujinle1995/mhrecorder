//! 识别流水线。
//!
//! 把解码、定位、分类、OCR、字段提取、身份关联串成一次调用，并在每张图
//! 之间检查取消 / 超时。
//!
//! 顺序是这套逻辑的关键：**先逐图读完，再决定能不能合并**。旧做法把所有图的
//! OCR 行拼成一个池子、最后统一提取身份，于是「甲的人物属性 + 乙的包裹」会
//! 安静地变成「甲的金币」——池子里同时存在两个角色的身份字段，谁先被扫到就
//! 用谁。现在每张图只认它自己的面板内容，合并前先由
//! [`identity::plan_batch`] 判断这一批到底是不是同一个人。

use crate::analysis::ImageAnalysis;
use crate::appearance::{appearance_signature, crop_rgba};
use crate::classify::{classify_from_lines, classify_screen, locate_panel};
use crate::extract::{
    avatar_geometry, crop_avatar_png, crop_avatar_thumb, extract_character_score, extract_currencies,
    extract_equipment, extract_game_id, extract_inventory, extract_level, extract_nickname,
    extract_school, extract_stats, looks_like_bag, merge_adjacent_fragments,
};
use crate::features::{extract_features, ImageFeatures};
use crate::identity::{
    associate_by_evidence, decide_identity, plan_batch, resolve_identity, BatchPlan,
    IdentityDecision, MatchPolicy, ScreenIdentity,
};
use crate::ocr::OcrEngine;
use crate::prelude::*;
use crate::regions::{build_regions, currency_specs, push_box_region, stat_specs, RegionSpec};
use crate::thumbnail::panel_thumb;

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// A batch's product: the merged result plus the cache ledger.
///
/// The ledger goes back to the caller (`mhservice`): the pipeline holds no
/// storage. Whether to write cache entries, and how many to keep, is a
/// service-layer decision.
pub struct BatchOutcome {
    pub result: RecognitionResult,
    /// Indices of the images whose analysis came from `cache`.
    pub cached_screens: Vec<usize>,
    /// Analysis computed this run, to be written back by the caller.
    pub computed: Vec<(usize, ImageAnalysis)>,
}

/// One screenshot, read on its own.
///
/// This is the intermediate the merge step consumes. Keeping it per-image is
/// what stops one capture's identity leaking into another's numbers.
pub(crate) struct ScreenScan {
    pub(crate) index: usize,
    pub(crate) screen: ScreenKind,
    pub(crate) character: RecognizedCharacter,
    pub(crate) stats: CombatStats,
    pub(crate) currencies: Currencies,
    pub(crate) equipment: Vec<EquipmentItem>,
    pub(crate) inventory: Vec<InventoryItem>,
    pub(crate) confidence: f64,
    pub(crate) fingerprints: Vec<PanelFingerprint>,
    pub(crate) avatar_png: Option<String>,
    pub(crate) avatar_thumb: Option<String>,
    pub(crate) appearance: Option<AppearanceSignature>,
    pub(crate) regions: Vec<FieldRegion>,
}

impl ScreenScan {
    /// A screen we could not read at all. Still occupies its index so the
    /// per-screen list lines up with the submitted images.
    pub(crate) fn unreadable(index: usize, screen: ScreenKind) -> Self {
        Self {
            index,
            screen,
            character: RecognizedCharacter::default(),
            stats: CombatStats::default(),
            currencies: Currencies::default(),
            equipment: Vec::new(),
            inventory: Vec::new(),
            confidence: 0.0,
            fingerprints: Vec::new(),
            avatar_png: None,
            avatar_thumb: None,
            appearance: None,
            regions: Vec::new(),
        }
    }
}

/// High-level pipeline: decode -> classify -> OCR -> field extraction.
pub struct RecognitionPipeline {
    engine: OcrEngine,
}

impl RecognitionPipeline {
    pub fn new(models_dir: &Path) -> Result<Self> {
        Ok(Self { engine: OcrEngine::new(models_dir)? })
    }

    /// 用显式配置建流水线（线程数 / 批大小 / 检测后处理）。
    ///
    /// 配置来自用户的"性能档位"，所以必须能在运行时重建——`mhservice` 在
    /// 设置变更后会丢掉旧流水线、按新配置再建一个。
    pub fn with_config(models_dir: &Path, config: crate::ocr::OcrConfig) -> Result<Self> {
        Ok(Self { engine: OcrEngine::with_config(models_dir, config)? })
    }

    /// 当前引擎配置，供 `version` 回报与日志核对。
    pub fn ocr_config(&self) -> &crate::ocr::OcrConfig {
        self.engine.config()
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
        self.recognize_batch_cached(images, &[], known, refs, control)
            .map(|outcome| outcome.result)
    }

    /// Recognize a batch, reusing per-image analysis from `cache`.
    ///
    /// `cache[i]` is the analysis previously computed for `images[i]`, keyed by
    /// the SHA-256 of its bytes. A hit skips OCR — the expensive step — while
    /// everything after it (fragment merging, field extraction, identity
    /// association, review boxes) still runs, so changing an extractor does not
    /// require invalidating the cache.
    pub fn recognize_batch_cached(
        &mut self,
        images: &[image::RgbaImage],
        cache: &[Option<ImageAnalysis>],
        known: &[Character],
        refs: &[IdentityRef],
        control: &dyn JobControl,
    ) -> Result<BatchOutcome> {
        if images.is_empty() {
            return Err(VisionError::Ocr("empty batch".into()));
        }

        let mut cached_screens: Vec<usize> = Vec::new();
        let mut computed: Vec<(usize, ImageAnalysis)> = Vec::new();
        let mut scans: Vec<ScreenScan> = Vec::with_capacity(images.len());
        let mut warnings: Vec<String> = Vec::new();

        // ---- 第一步：每张图独立读完，谁的信息都不混 ----
        for (screen_index, img) in images.iter().enumerate() {
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
            let panel = locate_panel(img);
            let cropped = panel.and_then(|b| crop_rgba(img, b.x, b.y, b.width, b.height));
            // OCR 坐标相对"裁切后的面板"，审核框画在"原始截图"上，所以要把裁掉
            // 的左上角记下来加回去，否则所有框都会整体偏移一个面板位置。
            let offset = match (&cropped, panel) {
                (Some(_), Some(b)) => (b.x, b.y),
                _ => (0, 0),
            };
            let work: &image::RgbaImage = cropped.as_ref().unwrap_or(img);

            // 缓存命中就跳过 OCR。命中之后仍用 dHash 复核"这条缓存确实是这张
            // 图算出来的"，不一致就当未命中——宁可多花两秒，不能用错行。
            let hit = cache
                .get(screen_index)
                .and_then(Option::as_ref)
                .filter(|analysis| analysis.matches_image(work));

            let lines = if let Some(analysis) = hit {
                cached_screens.push(screen_index);
                analysis.lines.clone()
            } else {
                match self.engine.recognize(work) {
                    Ok(l) => {
                        // 先把能缓存的存下来：哪怕后面提取出错，这份 OCR 也值得留。
                        match ImageAnalysis::compute(work, l.clone()) {
                            Ok(analysis) => computed.push((screen_index, analysis)),
                            Err(e) => tracing::debug!("analysis cache skipped: {e}"),
                        }
                        l
                    }
                    Err(e) => {
                        warnings.push(format!("第 {} 张截图 OCR 失败：{e}", screen_index + 1));
                        scans.push(ScreenScan::unreadable(screen_index, classify_screen(work)));
                        continue;
                    }
                }
            };
            if lines.is_empty() {
                warnings.push(format!("第 {} 张截图没有识别到文字", screen_index + 1));
            }

            // The real game panels render labels with wide letter spacing and
            // the detector emits one box per glyph ("气" + "血"). Re-join those
            // fragments into words before any extractor sees them.
            let lines = merge_adjacent_fragments(&lines);

            // Classify from the panel's own labels. The pixel heuristic below
            // was tuned on panel-only images and called a perfectly readable
            // window capture `Unknown`, which the UI then reported as failure.
            let kind = classify_from_lines(&lines).unwrap_or_else(|| classify_screen(work));

            // Classification only labels the screenshot for the UI. Every
            // extractor runs on every screenshot because field presence is a
            // far more reliable discriminator than a layout guess: a 背包
            // capture can still expose 金币/银币, and a 人物属性 capture may
            // list equipment.
            let stats = extract_stats(&lines);
            let currencies = extract_currencies(&lines);
            let equipment = extract_equipment(&lines);
            // Inventory pairing is per-screenshot: names and counts only make
            // sense relative to each other within one capture. It also needs a
            // 包裹-class panel to be present — on a 人物属性 frame every short
            // CJK run next to a number looks like "item + stack count", which is
            // how a quest tracker became an inventory item.
            let inventory = if looks_like_bag(&lines) {
                extract_inventory(&lines)
            } else {
                Vec::new()
            };

            // 身份字段只从这张图自己的 OCR 行里取 —— 这是整套修复的核心。
            let character = screen_identity(&lines, &stats);

            // Fingerprint the panel itself. A 包裹 screenshot carries no
            // 游戏ID and no nickname, so this thumbnail (plus the 人物评分 when
            // the bag shows one) is the only way a later capture can be linked
            // back to its character.
            let fingerprint = {
                use base64::Engine as _;
                PanelFingerprint {
                    screen: kind,
                    thumb: base64::engine::general_purpose::STANDARD.encode(panel_thumb(work)),
                    score: extract_character_score(&lines),
                }
            };

            let regions = screen_regions(ScreenRegions {
                lines: &lines,
                work,
                screen_index,
                kind,
                offset,
                stats: &stats,
                currencies: &currencies,
                character: &character,
                inventory: &inventory,
            });

            scans.push(ScreenScan {
                index: screen_index,
                screen: kind,
                // A 16x16 grayscale of the portrait: the UI shows the PNG, the
                // matcher compares the thumb. Colour is discarded so a change
                // of dye or lighting does not make the same face score lower.
                avatar_thumb: crop_avatar_thumb(work, &lines),
                avatar_png: crop_avatar_png(work, &lines),
                // 发色 / 武器 of the character on this panel.
                appearance: appearance_signature(work, kind),
                stats,
                currencies,
                equipment,
                inventory,
                character,
                confidence: mean_confidence(&lines),
                fingerprints: vec![fingerprint],
                regions,
            });
        }

        // ---- 第二步：这一批到底是不是同一个人 ----
        let identities: Vec<ScreenIdentity<'_>> = scans
            .iter()
            .map(|s| ScreenIdentity {
                index: s.index,
                game_id: s.character.game_id.as_deref(),
                nickname: s.character.nickname.as_deref(),
            })
            .collect();
        let mut plan = plan_batch(&identities);

        // 没有游戏ID的图，若视觉证据明确指向另一个角色，同样要排除。
        verify_unsigned_screens(&mut plan, &scans, refs, known);

        // ---- 第三步：只合并确认属于同一角色的那些图 ----
        let (mut result, mut confidences) = merge_included(&scans, &plan, images.len(), &warnings);

        // ---- 第四步：这一批是谁 ----
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

        Ok(BatchOutcome { result, cached_screens, computed })
    }

    /// Feature extraction for caching, exposed for the service layer.
    pub fn features(&self, img: &image::RgbaImage) -> Result<ImageFeatures> {
        extract_features(img)
    }
}

/// 把允许合并的图并成批次结果，被排除的图只留在 `screen_results` 里。
///
/// 单独成函数是为了能被测试直接调用：这段逻辑以前混在循环里，没法在不跑 OCR
/// 的情况下证明"乙的包裹不会被算进甲的金币"。
pub(crate) fn merge_included(
    scans: &[ScreenScan],
    plan: &BatchPlan,
    image_count: usize,
    warnings: &[String],
) -> (RecognitionResult, Vec<f64>) {
    let mut result = RecognitionResult::new(IdentitySource::VisualMatch);
    result.screenshot_count = image_count;
    result.screens = scans.iter().map(|s| s.screen).collect();
    result.batch_identity = plan.identity;
    result.warnings.extend(warnings.iter().cloned());
    result.warnings.extend(plan.notes.iter().cloned());
    for (index, reason) in &plan.excluded {
        result
            .conflicts
            .push(format!("第 {} 张截图不属于这批角色：{reason}", index + 1));
    }

    let mut stats_merged = CombatStats::default();
    let mut currency_merged = Currencies::default();
    let mut confidences: Vec<f64> = Vec::new();

    for scan in scans {
        let included = plan.includes(scan.index);
        result.screen_results.push(ScreenResult {
            screen_index: scan.index,
            screen: scan.screen,
            character: scan.character.clone(),
            stats: scan.stats.clone(),
            currencies: scan.currencies.clone(),
            equipment: scan.equipment.clone(),
            inventory: scan.inventory.clone(),
            confidence: scan.confidence,
            included,
            excluded_reason: plan
                .excluded
                .iter()
                .find(|(i, _)| *i == scan.index)
                .map(|(_, reason)| reason.clone()),
            regions: scan.regions.clone(),
        });

        if !included {
            continue;
        }

        merge_stats(&mut stats_merged, &scan.stats);
        merge_currencies(&mut currency_merged, &scan.currencies);

        for item in &scan.equipment {
            if let Some(existing) =
                result.equipment.iter_mut().find(|x| x.slot == item.slot && x.name == item.name)
            {
                existing.confidence = existing.confidence.max(item.confidence);
            } else {
                result.equipment.push(item.clone());
            }
        }
        for item in &scan.inventory {
            if let Some(existing) = result.inventory.iter_mut().find(|x| x.name == item.name) {
                existing.count += item.count;
            } else {
                result.inventory.push(item.clone());
            }
        }

        // 审核框只画进批次结果的那几张图：被排除的图上的数字不属于这条记录。
        result.regions.extend(scan.regions.iter().cloned());
        result.fingerprints.extend(scan.fingerprints.iter().cloned());

        if result.avatar_png_base64.is_none() {
            result.avatar_png_base64 = scan.avatar_png.clone();
        }
        if result.avatar_thumb.is_none() {
            result.avatar_thumb = scan.avatar_thumb.clone();
        }
        if result.appearance.is_none() {
            result.appearance = scan.appearance.clone();
        }

        merge_identity(&mut result.character, &scan.character);
        confidences.push(scan.confidence);
    }

    result.stats = stats_merged;
    result.currencies = currency_merged;

    (result, confidences)
}

/// 这张图自己写着"我是谁" —— 只看这一张图的 OCR 行。
pub(crate) fn screen_identity(lines: &[OcrLine], stats: &CombatStats) -> RecognizedCharacter {
    let game_id = extract_game_id(lines).map(|(gid, _)| gid);
    RecognizedCharacter {
        // Only trust a nickname when the panel actually carries an identity
        // header. A bare 包裹 capture has no name at all, so guessing one from
        // stray panel chrome would invent a character.
        nickname: if game_id.is_some()
            || lines.iter().any(|l| l.text.contains("级") || l.text.contains("Lv"))
        {
            extract_nickname(lines, stats, game_id.as_deref())
        } else {
            None
        },
        game_id,
        level: extract_level(lines).map(|(lvl, _)| lvl),
        school: extract_school(lines),
        // 服务器不在面板上单独成行，目前无从读取；留给手工更正。
        server: None,
    }
}

/// 合并同一角色的多张图的身份字段：先到先得，后来的只补空。
pub(crate) fn merge_identity(dst: &mut RecognizedCharacter, src: &RecognizedCharacter) {
    if dst.game_id.is_none() {
        dst.game_id = src.game_id.clone();
    }
    if dst.nickname.is_none() {
        dst.nickname = src.nickname.clone();
    }
    if dst.level.is_none() {
        dst.level = src.level;
    }
    if dst.school.is_none() {
        dst.school = src.school.clone();
    }
    if dst.server.is_none() {
        dst.server = src.server.clone();
    }
}

fn mean_confidence(lines: &[OcrLine]) -> f64 {
    if lines.is_empty() {
        return 0.0;
    }
    let mean = lines.iter().map(|l| l.confidence).sum::<f64>() / lines.len() as f64;
    mean.clamp(0.0, 1.0)
}

/// 视觉复核没有游戏ID的图。
///
/// 一张包裹截图不写身份，正常情况下它就是同一批里那个人拍的。但"没有身份"
/// 不等于"属于这一批"：如果这张图的版式、评分或外观明确落在库里另一个角色上，
/// 那它就是别人的包裹，不能并进来。证据不足时保持并入——第一次录入时库里
/// 本来就没有可比的参照物，强求验证会让正常流程失败。
fn verify_unsigned_screens(
    plan: &mut BatchPlan,
    scans: &[ScreenScan],
    refs: &[IdentityRef],
    known: &[Character],
) {
    if refs.is_empty() {
        return;
    }
    let Some(primary_gid) = scans
        .iter()
        .filter(|s| plan.includes(s.index))
        .find_map(|s| s.character.game_id.clone())
    else {
        return;
    };

    let mut to_exclude: Vec<(usize, String)> = Vec::new();
    for scan in scans.iter().filter(|s| plan.includes(s.index)) {
        if scan.character.game_id.is_some() || scan.fingerprints.is_empty() {
            continue;
        }
        let assoc = associate_by_evidence(
            &scan.fingerprints,
            refs,
            scan.appearance.as_ref(),
            scan.avatar_thumb.as_deref(),
            now_ms(),
            &MatchPolicy::default(),
        );
        let Some(id) = assoc.character_id.as_deref() else { continue };
        let Some(owner) = known.iter().find(|c| c.id == id) else { continue };
        if let Some(gid) = owner.game_id.as_deref() {
            if gid != primary_gid {
                to_exclude.push((
                    scan.index,
                    format!(
                        "视觉匹配指向「{}」（游戏ID {gid}），与这一批的 {primary_gid} 不同",
                        owner.nickname
                    ),
                ));
            }
        }
    }

    for (index, reason) in to_exclude {
        plan.exclude(index, reason);
    }
}

/// 审核框的输入，参数一多就不适合平铺成位置参数了。
struct ScreenRegions<'a> {
    lines: &'a [OcrLine],
    work: &'a image::RgbaImage,
    screen_index: usize,
    kind: ScreenKind,
    offset: (i32, i32),
    stats: &'a CombatStats,
    currencies: &'a Currencies,
    character: &'a RecognizedCharacter,
    inventory: &'a [InventoryItem],
}

/// ---- 审核框：每个值到底读的是截图上哪一块 ----
///
/// 这一步只回填坐标，不参与识别结论；查不到框的字段会被跳过，所以它不会让
/// 一次成功的识别失败。
fn screen_regions(input: ScreenRegions<'_>) -> Vec<FieldRegion> {
    let mut specs = stat_specs(input.stats);
    specs.extend(currency_specs(input.currencies));

    if let Some(gid) = input.character.game_id.as_deref() {
        specs.push(RegionSpec::from_line(
            "game_id",
            "游戏ID",
            gid,
            None,
            FieldRegion::KIND_IDENTITY,
        ));
    }
    if let Some(level) = input.character.level {
        specs.push(RegionSpec::from_line(
            "level",
            "等级",
            level.to_string(),
            Some(level),
            FieldRegion::KIND_IDENTITY,
        ));
    }
    if let Some(nickname) = input.character.nickname.as_deref() {
        specs.push(RegionSpec::from_line(
            "nickname",
            "昵称",
            nickname,
            None,
            FieldRegion::KIND_IDENTITY,
        ));
    }
    for item in input.inventory {
        specs.push(RegionSpec::from_line(
            format!("item:{}", item.name),
            "物品",
            item.name.clone(),
            Some(item.count),
            FieldRegion::KIND_ITEM,
        ));
    }

    let mut regions = build_regions(
        input.lines,
        input.screen_index,
        input.kind,
        input.offset,
        &specs,
    );

    // 头像不是 OCR 读出来的，但审核时要能看见"匹配时看的是这张脸"。
    if let Some((ax, ay, size)) = avatar_geometry(
        input.lines,
        input.work.width() as i32,
        input.work.height() as i32,
    ) {
        push_box_region(
            &mut regions,
            input.screen_index,
            input.kind,
            input.offset,
            "feature:avatar",
            "头像",
            FieldRegion::KIND_FEATURE,
            ax,
            ay,
            size,
            size,
        );
    }

    regions
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

pub(crate) fn merge_stats(dst: &mut CombatStats, src: &CombatStats) {
    macro_rules! take {
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
