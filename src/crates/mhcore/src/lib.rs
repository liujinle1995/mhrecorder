//! Domain types shared across the whole backend and mirrored 1:1 by the
//! C# front end (`MhRecorder.Core/Models.cs`). Field names are serialized in
//! camelCase to match the JSON contract sent over the named pipe.

use serde::{Deserialize, Serialize};

pub mod protocol;

/// Which kind of game screenshot an image was classified as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScreenKind {
    /// 人物属性 panel (character stats + currencies).
    CharacterAttributes,
    /// 背包 / inventory grid.
    Inventory,
    /// Anything we could not classify confidently.
    Unknown,
}

impl Default for ScreenKind {
    fn default() -> Self {
        ScreenKind::Unknown
    }
}

/// How the identity of a character was resolved. Drives the confidence
/// number surfaced in the UI and whether the user must confirm it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentitySource {
    /// Read directly from an explicit 游戏ID field on the screenshot.
    IdField,
    /// Nickname matched exactly one existing character row.
    Nickname,
    /// Visual / behavioural evidence matched a previously stored capture.
    VisualMatch,
    /// Several rows matched equally well (same nickname on different
    /// servers, or two characters sharing a look). Nothing is picked for the
    /// user; the UI must ask.
    Ambiguous,
    /// User typed the identity in by hand.
    Manual,
}

impl IdentitySource {
    /// Whether this resolution may be written without the user confirming it.
    pub fn is_conclusive(self) -> bool {
        matches!(self, IdentitySource::IdField | IdentitySource::Manual)
    }
}

/// A single recognised numeric attribute, carrying enough provenance for the
/// UI to show and correct it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttributeValue {
    pub value: i64,
    /// 0.0 - 1.0 confidence for this specific field.
    pub confidence: f64,
    /// Raw OCR text that produced this value, kept for the correction UI.
    pub raw_text: String,
}

impl AttributeValue {
    pub fn new(value: i64, confidence: f64, raw_text: impl Into<String>) -> Self {
        Self { value, confidence, raw_text: raw_text.into() }
    }

    pub fn missing() -> Self {
        Self { value: 0, confidence: 0.0, raw_text: String::new() }
    }
}

/// The full statistical block shown on the 人物属性 screen.
///
/// `None` means "not visible on this screenshot", which is deliberately
/// distinct from a recognised zero.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CombatStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hp: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mp: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_damage: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magic_damage: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_defense: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magic_defense: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healing_power: Option<AttributeValue>,
}

/// Currency balances. Gold/silver are the two the design surfaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Currencies {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gold: Option<AttributeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silver: Option<AttributeValue>,
}

/// One equipment slot read off the 装备 panel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EquipmentItem {
    pub slot: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    pub confidence: f64,
}

/// One inventory cell read off the 背包 screenshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItem {
    pub name: String,
    pub count: i64,
    /// Grid column/row within the inventory panel, when determinable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<i32>,
    pub confidence: f64,
}

/// A character as the OCR layer believes it to be, before persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecognizedCharacter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub school: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// Compact visual fingerprint of one capture.
///
/// A 包裹 screenshot carries no 游戏ID and no nickname — the panel simply does
/// not render them — so the only way to tell whose bag it is, is to recognise
/// the picture itself. The thumbnail is a 32x32 grayscale of the panel area,
/// which separates "same panel captured again" (~0.98) from "a different
/// panel" (~0.48) far better than the global pHash/HSV pair ever did: those
/// score a bag against a 人物属性 panel at 0.91 because both are bright
/// parchment rectangles in the same screen region.
///
/// The 人物评分 read off the same capture is carried along as an exact,
/// explainable key for when the pixels have moved on (items changed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelFingerprint {
    /// Which panel this thumbnail was taken from.
    pub screen: ScreenKind,
    /// Base64 of a 32x32 row-major grayscale thumbnail (1024 bytes).
    pub thumb: String,
    /// 人物评分 printed on this capture, when the panel shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
}

/// Visible traits that survive item changes and panel switches.
///
/// The thumbnail fingerprint says "this is the same picture again"; these say
/// "this is the same *character*". A 包裹 screenshot renders no 游戏ID, so once
/// the items change the thumbnail drifts and there is nothing left to match on.
/// 发色 does not drift: measured off the real captures, the same character's
/// 人物属性 portrait and 包裹 model come out 10 RGB units apart, while different
/// characters sit hundreds apart.
///
/// 武器 is the bright accent of the model. It is *not* comparable across panel
/// kinds (the character stands in one panel and rides a mount in the other, so
/// the blade is framed differently) — it is only used to confirm a match when
/// both captures came from the same kind of panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSignature {
    /// 发色, as the nearest name from a small palette ("深蓝", "墨黑", ...).
    pub hair_name: String,
    /// The measured 发色, kept so two captures can be compared by distance
    /// instead of by bucket (adjacent buckets are not different characters).
    pub hair_rgb: [u8; 3],
    /// 武器高光色 name, when a bright accent was actually found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon_rgb: Option<[u8; 3]>,
}

impl AppearanceSignature {
    /// One-line description for the UI, e.g. "发色 深蓝 · 武器 青碧".
    pub fn describe(&self) -> String {
        match &self.weapon_name {
            Some(w) => format!("发色 {} · 武器 {}", self.hair_name, w),
            None => format!("发色 {}", self.hair_name),
        }
    }
}

/// One kind of evidence the association layer can weigh.
///
/// Deliberately not a free-form string: the weight each signal carries lives in
/// `mhvision::MatchPolicy`, and the UI groups candidates by this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceKind {
    /// 游戏ID printed on the panel — the only authoritative identity field.
    GameId,
    /// 人物评分. Unique per character *right now*, but it moves when equipment
    /// or level changes, so it is an auxiliary signal, never an identity key.
    CharacterScore,
    /// Panel thumbnail similarity: "this is the same picture again".
    PanelThumb,
    /// 发色 / 武器: describes the character, survives item changes.
    Appearance,
    /// Cropped portrait similarity.
    Avatar,
    /// Recency: the character was captured recently, same session.
    History,
    /// Nickname text. Weak on its own (same name exists on other servers).
    Nickname,
}

/// A single piece of evidence for one candidate, kept so the UI can show *why*
/// a character was picked instead of only a final score.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MatchEvidence {
    pub kind: EvidenceKind,
    /// Raw strength of this signal in 0.0..=1.0, before weighting.
    pub strength: f64,
    /// How much this signal actually contributed (strength * policy weight).
    pub contribution: f64,
    /// Human-readable, e.g. "面板相似度 0.98（包裹）".
    pub detail: String,
}

impl MatchEvidence {
    pub fn new(kind: EvidenceKind, strength: f64, contribution: f64, detail: impl Into<String>) -> Self {
        Self { kind, strength, contribution, detail: detail.into() }
    }
}

/// One character the pipeline considered for a capture that carries no 游戏ID.
///
/// The pipeline returns these instead of guessing whenever the evidence does
/// not clearly single out one row; the UI shows them so the user can pick.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCandidate {
    pub character_id: String,
    pub nickname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    /// Combined confidence in 0.0..=1.0.
    pub confidence: f64,
    /// Every signal that voted for this candidate, strongest first.
    #[serde(default)]
    pub evidence: Vec<MatchEvidence>,
}

/// One character's stored fingerprints, handed to the recognition pipeline so
/// it can link an unidentified capture. Never crosses the pipe.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdentityRef {
    pub character_id: String,
    pub nickname: String,
    pub game_id: Option<String>,
    pub server: Option<String>,
    pub level: Option<i64>,
    pub fingerprints: Vec<PanelFingerprint>,
    /// 发色 / 武器 recorded from this character's earlier captures.
    pub appearance: Option<AppearanceSignature>,
    /// 16x16 grayscale thumbnail of the cropped portrait, when one was saved.
    pub avatar_thumb: Option<String>,
    /// When this character was last captured (ms since epoch), used as
    /// context evidence — a character seen a minute ago is likelier than one
    /// untouched for a month.
    pub last_seen_at: Option<i64>,
}

/// The complete result of running the pipeline over one uploaded batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecognitionResult {
    /// Existing `characters.id` when we are confident this is someone we know.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    pub character: RecognizedCharacter,
    /// Screens we detected in the batch, in order.
    pub screens: Vec<ScreenKind>,
    #[serde(default)]
    pub stats: CombatStats,
    #[serde(default)]
    pub currencies: Currencies,
    #[serde(default)]
    pub equipment: Vec<EquipmentItem>,
    #[serde(default)]
    pub inventory: Vec<InventoryItem>,
    /// Overall confidence for this whole recognition, 0.0 - 1.0.
    pub confidence: f64,
    /// How identity was resolved.
    pub identity_source: IdentitySource,
    /// Non-fatal problems worth surfacing to the user.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Number of source screenshots consumed.
    pub screenshot_count: usize,
    /// Ids of cache entries used, for the "来源" line in the UI.
    #[serde(default)]
    pub cache_hits: Vec<String>,
    /// Base64-encoded PNG of the avatar cropped out of the character panel.
    ///
    /// The 人物属性 screenshot carries a framed portrait next to the game ID.
    /// We crop it here so the UI can show the real character instead of an
    /// initial-letter placeholder. `None` when no avatar could be located.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_png_base64: Option<String>,
    /// Visual fingerprints of the captures in this batch. Persisted against the
    /// resolved character when the user saves, so a later 包裹 capture can be
    /// linked back without any identity field on screen.
    #[serde(default)]
    pub fingerprints: Vec<PanelFingerprint>,
    /// Human-readable explanation of how the identity was resolved, e.g.
    /// "人物评分 20214 与「莫莫莫」一致". Shown next to the confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_evidence: Option<String>,
    /// 发色 / 武器 measured off this batch. Saved against the resolved character
    /// so a later capture whose items have changed can still be linked by what
    /// the character looks like rather than by what the panel looks like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<AppearanceSignature>,
    /// 16x16 grayscale thumbnail of the portrait cropped out of this batch.
    /// Persisted so a future capture can be compared against the face itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_thumb: Option<String>,
    /// Every character the association layer considered, best first. Empty
    /// when the identity came from an explicit 游戏ID (nothing to choose).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<IdentityCandidate>,
    /// Contradictions between signals, e.g. "视觉匹配指向 A，但游戏ID 明确是 B".
    /// The pipeline resolves them in favour of the 游戏ID; they are surfaced so
    /// a wrong visual model becomes visible instead of silently ignored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// Where on the source screenshot each recognised value came from.
    ///
    /// A wrong number is only actionable if you can see *which pixels* it was
    /// read from, so every field carries its OCR box back to the UI. Pixel
    /// coordinates refer to the image as submitted (panel cropping already
    /// folded back in), which is what the review workspace draws on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<FieldRegion>,
}

/// One box on a source screenshot that produced a recognised value.
///
/// `kind` separates what the box means to the reviewer:
/// * `field`    — a numeric field (气血 / 金币 …)
/// * `identity` — 昵称 / 游戏ID / 等级 read off the panel
/// * `item`     — one inventory cell
/// * `feature`  — a region the matcher uses rather than displays (头像 / 外观)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FieldRegion {
    /// Index of the image inside the recognised batch.
    pub screen_index: usize,
    pub screen: ScreenKind,
    /// Machine key: `hp`, `gold`, `nickname`, `item:金柳露`, `feature:avatar` …
    pub key: String,
    /// Human label the UI puts on the box: 气血 / 金币 / 昵称 …
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Raw OCR text inside the box, before any parsing.
    pub raw_text: String,
    /// 0.0 - 1.0 confidence reported by the recogniser for this box.
    pub confidence: f64,
    /// Parsed value when this box carries a number (`None` for 昵称 etc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
    pub kind: String,
}

impl FieldRegion {
    pub const KIND_FIELD: &'static str = "field";
    pub const KIND_IDENTITY: &'static str = "identity";
    pub const KIND_ITEM: &'static str = "item";
    pub const KIND_FEATURE: &'static str = "feature";
}

impl RecognitionResult {
    pub fn new(identity_source: IdentitySource) -> Self {
        Self {
            character_id: None,
            character: RecognizedCharacter::default(),
            screens: Vec::new(),
            stats: CombatStats::default(),
            currencies: Currencies::default(),
            equipment: Vec::new(),
            inventory: Vec::new(),
            confidence: 0.0,
            identity_source,
            warnings: Vec::new(),
            screenshot_count: 0,
            cache_hits: Vec::new(),
            avatar_png_base64: None,
            fingerprints: Vec::new(),
            identity_evidence: None,
            appearance: None,
            avatar_thumb: None,
            candidates: Vec::new(),
            conflicts: Vec::new(),
            regions: Vec::new(),
        }
    }
}

/// A persisted character row plus its latest known snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    pub id: String,
    pub nickname: String,
    #[serde(default)]
    pub game_id: Option<String>,
    #[serde(default)]
    pub level: Option<i64>,
    #[serde(default)]
    pub school: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    /// Confidence that this row's identity is correct.
    pub identity_confidence: f64,
    /// True once a human has corrected/confirmed the row.
    pub identity_confirmed: bool,
    #[serde(default)]
    pub avatar_path: Option<String>,
    /// Milliseconds since epoch; when this character was first imported.
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub latest_stats: CombatStats,
    #[serde(default)]
    pub latest_currencies: Currencies,
}

/// One immutable observation of a character's stats at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub character_id: String,
    pub recorded_at: i64,
    #[serde(default)]
    pub stats: CombatStats,
    #[serde(default)]
    pub currencies: Currencies,
    #[serde(default)]
    pub equipment: Vec<EquipmentItem>,
    #[serde(default)]
    pub inventory: Vec<InventoryItem>,
    pub confidence: f64,
    #[serde(default)]
    pub source_screens: Vec<String>,
}

/// A single time-series point used by the 资产统计 charts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPoint {
    pub date: String,
    /// Day total for 金币, or `None` when no capture that day showed 金币.
    ///
    /// `None` is *not* zero: a 人物属性 screenshot does not render currency at
    /// all, so a day whose only capture was a portrait panel knows nothing
    /// about the balance. Rendering that as 0 produced a fake "金币归零".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silver: Option<i64>,
}

/// Aggregate figures for the top of the 资产统计 page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetSummary {
    pub gold_total: i64,
    pub silver_total: i64,
    pub character_count: i64,
    /// Percentage change vs. the previous snapshot, e.g. 3.2 for +3.2%.
    pub gold_change_percent: Option<f64>,
    pub silver_change_percent: Option<f64>,
    pub character_change: Option<i64>,
}

/// One bar group in the per-character currency comparison chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterAssetBar {
    pub character_id: String,
    pub nickname: String,
    /// Values are in 万元 (10k) to match the chart axis label.
    pub gold_wan: f64,
    pub silver_wan: f64,
}

/// Everything the dashboard needs in one call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatisticsPayload {
    #[serde(default)]
    pub summary: AssetSummary,
    #[serde(default)]
    pub characters: Vec<CharacterAssetBar>,
    #[serde(default)]
    pub recent: Vec<AssetPoint>,
}

/// User-facing settings, persisted alongside the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Show editable fields before saving a recognition.
    pub editable_before_save: bool,
    /// Jump to 角色管理 automatically after a successful save.
    pub auto_navigate_after_save: bool,
    /// Directory holding the database and exported blobs.
    pub data_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            editable_before_save: true,
            auto_navigate_after_save: false,
            data_dir: default_data_dir(),
        }
    }
}

/// Resolve the default on-disk location: `%LOCALAPPDATA%\MhRecorder`.
pub fn default_data_dir() -> String {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    format!("{base}\\MhRecorder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_value_roundtrips_camel_case() {
        let v = AttributeValue::new(7721, 0.98, "7,721");
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("\"rawText\""), "expected camelCase, got {json}");
        let back: AttributeValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn missing_stats_are_omitted_not_zeroed() {
        let stats = CombatStats::default();
        let json = serde_json::to_string(&stats).unwrap();
        assert_eq!(json, "{}", "absent stats must not serialize as zeros");
    }

    #[test]
    fn screen_kind_serializes_as_camel_case_string() {
        assert_eq!(
            serde_json::to_string(&ScreenKind::CharacterAttributes).unwrap(),
            "\"characterAttributes\""
        );
    }

    #[test]
    fn recognition_result_roundtrips() {
        let mut r = RecognitionResult::new(IdentitySource::IdField);
        r.character.nickname = Some("海淘淘".into());
        r.stats.hp = Some(AttributeValue::new(7721, 0.99, "7,721"));
        r.screenshot_count = 2;
        let json = serde_json::to_string(&r).unwrap();
        let back: RecognitionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.character.nickname.as_deref(), Some("海淘淘"));
        assert_eq!(back.stats.hp.unwrap().value, 7721);
    }
}
