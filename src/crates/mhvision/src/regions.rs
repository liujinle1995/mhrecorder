//! 把"字段值"还原成"它在截图上的框"。
//!
//! 审核工作区要回答的问题是「这个数字读错了，错在哪一块像素上」。字段提取阶段
//! 只留下 `AttributeValue { value, confidence, raw_text }`，位置信息在
//! `OcrLine.rect` 里被丢掉了；这里用 `raw_text` 反查回产生它的那一行 OCR，从而
//! 拿到像素框。
//!
//! 为什么不在 `extract` 里顺手带上框：那会把每个提取函数的返回值都改成四元组，
//! 而提取函数被 900 行测试逐字段断言。反查是在同一批 OCR 行上做的纯函数，改动
//! 面小得多，而且天然对"以后新增字段忘了带框"免疫——查不到就不画框，不会崩。

use crate::prelude::*;
use mhcore::FieldRegion;

/// 一条待定位的字段：它的键、给用户看的名字、产生它的原始 OCR 文本。
pub(crate) struct RegionSpec {
    pub key: String,
    pub label: String,
    pub raw: String,
    /// `None` = 这条没有单独的置信度（昵称、游戏ID 是整行读出来的），用 OCR 行自己的。
    pub confidence: Option<f64>,
    pub value: Option<i64>,
    pub kind: &'static str,
}

impl RegionSpec {
    pub(crate) fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        raw: impl Into<String>,
        confidence: f64,
        value: Option<i64>,
        kind: &'static str,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            raw: raw.into(),
            confidence: Some(confidence),
            value,
            kind,
        }
    }

    /// 置信度取自命中的那一行 OCR，而不是某个字段级分数。
    pub(crate) fn from_line(
        key: impl Into<String>,
        label: impl Into<String>,
        raw: impl Into<String>,
        value: Option<i64>,
        kind: &'static str,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            raw: raw.into(),
            confidence: None,
            value,
            kind,
        }
    }
}

/// 去掉空白与千分位/冒号，只用来比对，不参与解析。
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_whitespace() && !matches!(c, ',' | '，' | '：' | ':' | '·' | '．' | '.')
        })
        .collect()
}

/// 在一批 OCR 行里找出产生 `raw` 的那一行。
///
/// 三级降级，从最可信到最宽松：
/// 1. 整行文本完全一致（值单独成框，最常见）；
/// 2. 行内包含该值（标签与值挤在同一个框：`物理伤害709`）；
/// 3. 该值是纯数字且行内出现的数字与它相等（OCR 把 `7,721` 拆成 `7` `721`）。
fn find_line<'a>(lines: &'a [OcrLine], raw: &str) -> Option<&'a OcrLine> {
    let want = normalize(raw);
    if want.is_empty() {
        return None;
    }

    if let Some(line) = lines.iter().find(|l| normalize(&l.text) == want) {
        return Some(line);
    }

    // 包含匹配时优先"最短的容器"：`709` 同时出现在 `物理伤害709` 和一行别的
    // 长文本里时，前者才是它的来源。
    let mut best: Option<&OcrLine> = None;
    for line in lines {
        let text = normalize(&line.text);
        if !text.contains(&want) {
            continue;
        }
        let shorter = best
            .map(|b| text.len() < normalize(&b.text).len())
            .unwrap_or(true);
        if shorter {
            best = Some(line);
        }
    }
    if best.is_some() {
        return best;
    }

    let digits: String = want.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 3 || digits.len() != want.len() {
        return None;
    }
    let wanted: i64 = digits.parse().ok()?;
    lines.iter().find(|l| {
        let text = normalize(&l.text);
        text.chars().all(|c| c.is_ascii_digit()) && text.parse::<i64>().ok() == Some(wanted)
    })
}

/// 把 specs 全部翻译成 `FieldRegion`，查不到框的字段直接跳过。
///
/// `offset` 是面板裁切时被切掉的左上角：OCR 坐标相对裁切后的图，UI 画在原始
/// 截图上，必须加回去。
pub(crate) fn build_regions(
    lines: &[OcrLine],
    screen_index: usize,
    screen: ScreenKind,
    offset: (i32, i32),
    specs: &[RegionSpec],
) -> Vec<FieldRegion> {
    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        let Some(line) = find_line(lines, &spec.raw) else { continue };
        out.push(FieldRegion {
            screen_index,
            screen,
            key: spec.key.clone(),
            label: spec.label.clone(),
            x: line.rect.0 + offset.0,
            y: line.rect.1 + offset.1,
            width: line.rect.2,
            height: line.rect.3,
            raw_text: line.text.clone(),
            confidence: spec.confidence.unwrap_or(line.confidence),
            value: spec.value,
            kind: spec.kind.to_string(),
        });
    }
    out
}

/// 直接把一个已知的像素框登记成区域（头像、外观这类不是 OCR 读出来的）。
pub(crate) fn push_box_region(
    out: &mut Vec<FieldRegion>,
    screen_index: usize,
    screen: ScreenKind,
    offset: (i32, i32),
    key: &str,
    label: &str,
    kind: &'static str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    out.push(FieldRegion {
        screen_index,
        screen,
        key: key.to_string(),
        label: label.to_string(),
        x: x + offset.0,
        y: y + offset.1,
        width,
        height,
        raw_text: String::new(),
        confidence: 1.0,
        value: None,
        kind: kind.to_string(),
    });
}

/// 数值字段（气血 / 物伤 / 金币 …）的 spec 列表。
pub(crate) fn stat_specs(stats: &CombatStats) -> Vec<RegionSpec> {
    let mut specs = Vec::new();
    let mut push = |key: &str, label: &str, av: &Option<AttributeValue>| {
        if let Some(v) = av {
            specs.push(RegionSpec::new(
                key,
                label,
                v.raw_text.clone(),
                v.confidence,
                Some(v.value),
                FieldRegion::KIND_FIELD,
            ));
        }
    };
    push("hp", "气血", &stats.hp);
    push("mp", "魔法", &stats.mp);
    push("physical_damage", "物伤", &stats.physical_damage);
    push("magic_damage", "法伤", &stats.magic_damage);
    push("physical_defense", "物防", &stats.physical_defense);
    push("magic_defense", "法防", &stats.magic_defense);
    push("speed", "速度", &stats.speed);
    push("healing_power", "治疗强度", &stats.healing_power);
    specs
}

/// 货币字段的 spec 列表。
pub(crate) fn currency_specs(currencies: &Currencies) -> Vec<RegionSpec> {
    let mut specs = Vec::new();
    if let Some(gold) = &currencies.gold {
        specs.push(RegionSpec::new(
            "gold",
            "金币",
            gold.raw_text.clone(),
            gold.confidence,
            Some(gold.value),
            FieldRegion::KIND_FIELD,
        ));
    }
    if let Some(silver) = &currencies.silver {
        specs.push(RegionSpec::new(
            "silver",
            "银币",
            silver.raw_text.clone(),
            silver.confidence,
            Some(silver.value),
            FieldRegion::KIND_FIELD,
        ));
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, rect: (i32, i32, i32, i32)) -> OcrLine {
        OcrLine { text: text.to_string(), confidence: 0.9, rect }
    }

    #[test]
    fn finds_exact_line() {
        let lines = vec![line("7,721 / 7,721", (100, 200, 120, 20))];
        let found = find_line(&lines, "7,721 / 7,721").expect("exact");
        assert_eq!(found.rect, (100, 200, 120, 20));
    }

    #[test]
    fn prefers_shortest_container() {
        let lines = vec![
            line("气血 7,721 / 7,721 战斗属性说明", (10, 10, 400, 20)),
            line("物理伤害709", (10, 60, 100, 20)),
        ];
        let found = find_line(&lines, "709").expect("contained");
        assert_eq!(found.rect.1, 60);
    }

    #[test]
    fn matches_digits_split_by_ocr() {
        let lines = vec![line("7721", (5, 5, 40, 16))];
        assert_eq!(find_line(&lines, "7,721").map(|l| l.rect), Some((5, 5, 40, 16)));
    }

    #[test]
    fn offset_is_applied() {
        let lines = vec![line("2,942,685", (10, 20, 80, 18))];
        let specs = vec![RegionSpec::new(
            "gold",
            "金币",
            "2,942,685",
            0.97,
            Some(2942685),
            FieldRegion::KIND_FIELD,
        )];
        let regions = build_regions(&lines, 0, ScreenKind::Inventory, (30, 40), &specs);
        assert_eq!(regions.len(), 1);
        assert_eq!((regions[0].x, regions[0].y), (40, 60));
        assert_eq!(regions[0].label, "金币");
    }
}
