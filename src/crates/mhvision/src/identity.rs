//! 身份关联：多证据加权 + 冲突保护 + 昵称歧义。
//!
//! 回答"这张截图是谁的"。绝不让单一信号定案：面板缩略图、人物评分、发色
//! 武器、头像、时间上下文各记一份证据，加权后还要看领先幅度。

use crate::prelude::*;
use crate::appearance::appearance_strength;
use crate::thumbnail::thumb_similarity;
/// Thresholds and weights for turning several weak signals into one decision.
///
/// These numbers are **heuristics fitted on a few dozen real captures** of one
/// game at one UI scale — not universal constants. They live in a struct so
/// they can be re-fitted (or shadowed per-call in tests) in one place, and so
/// the reasoning is visible instead of hidden in scattered `if x > 0.94`.
///
/// | 信号 | 权重 | 能否单独定案 |
/// |---|---|---|
/// | 面板缩略图 | 0.55 | 强度 ≥ 0.90（≈ 相似度 0.98）时可以 |
/// | 人物评分   | 0.40 | **不能** —— 随装备/等级变化 |
/// | 发色/武器  | 0.35 | 强度 ≥ 0.55 时可以 |
/// | 头像       | 0.25 | 强度 ≥ 0.95 时可以 |
/// | 时间上下文 | 0.10 | 不能 |
///
/// 权重之和刻意大于 1：互相印证的信号能越过判定线，孤立信号停在线上。
///
/// **没有任何信号能否决候选**。曾经唯一的例外是"发色差得远"，但发色读数
/// 本身就会因为换染、光线、面板版式而漂移 —— 一次取样偏差把整条证据删掉，
/// 代价是"先录属性图、再拍包裹图"这个推荐顺序直接失效。现在差得远只是把
/// 这条证据压到 0，其余证据照常加权。
#[derive(Debug, Clone)]
pub struct MatchPolicy {
    pub panel_weight: f64,
    pub score_weight: f64,
    pub appearance_weight: f64,
    pub avatar_weight: f64,
    pub history_weight: f64,
    /// 低于此相似度的两张缩略图不是同一张图（包裹 vs 人物属性 ≈ 0.48，
    /// 不同角色的同版式面板 ≈ 0.82，同一面板重拍 ≈ 0.98）。
    pub panel_floor: f64,
    /// 发色距离超过此值视为"这次的发色读数和记录差太多"，**降低可信度而不是
    /// 否决**——换染、光线、取样偏差都会造成大距离，而得分有加权兜底。
    pub hair_gate: f64,
    /// 发色距离在此以内算"同一个颜色"，而不是"同一个色系".
    pub hair_close: f64,
    /// 发色距离 → 强度的曲线指数，`(1 - d/gate)^curve`。
    ///
    /// < 1 让近距离更接近 1：同一角色跨面板实测 40~70，线性曲线会把这段
    /// 压到 0.4 以下，正好卡在"能单独定案"的门槛之外，于是永远是差一点。
    pub hair_curve: f64,
    /// 武器高光色距离超过此值视为换了武器（阻尼而非否决）。
    pub weapon_gate: f64,
    /// 两条外观记录由不同的取样算法产生时，发色证据的固定强度。
    ///
    /// 旧算法（method 0）的取样框在窗口截图上落在标题栏上，读数本身就是错的，
    /// 拿它和 method 1 的读数比距离毫无意义；给一个弱但非零的强度，让"这两个
    /// 角色看着像不像"不至于因为历史数据而永久失效。
    pub method_mismatch_strength: f64,
    /// 通过所需的最低总分。
    pub min_total: f64,
    /// 与第二名的必备差距，避免"看起来都差不多"时瞎猜。
    pub min_margin: f64,
    /// 各信号可以单独定案的强度线。
    pub decisive_panel: f64,
    pub decisive_appearance: f64,
    pub decisive_avatar: f64,
    /// 上下文证据：此窗口内算"刚刚采集过".
    pub recent_hours: f64,
    /// 超过此天数不再给上下文加分。
    pub stale_days: f64,
}

impl Default for MatchPolicy {
    fn default() -> Self {
        Self {
            panel_weight: 0.55,
            score_weight: 0.40,
            appearance_weight: 0.35,
            avatar_weight: 0.25,
            history_weight: 0.10,
            panel_floor: 0.80,
            // 同一角色跨面板实测 32，不同角色 45——70 只拦"明显不是一个人"。
            // 现在 70 → 90：超过门槛不再一票否决，只是把发色证据压到 0，
            // 于是"录了属性图再拍包裹图"这种跨面板场景不会因为一次取样偏差全盘失败。
            hair_gate: 90.0,
            hair_close: 40.0,
            hair_curve: 0.65,
            // 换武器的代价比换发型小得多，门槛放得比发色宽。
            weapon_gate: 80.0,
            method_mismatch_strength: 0.30,
            // 0.45 → 0.27. The old bar assumed three signals would agree, but
            // a 包裹 capture has no 游戏ID, its panel fingerprint never matches
            // the 人物属性 one, and 人物属性 shows no 评分 — so the only two
            // signals left are appearance and history, worth 0.45 combined at
            // absolute best. 0.27 is what "the look matches reasonably and this
            // character was captured recently" actually sums to; the
            // two-agreeing-signals rule and the margin rule still do the real
            // filtering.
            min_total: 0.27,
            min_margin: 0.08,
            decisive_panel: 0.90,
            decisive_appearance: 0.55,
            decisive_avatar: 0.95,
            recent_hours: 24.0,
            stale_days: 30.0,
        }
    }
}

/// Outcome of weighing every stored character against one capture.
#[derive(Debug, Clone, Default)]
pub struct VisualAssociation {
    /// `Some` only when the evidence clearly singles one character out.
    pub character_id: Option<String>,
    pub confidence: f64,
    /// Why that character won, strongest evidence first.
    pub evidence: Vec<MatchEvidence>,
    /// Ranked candidates, best first. Populated even when nothing was picked,
    /// so the UI can offer a choice instead of a blank.
    pub candidates: Vec<IdentityCandidate>,
}

pub(crate) struct Scored<'a> {
    reference: &'a IdentityRef,
    total: f64,
    evidence: Vec<MatchEvidence>,
    decisive: bool,
}

pub(crate) fn panel_label(kind: ScreenKind) -> &'static str {
    match kind {
        ScreenKind::Inventory => "包裹",
        ScreenKind::CharacterAttributes => "人物属性",
        ScreenKind::Unknown => "面板",
    }
}

/// Weigh one stored character against the current capture.
///
/// Every signal contributes `strength * weight`; nothing vetoes the candidate
/// outright except "obviously different hair", because a changed weapon or a
/// re-shot panel is normal and should lower the score rather than rule someone
/// out.
pub(crate) fn score_candidate(
    reference: &IdentityRef,
    decoded: &[(ScreenKind, Vec<u8>)],
    current_scores: &[i64],
    appearance: Option<&mhcore::AppearanceSignature>,
    avatar_thumb: Option<&[u8]>,
    now_ms: i64,
    policy: &MatchPolicy,
) -> (f64, Vec<MatchEvidence>, bool) {
    use base64::Engine as _;
    let mut evidence: Vec<MatchEvidence> = Vec::new();

    // ---- 1. same panel picture ------------------------------------------
    let mut best_sim = 0.0f64;
    let mut best_kind = ScreenKind::Unknown;
    for (kind, cur) in decoded {
        for stored in &reference.fingerprints {
            if stored.screen != *kind {
                continue;
            }
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&stored.thumb) else {
                continue;
            };
            let sim = thumb_similarity(cur, &bytes);
            if sim > best_sim {
                best_sim = sim;
                best_kind = *kind;
            }
        }
    }
    if best_sim >= policy.panel_floor {
        let strength = ((best_sim - policy.panel_floor) / (1.0 - policy.panel_floor)).clamp(0.0, 1.0);
        evidence.push(MatchEvidence::new(
            EvidenceKind::PanelThumb,
            strength,
            strength * policy.panel_weight,
            format!("{}面板相似度 {:.2}", panel_label(best_kind), best_sim),
        ));
    }

    // ---- 2. 人物评分 (auxiliary: it moves with gear) ---------------------
    if let Some(score) = current_scores
        .iter()
        .copied()
        .find(|s| reference.fingerprints.iter().any(|f| f.score == Some(*s)))
    {
        evidence.push(MatchEvidence::new(
            EvidenceKind::CharacterScore,
            1.0,
            policy.score_weight,
            format!("人物评分 {score} 一致"),
        ));
    } else if evidence.is_empty() && !reference.fingerprints.is_empty() {
        // Nothing else matched. Say which signals could not even be tried, so
        // the candidate list explains itself instead of showing a bare 0.00.
        //
        // 面板指纹按 (角色, 面板类型) 存：一个只录过「人物属性」的角色，拿
        // 「包裹」截图去比，指纹一条都碰不上；而 人物评分 只在 包裹 上显示。
        // 这两件事叠在一起就是"看起来什么都没比"，用户完全看不出原因。
        let kinds: Vec<&str> = reference
            .fingerprints
            .iter()
            .map(|f| panel_label(f.screen))
            .collect();
        let mut missing = Vec::new();
        if !decoded.is_empty() && !decoded.iter().any(|(k, _)| reference.fingerprints.iter().any(|f| f.screen == *k)) {
            missing.push(format!("只存过{}面板的指纹，本次面板无法比对", kinds.join("、")));
        }
        if !current_scores.is_empty() && reference.fingerprints.iter().all(|f| f.score.is_none()) {
            missing.push(format!(
                "没有存过评分（{}面板不显示），本次 {} 无法比对",
                kinds.join("、"),
                current_scores[0]
            ));
        }
        if !missing.is_empty() {
            evidence.push(MatchEvidence::new(
                EvidenceKind::CharacterScore,
                0.0,
                0.0,
                missing.join("；"),
            ));
        }
    }

    // ---- 3. 发色 / 武器 --------------------------------------------------
    if let (Some(cur), Some(stored)) = (appearance, reference.appearance.as_ref()) {
        if let Some((strength, detail)) = appearance_strength(cur, stored, policy) {
            evidence.push(MatchEvidence::new(
                EvidenceKind::Appearance,
                strength,
                strength * policy.appearance_weight,
                detail,
            ));
        }
    }

    // ---- 4. 头像 ---------------------------------------------------------
    if let (Some(cur), Some(stored_b64)) = (avatar_thumb, reference.avatar_thumb.as_ref()) {
        if let Ok(stored) = base64::engine::general_purpose::STANDARD.decode(stored_b64) {
            if !cur.is_empty() && stored.len() == cur.len() {
                let sim = thumb_similarity(cur, &stored);
                if sim >= policy.panel_floor {
                    let strength =
                        ((sim - policy.panel_floor) / (1.0 - policy.panel_floor)).clamp(0.0, 1.0);
                    evidence.push(MatchEvidence::new(
                        EvidenceKind::Avatar,
                        strength,
                        strength * policy.avatar_weight,
                        format!("头像相似度 {:.2}", sim),
                    ));
                }
            }
        }
    }

    // ---- 5. context: seen recently ---------------------------------------
    if let Some(last) = reference.last_seen_at {
        let hours = (now_ms - last).max(0) as f64 / 3_600_000.0;
        let horizon = policy.stale_days * 24.0;
        if hours <= horizon {
            let strength = if hours <= policy.recent_hours {
                1.0
            } else {
                let span = (horizon - policy.recent_hours).max(1.0);
                (1.0 - (hours - policy.recent_hours) / span).clamp(0.0, 1.0)
            };
            let detail = if hours <= policy.recent_hours {
                "最近采集过".to_string()
            } else {
                format!("{:.0} 天前采集过", (hours / 24.0).ceil())
            };
            evidence.push(MatchEvidence::new(
                EvidenceKind::History,
                strength,
                strength * policy.history_weight,
                detail,
            ));
        }
    }

    evidence.sort_by(|a, b| {
        b.contribution
            .partial_cmp(&a.contribution)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total = evidence
        .iter()
        .map(|e| e.contribution)
        .sum::<f64>()
        .clamp(0.0, 1.0);
    let decisive = evidence.iter().any(|e| match e.kind {
        EvidenceKind::PanelThumb => e.strength >= policy.decisive_panel,
        EvidenceKind::Appearance => e.strength >= policy.decisive_appearance,
        EvidenceKind::Avatar => e.strength >= policy.decisive_avatar,
        _ => false,
    });
    (total, evidence, decisive)
}

/// What the pipeline does with the visual evidence.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IdentityDecision {
    /// 面板身份字段与视觉指向不同的人：只记录，**不覆盖**。
    Conflict(String),
    /// 面板没报身份，视觉证据定案。
    Adopt { character_id: String, confidence: f64 },
    /// 没有定论，交回用户。
    Unresolved,
}

/// 冲突保护：面板上写着的身份永远优先于视觉推断。
///
/// 这里刻意没有"视觉置信度高就覆盖"的分支——游戏ID 是唯一权威标识，让视觉
/// 覆盖它就是过拟合风险的来源：换发色、换时装、换坐骑都会让缩略图漂移，
/// 而游戏ID 不会。
pub(crate) fn decide_identity(
    resolved: Option<&str>,
    names_itself: bool,
    source: IdentitySource,
    assoc: &VisualAssociation,
    label: &dyn Fn(&str) -> String,
) -> IdentityDecision {
    match (resolved, assoc.character_id.as_deref()) {
        (Some(chosen), Some(visual)) if chosen != visual => {
            let key = match source {
                IdentitySource::IdField => "游戏ID",
                IdentitySource::Nickname => "昵称",
                _ => "面板身份",
            };
            IdentityDecision::Conflict(format!(
                "视觉匹配指向「{}」，但{key}明确是「{}」，已以{key}为准",
                label(visual),
                label(chosen)
            ))
        }
        (Some(_), _) => IdentityDecision::Unresolved,
        (None, Some(visual)) if !names_itself => IdentityDecision::Adopt {
            character_id: visual.to_string(),
            confidence: assoc.confidence,
        },
        // 面板自己报了身份（新角色），视觉结果不该接管：那正是
        // "把已有角色改名成新截图的名字" 的老 bug。
        (None, Some(_)) => IdentityDecision::Unresolved,
        (None, None) => IdentityDecision::Unresolved,
    }
}

/// Link an unidentified capture to a character by weighing every signal.
///
/// Replaces the old chain of hard thresholds (`if sim >= 0.94 …`) which had
/// three failure modes: a bag whose items changed scored just under the line,
/// 人物评分 was trusted as if it were an identity key, and nothing combined.
///
/// Rules:
///  * one signal alone may decide only when it is decisive (see [`MatchPolicy`]);
///  * otherwise at least two signals must agree, and the winner must beat the
///    runner-up by `min_margin`;
///  * a candidate is only rejected outright for clearly different hair — a
///    swapped weapon, a restyled panel or a new dye all just lower the score.
///
/// Returns ranked candidates even when nothing is conclusive, so the caller can
/// ask the user instead of silently creating a duplicate character.
pub fn associate_by_evidence(
    current: &[PanelFingerprint],
    refs: &[IdentityRef],
    appearance: Option<&mhcore::AppearanceSignature>,
    avatar_thumb: Option<&str>,
    now_ms: i64,
    policy: &MatchPolicy,
) -> VisualAssociation {
    use base64::Engine as _;
    if refs.is_empty() {
        return VisualAssociation::default();
    }

    let decoded: Vec<(ScreenKind, Vec<u8>)> = current
        .iter()
        .filter_map(|f| {
            base64::engine::general_purpose::STANDARD
                .decode(&f.thumb)
                .ok()
                .map(|b| (f.screen, b))
        })
        .collect();
    let current_scores: Vec<i64> = current.iter().filter_map(|f| f.score).collect();
    let avatar = avatar_thumb.and_then(|t| base64::engine::general_purpose::STANDARD.decode(t).ok());

    let mut scored: Vec<Scored> = refs
        .iter()
        .filter_map(|r| {
            let (total, evidence, decisive) = score_candidate(
                r,
                &decoded,
                &current_scores,
                appearance,
                avatar.as_deref(),
                now_ms,
                policy,
            );
            if evidence.is_empty() {
                None
            } else {
                Some(Scored { reference: r, total, evidence, decisive })
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.total
            .partial_cmp(&a.total)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let candidates: Vec<IdentityCandidate> = scored
        .iter()
        .take(3)
        .map(|s| IdentityCandidate {
            character_id: s.reference.character_id.clone(),
            nickname: s.reference.nickname.clone(),
            game_id: s.reference.game_id.clone(),
            server: s.reference.server.clone(),
            level: s.reference.level,
            confidence: round2(s.total),
            evidence: s.evidence.clone(),
        })
        .collect();

    let Some(best) = scored.first() else {
        return VisualAssociation { candidates, ..Default::default() };
    };
    let runner_up = scored.get(1).map(|s| s.total).unwrap_or(0.0);
    let agreeing = best.evidence.iter().filter(|e| e.strength >= 0.5).count();
    // A character whose look was never recorded could be the real owner, and an
    // appearance-only decision could not rule them out. Stay out of the way.
    let appearance_only = best
        .evidence
        .iter()
        .all(|e| matches!(e.kind, EvidenceKind::Appearance | EvidenceKind::History));
    let look_unknown = refs.iter().any(|r| r.appearance.is_none());
    // 两种定案方式：一个信号本身就足够决定性（相似度 0.98 的面板、完全一致的
    // 发色武器），或者两个以上信号互相印证并越过总分线。评分单独永远不够。
    let conclusive = best.total - runner_up >= policy.min_margin
        && !(appearance_only && look_unknown)
        && ((best.total >= policy.min_total && agreeing >= 2) || best.decisive);

    VisualAssociation {
        character_id: if conclusive {
            Some(best.reference.character_id.clone())
        } else {
            None
        },
        confidence: round2(best.total),
        evidence: best.evidence.clone(),
        candidates,
    }
}

pub(crate) fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Identity resolved from what the panel says about itself.
pub(crate) struct IdentityResolution {
    pub(crate) character_id: Option<String>,
    pub(crate) source: IdentitySource,
    pub(crate) confidence: f64,
    pub(crate) candidates: Vec<IdentityCandidate>,
    /// Things the user should know, e.g. "两个角色同名" or "服务器对不上".
    pub(crate) notes: Vec<String>,
}

pub(crate) fn character_candidate(c: &Character, confidence: f64, detail: String) -> IdentityCandidate {
    IdentityCandidate {
        character_id: c.id.clone(),
        nickname: c.nickname.clone(),
        game_id: c.game_id.clone(),
        server: c.server.clone(),
        level: c.level,
        confidence,
        evidence: vec![MatchEvidence::new(
            EvidenceKind::Nickname,
            1.0,
            confidence,
            detail,
        )],
    }
}

/// Who does this capture claim to be?
///
/// 游戏ID is the only authoritative key; everything else is a hint that may
/// have to be confirmed by the user:
///
///  * 游戏ID 命中 → 直接认（0.98）。未命中说明是新角色，不拿别人的行顶上。
///  * 昵称只在**唯一命中**时使用。游戏里不同服务器可以有同名角色，旧实现用
///    `find()` 取第一个，两个区服的重名角色会被悄悄合并成一个。
///  * 同名多于一个 → 返回候选列表、标记 `Ambiguous`，让用户选。
///  * 服务器只用于在同名候选里筛，不反向否决唯一候选。
pub(crate) fn resolve_identity(
    game_id: Option<&str>,
    nickname: Option<&str>,
    server: Option<&str>,
    known: &[Character],
) -> IdentityResolution {
    let unresolved = |source: IdentitySource, confidence: f64, notes: Vec<String>| IdentityResolution {
        character_id: None,
        source,
        confidence,
        candidates: Vec::new(),
        notes,
    };

    if let Some(gid) = game_id.filter(|s| !s.trim().is_empty()) {
        let hits: Vec<&Character> = known
            .iter()
            .filter(|c| c.game_id.as_deref() == Some(gid))
            .collect();
        return match hits.len() {
            1 => IdentityResolution {
                character_id: Some(hits[0].id.clone()),
                source: IdentitySource::IdField,
                confidence: 0.98,
                candidates: Vec::new(),
                notes: Vec::new(),
            },
            // A visible id that matches nothing is still strong proof of a new
            // character — but it is *not* an excuse to pick a look-alike row.
            0 => unresolved(IdentitySource::IdField, 0.95, Vec::new()),
            _ => IdentityResolution {
                character_id: None,
                source: IdentitySource::Ambiguous,
                confidence: 0.5,
                candidates: hits
                    .iter()
                    .map(|c| character_candidate(c, 0.5, format!("游戏ID {gid}")))
                    .collect(),
                notes: vec![format!(
                    "游戏ID {gid} 在库中对应 {} 个角色，库数据可能重复，请在右侧手动确认",
                    hits.len()
                )],
            },
        };
    }

    if let Some(nick) = nickname.filter(|s| !s.trim().is_empty()) {
        let mut hits: Vec<&Character> = known.iter().filter(|c| c.nickname == nick).collect();
        if !hits.is_empty() {
            // 服务器只能帮忙缩小范围，不能反过来把唯一候选否掉。
            if hits.len() > 1 {
                if let Some(srv) = server.filter(|s| !s.is_empty()) {
                    let narrowed: Vec<&Character> = hits
                        .iter()
                        .copied()
                        .filter(|c| c.server.as_deref() == Some(srv))
                        .collect();
                    if !narrowed.is_empty() {
                        hits = narrowed;
                    }
                }
            }

            if hits.len() == 1 {
                let c = hits[0];
                let mismatch = matches!(
                    (server.filter(|s| !s.is_empty()), c.server.as_deref()),
                    (Some(a), Some(b)) if a != b
                );
                return IdentityResolution {
                    character_id: Some(c.id.clone()),
                    source: IdentitySource::Nickname,
                    confidence: if mismatch { 0.70 } else { 0.82 },
                    candidates: Vec::new(),
                    notes: if mismatch {
                        vec![format!(
                            "截图服务器「{}」与「{}」记录的「{}」不一致，已按昵称归属，请确认",
                            server.unwrap_or(""),
                            c.nickname,
                            c.server.clone().unwrap_or_default()
                        )]
                    } else {
                        Vec::new()
                    },
                };
            }

            return IdentityResolution {
                character_id: None,
                source: IdentitySource::Ambiguous,
                confidence: 0.5,
                candidates: hits
                    .iter()
                    .map(|c| character_candidate(c, 0.5, format!("同名「{nick}」")))
                    .collect(),
                notes: vec![format!(
                    "库中有 {} 个角色都叫「{nick}」，无法凭昵称确定归属，请在右侧选择（游戏ID / 服务器可区分）",
                    hits.len()
                )],
            };
        }
    }

    unresolved(IdentitySource::VisualMatch, 0.6, Vec::new())
}

// ---------------------------------------------------------------------------
// 批次身份校验
// ---------------------------------------------------------------------------

/// 一张截图自己报出的身份字段 —— [`plan_batch`] 的唯一输入。
///
/// 只看"这张图上写着什么"，不看视觉证据：批次分组必须能从面板内容复核，
/// 否则出了错没人能解释为什么这两张图被当成同一个人。
#[derive(Debug, Clone, Copy)]
pub struct ScreenIdentity<'a> {
    pub index: usize,
    pub game_id: Option<&'a str>,
    pub nickname: Option<&'a str>,
}

/// 一批截图该怎么合并。
#[derive(Debug, Clone, Default)]
pub struct BatchPlan {
    /// 这一批的身份结论。
    pub identity: BatchIdentity,
    /// 身份当作"这一批是谁"的那张图的下标。
    pub primary: usize,
    /// 允许并进批次结果的图。
    pub included: Vec<usize>,
    /// 被排除的图及其原因：`(screen_index, 原因)`。
    pub excluded: Vec<(usize, String)>,
    /// 给用户看的说明。
    pub notes: Vec<String>,
}

impl BatchPlan {
    pub fn includes(&self, index: usize) -> bool {
        self.included.contains(&index)
    }

    /// 把一张已并入的图排除掉（视觉复核发现它属于别人时用）。
    pub fn exclude(&mut self, index: usize, reason: impl Into<String>) {
        self.included.retain(|i| *i != index);
        let reason = reason.into();
        if !self.excluded.iter().any(|(i, _)| *i == index) {
            self.excluded.push((index, reason.clone()));
        }
        self.identity = BatchIdentity::Conflict;
        self.notes.push(format!("第 {} 张截图已排除：{reason}", index + 1));
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// 按某个键把截图分组，保留首次出现顺序。
fn group_by<'a, F>(screens: &[ScreenIdentity<'a>], key: F) -> Vec<(&'a str, Vec<usize>)>
where
    F: Fn(&ScreenIdentity<'a>) -> Option<&'a str>,
{
    let mut groups: Vec<(&'a str, Vec<usize>)> = Vec::new();
    for screen in screens {
        let Some(value) = key(screen) else { continue };
        match groups.iter_mut().find(|(existing, _)| *existing == value) {
            Some((_, members)) => members.push(screen.index),
            None => groups.push((value, vec![screen.index])),
        }
    }
    groups
}

/// 组里成员最多的那个（并列取最早出现的）。
fn largest<'a, 'b>(groups: &'b [(&'a str, Vec<usize>)]) -> Option<(&'a str, &'b [usize])> {
    groups
        .iter()
        .max_by_key(|(_, members)| (members.len(), usize::MAX - members[0]))
        .map(|(key, members)| (*key, members.as_slice()))
}

/// 决定一批截图能不能合并成一个角色。
///
/// 规则（对应产品要求，顺序即优先级）：
/// 1. 两张图都有游戏ID 且不同 → 冲突，只合并主组；
/// 2. 一张有ID、另一张没有 → 允许合并，后续由视觉复核（[`BatchPlan::exclude`]）；
/// 3. 两张都有昵称且不同 → 冲突；
/// 4. 完全没有身份字段 → 仍是"未知"，由用户确认归属，但不算冲突。
///
/// 冲突时不会丢数据：被排除的图照样进 `screen_results`，只是不参与合并。
pub fn plan_batch(screens: &[ScreenIdentity<'_>]) -> BatchPlan {
    let mut plan = BatchPlan {
        identity: BatchIdentity::Consistent,
        primary: screens.first().map(|s| s.index).unwrap_or(0),
        included: screens.iter().map(|s| s.index).collect(),
        excluded: Vec::new(),
        notes: Vec::new(),
    };
    if screens.is_empty() {
        return plan;
    }

    let by_id = group_by(screens, |s| non_empty(s.game_id));

    match by_id.len() {
        // 多个游戏ID：这一批里混了至少两个角色。
        2.. => {
            let (primary_id, members) = largest(&by_id).expect("non-empty");
            plan.identity = BatchIdentity::Conflict;
            plan.primary = members[0];
            plan.included = members.to_vec();

            for screen in screens {
                if members.contains(&screen.index) {
                    continue;
                }
                let reason = match non_empty(screen.game_id) {
                    Some(other) => format!(
                        "游戏ID {other} 与第 {} 张的 {primary_id} 不同",
                        plan.primary + 1
                    ),
                    None => format!(
                        "这张图没有游戏ID，无法判断它是否属于游戏ID {primary_id} 的角色"
                    ),
                };
                plan.excluded.push((screen.index, reason));
            }
            plan.notes.push(format!(
                "这批截图不是同一个角色：已只合并第 {} 张（游戏ID {primary_id}）那一组，其余 {} 张请单独导入",
                plan.primary + 1,
                plan.excluded.len()
            ));
        }
        // 只有一个游戏ID：这个ID就是这一批的身份。
        1 => {
            let (game_id, members) = &by_id[0];
            let game_id = game_id.to_string();
            let members = members.clone();
            plan.primary = members[0];
            plan.included = members.clone();

            // 主组的昵称，用来判"无ID的图是否明显是别人"。
            let primary_nick = screens
                .iter()
                .filter(|s| members.contains(&s.index))
                .find_map(|s| non_empty(s.nickname));

            for screen in screens {
                if members.contains(&screen.index) {
                    continue;
                }
                match (non_empty(screen.nickname), primary_nick) {
                    (Some(nick), Some(primary)) if nick != primary => {
                        plan.identity = BatchIdentity::Conflict;
                        plan.excluded.push((
                            screen.index,
                            format!("昵称「{nick}」与第 {} 张的「{primary}」不同", plan.primary + 1),
                        ));
                    }
                    _ => plan.included.push(screen.index),
                }
            }

            if plan.identity == BatchIdentity::Conflict {
                plan.notes
                    .push(format!("这批截图里有昵称不一致的图，已只合并游戏ID {game_id} 那一组"));
            } else if plan.included.len() > members.len() {
                let extras = plan
                    .included
                    .iter()
                    .filter(|i| !members.contains(i))
                    .map(|i| (i + 1).to_string())
                    .collect::<Vec<_>>()
                    .join("、");
                plan.notes.push(format!(
                    "第 {extras} 张没有游戏ID，已按同批次的游戏ID {game_id} 归到一起"
                ));
            }
        }
        // 一个游戏ID都没有：退回昵称。
        _ => {
            let by_nick = group_by(screens, |s| non_empty(s.nickname));
            match by_nick.len() {
                2.. => {
                    let (primary_nick, members) = largest(&by_nick).expect("non-empty");
                    plan.identity = BatchIdentity::Conflict;
                    plan.primary = members[0];
                    plan.included = members.to_vec();

                    for screen in screens {
                        if members.contains(&screen.index) {
                            continue;
                        }
                        let reason = match non_empty(screen.nickname) {
                            Some(other) => format!(
                                "昵称「{other}」与第 {} 张的「{primary_nick}」不同",
                                plan.primary + 1
                            ),
                            // 一张没有身份的图夹在两个不同昵称之间：谁都不是它。
                            None => "这批截图出现了多个昵称，无法判断这张无身份信息的图属于谁"
                                .to_string(),
                        };
                        plan.excluded.push((screen.index, reason));
                    }
                    plan.notes.push(format!(
                        "这批截图出现了 {} 个不同昵称，已只合并「{primary_nick}」那一组",
                        by_nick.len()
                    ));
                }
                1 => {
                    let (nickname, members) = &by_nick[0];
                    plan.primary = members[0];
                    plan.included = screens.iter().map(|s| s.index).collect();
                    plan.notes.push(format!("整批按昵称「{nickname}」归到一起"));
                }
                // 什么身份字段都没有。
                _ => {
                    plan.identity = BatchIdentity::Unidentified;
                    plan.primary = screens[0].index;
                    plan.notes.push(
                        "这批截图没有任何身份信息，已按同一角色处理；如果不是同一个人请分开导入"
                            .to_string(),
                    );
                }
            }
        }
    }

    plan
}
