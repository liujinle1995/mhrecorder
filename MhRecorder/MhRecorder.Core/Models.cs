// Mirror of the Rust domain types in `src/crates/mhcore/src/lib.rs`.
// Every field is serialized camelCase because the named-pipe contract is the
// Rust serde output verbatim. Keep this file in lock-step with mhcore.

using System.Text.Json.Serialization;

namespace MhRecorder.Core.Models;

/// <summary>Which kind of game screenshot an image was classified as.</summary>
public enum ScreenKind
{
    CharacterAttributes,
    Inventory,
    Unknown,
}

/// <summary>How the identity of a character was resolved (drives the confidence badge).</summary>
public enum IdentitySource
{
    /// <summary>Read directly from an explicit 游戏ID field on the screenshot.</summary>
    IdField,
    /// <summary>Nickname matched an existing character row.</summary>
    Nickname,
    /// <summary>Visual / behavioural evidence matched a previously stored capture.</summary>
    VisualMatch,
    /// <summary>
    /// Several rows matched equally well (same nickname on another server, or two
    /// characters sharing a look). Nothing is picked for the user; the UI must ask.
    /// </summary>
    Ambiguous,
    /// <summary>User typed the identity in by hand.</summary>
    Manual,
}

/// <summary>One recognised numeric attribute with provenance for the correction UI.</summary>
public sealed class AttributeValue
{
    public long Value { get; set; }

    /// <summary>0.0 - 1.0 confidence for this specific field.</summary>
    public double Confidence { get; set; }

    /// <summary>Raw OCR text that produced this value.</summary>
    public string RawText { get; set; } = string.Empty;
}

/// <summary>The statistical block shown on the 人物属性 screen. Null means "not visible".</summary>
public sealed class CombatStats
{
    public AttributeValue? Hp { get; set; }
    public AttributeValue? Mp { get; set; }
    public AttributeValue? PhysicalDamage { get; set; }
    public AttributeValue? MagicDamage { get; set; }
    public AttributeValue? PhysicalDefense { get; set; }
    public AttributeValue? MagicDefense { get; set; }
    public AttributeValue? Speed { get; set; }
    public AttributeValue? HealingPower { get; set; }
}

/// <summary>Currency balances: gold and silver, the two the design surfaces.</summary>
public sealed class Currencies
{
    public AttributeValue? Gold { get; set; }
    public AttributeValue? Silver { get; set; }
}

/// <summary>One equipment slot read off the 装备 panel.</summary>
public sealed class EquipmentItem
{
    public string Slot { get; set; } = string.Empty;
    public string Name { get; set; } = string.Empty;
    public long? Level { get; set; }
    public double Confidence { get; set; }
}

/// <summary>One inventory cell read off the 背包 screenshot.</summary>
public sealed class InventoryItem
{
    public string Name { get; set; } = string.Empty;
    public long Count { get; set; }
    public int? Column { get; set; }
    public int? Row { get; set; }
    public double Confidence { get; set; }
}

/// <summary>A character as the OCR layer believes it to be, before persistence.</summary>
public sealed class RecognizedCharacter
{
    public string? Nickname { get; set; }
    public string? GameId { get; set; }
    public long? Level { get; set; }
    public string? School { get; set; }
    public string? Server { get; set; }
}

/// <summary>The complete result of running the pipeline over one uploaded batch.</summary>
public sealed class RecognitionResult
{
    /// <summary>Existing character id when we are confident this is someone we know.</summary>
    public string? CharacterId { get; set; }
    public RecognizedCharacter Character { get; set; } = new();
    public List<ScreenKind> Screens { get; set; } = new();
    public CombatStats Stats { get; set; } = new();
    public Currencies Currencies { get; set; } = new();
    public List<EquipmentItem> Equipment { get; set; } = new();
    public List<InventoryItem> Inventory { get; set; } = new();

    /// <summary>Overall confidence for this whole recognition, 0.0 - 1.0.</summary>
    public double Confidence { get; set; }

    public IdentitySource IdentitySource { get; set; }
    public List<string> Warnings { get; set; } = new();

    /// <summary>
    /// Contradictions between signals, e.g. "视觉匹配指向 A，但游戏ID 明确是 B".
    /// The backend always resolves them in favour of the 游戏ID, so these are
    /// shown for diagnosis rather than as errors.
    /// </summary>
    public List<string> Conflicts { get; set; } = new();

    /// <summary>
    /// Characters the matcher considered, best first. Populated when the evidence
    /// did not single one out, so the user can choose instead of guessing.
    /// </summary>
    public List<IdentityCandidate> Candidates { get; set; } = new();
    public int ScreenshotCount { get; set; }
    public List<string> CacheHits { get; set; } = new();

    /// <summary>
    /// Base64 PNG of the portrait cropped out of the character panel.
    /// The UI turns this into a bitmap so the card shows the real avatar
    /// instead of an initial-letter placeholder.
    /// </summary>
    public string? AvatarPngBase64 { get; set; }

    /// <summary>
    /// Visual fingerprints of the captures in this batch. Round-tripped back on
    /// save so the backend can remember what this character's panels look like
    /// and link a later 包裹 screenshot — which shows no 游戏ID — to them.
    /// </summary>
    public List<PanelFingerprint> Fingerprints { get; set; } = new();

    /// <summary>
    /// How the identity was resolved, in words: "人物评分 20214 与「莫莫莫」一致".
    /// Null when the identity came straight off the panel (游戏ID / nickname).
    /// </summary>
    public string? IdentityEvidence { get; set; }

    /// <summary>
    /// 发色 / 武器 read off the character model on this capture. Recorded on save
    /// so a later capture whose items (and therefore panel pixels) have changed
    /// can still be linked by what the character looks like.
    /// </summary>
    public AppearanceSignature? Appearance { get; set; }

    /// <summary>
    /// Where on the source screenshot each recognised value came from.
    ///
    /// The review workspace draws these boxes over the capture, so a wrong
    /// number can be traced to the pixels it was read from instead of only
    /// showing up as a wrong number. Coordinates are in the submitted image's
    /// pixel space (panel cropping already folded back in).
    /// </summary>
    public List<FieldRegion> Regions { get; set; } = new();

    /// <summary>
    /// Every screenshot's own reading, one entry per submitted image.
    ///
    /// The top-level stats / currencies / inventory above are a *merged* view,
    /// and merging only makes sense when the batch really is one character.
    /// This keeps the per-image values, so "which screenshot did this gold come
    /// from" always has an answer — including for the screens that were left
    /// out of the merge.
    /// </summary>
    public List<ScreenResult> ScreenResults { get; set; } = new();

    /// <summary>Whether every screenshot in the batch points at the same character.</summary>
    public BatchIdentity BatchIdentity { get; set; }

    /// <summary>Screens that were read but not merged (a different character).</summary>
    [JsonIgnore]
    public List<ScreenResult> ExcludedScreens =>
        ScreenResults.Where(s => !s.Included).ToList();

    [JsonIgnore]
    public bool HasScreenConflict => BatchIdentity == BatchIdentity.Conflict;
}

/// <summary>Verdict on whether a batch of screenshots is one character.</summary>
public enum BatchIdentity
{
    /// <summary>All screens agree (or carry no identity field but contradict nothing).</summary>
    Consistent,

    /// <summary>At least two screens point at different characters.</summary>
    Conflict,

    /// <summary>No screen carries any identity field: nobody knows whose this is.</summary>
    Unidentified,
}

/// <summary>One screenshot's own recognition result.</summary>
public sealed class ScreenResult
{
    public int ScreenIndex { get; set; }
    public ScreenKind Screen { get; set; }

    /// <summary>Identity fields read off *this* panel only.</summary>
    public RecognizedCharacter Character { get; set; } = new();

    public CombatStats Stats { get; set; } = new();
    public Currencies Currencies { get; set; } = new();
    public List<EquipmentItem> Equipment { get; set; } = new();
    public List<InventoryItem> Inventory { get; set; } = new();
    public double Confidence { get; set; }

    /// <summary>Whether this screenshot's numbers went into the merged result.</summary>
    public bool Included { get; set; }

    /// <summary>Why it did not, e.g. "游戏ID … 与第 1 张的 … 不同".</summary>
    public string? ExcludedReason { get; set; }

    public List<FieldRegion> Regions { get; set; } = new();
}

/// <summary>
/// A recognition that could not be attributed to anyone, kept until a human
/// says whose it is. Saved instead of inventing a「未命名角色」row.
/// </summary>
public sealed class PendingRecognition
{
    public string Id { get; set; } = string.Empty;
    public long CreatedAt { get; set; }
    public string Reason { get; set; } = string.Empty;
    public double Confidence { get; set; }
    public string? Nickname { get; set; }
    public string? GameId { get; set; }
    public int ScreenshotCount { get; set; }
    public List<string> Screens { get; set; } = new();
    public bool HasCurrencies { get; set; }
}

public sealed class PendingPayload
{
    public List<PendingRecognition> Items { get; set; } = new();
    public long Count { get; set; }
}

/// <summary>One box on a source screenshot that produced a recognised value.</summary>
public sealed class FieldRegion
{
    /// <summary>Index of the image inside the recognised batch.</summary>
    public int ScreenIndex { get; set; }

    public ScreenKind Screen { get; set; }

    /// <summary>Machine key: <c>hp</c>, <c>gold</c>, <c>nickname</c>, <c>item:金柳露</c> …</summary>
    public string Key { get; set; } = string.Empty;

    /// <summary>Label the UI puts on the box: 气血 / 金币 / 昵称 …</summary>
    public string Label { get; set; } = string.Empty;

    public int X { get; set; }
    public int Y { get; set; }
    public int Width { get; set; }
    public int Height { get; set; }

    /// <summary>Raw OCR text inside the box, before any parsing.</summary>
    public string RawText { get; set; } = string.Empty;

    /// <summary>0.0 - 1.0 confidence for the box.</summary>
    public double Confidence { get; set; }

    /// <summary>Parsed value when the box carries a number.</summary>
    public long? Value { get; set; }

    /// <summary><c>field</c> | <c>identity</c> | <c>item</c> | <c>feature</c></summary>
    public string Kind { get; set; } = FieldRegionKind.Field;

    public bool IsFeature => Kind == FieldRegionKind.Feature;
    public bool IsItem => Kind == FieldRegionKind.Item;
}

public static class FieldRegionKind
{
    public const string Field = "field";
    public const string Identity = "identity";
    public const string Item = "item";
    public const string Feature = "feature";
}

/// <summary>One candidate the matcher considered for an unidentified capture.</summary>
public sealed class IdentityCandidate
{
    public string CharacterId { get; set; } = string.Empty;
    public string Nickname { get; set; } = string.Empty;
    public string? GameId { get; set; }
    public string? Server { get; set; }
    public long? Level { get; set; }

    /// <summary>Combined 0..1 score of every signal that voted for this row.</summary>
    public double Confidence { get; set; }

    /// <summary>Per-signal evidence, strongest first.</summary>
    public List<MatchEvidence> Evidence { get; set; } = new();

    /// <summary>"莫莫莫 · 包裹面板相似度 0.98；人物评分 20214 一致"</summary>
    public string Describe()
    {
        var name = string.IsNullOrWhiteSpace(Nickname) ? CharacterId : Nickname;
        var why = Evidence.Count == 0 ? "无依据" : string.Join("；", Evidence.Take(3).Select(e => e.Detail));
        return $"{name}（{Confidence:P0}）{why}";
    }
}

/// <summary>One signal that contributed to a candidate's score.</summary>
public sealed class MatchEvidence
{
    /// <summary>gameId / characterScore / panelThumb / appearance / avatar / history / nickname.</summary>
    public string Kind { get; set; } = string.Empty;

    public double Strength { get; set; }
    public double Contribution { get; set; }
    public string Detail { get; set; } = string.Empty;
}

/// <summary>
/// Visible traits that identify a *character* rather than a picture.
/// Carried from recognition to save untouched; the backend compares the raw
/// colours, the names are only for showing the user what was recorded.
/// </summary>
public sealed class AppearanceSignature
{
    /// <summary>发色 name from a small palette, e.g. "深蓝".</summary>
    public string HairName { get; set; } = string.Empty;

    /// <summary>The measured 发色; comparison uses this, not the name.</summary>
    public int[] HairRgb { get; set; } = Array.Empty<int>();

    /// <summary>武器高光色 name, when a glow was actually found.</summary>
    public string? WeaponName { get; set; }

    public int[]? WeaponRgb { get; set; }

    /// <summary>"发色 深蓝 · 武器 青碧" for the result panel.</summary>
    public string Describe() =>
        string.IsNullOrEmpty(WeaponName) ? $"发色 {HairName}" : $"发色 {HairName} · 武器 {WeaponName}";
}

/// <summary>
/// One capture's visual fingerprint, as produced by the recogniser.
/// Opaque to the UI: it is carried from recognition to save and never inspected.
/// </summary>
public sealed class PanelFingerprint
{
    public ScreenKind Screen { get; set; }
    /// <summary>Base64 of a 32x32 grayscale thumbnail.</summary>
    public string Thumb { get; set; } = string.Empty;
    /// <summary>人物评分 printed on this capture, when the panel shows it.</summary>
    public long? Score { get; set; }
}

/// <summary>A persisted character row plus its latest known snapshot.</summary>
public sealed class Character
{
    public string Id { get; set; } = string.Empty;
    public string Nickname { get; set; } = string.Empty;
    public string? GameId { get; set; }
    public long? Level { get; set; }
    public string? School { get; set; }
    public string? Server { get; set; }
    public double IdentityConfidence { get; set; }
    public bool IdentityConfirmed { get; set; }
    public string? AvatarPath { get; set; }

    /// <summary>Milliseconds since epoch.</summary>
    public long CreatedAt { get; set; }
    public long UpdatedAt { get; set; }
    public CombatStats LatestStats { get; set; } = new();
    public Currencies LatestCurrencies { get; set; } = new();
}

/// <summary>A character plus how many observations back it.</summary>
public sealed class CharacterDetail
{
    public Character Character { get; set; } = new();
    public int SnapshotCount { get; set; }
    public long LastRecordedAt { get; set; }
}

/// <summary>One immutable observation of a character's stats at a point in time.</summary>
public sealed class Snapshot
{
    public string Id { get; set; } = string.Empty;
    public string CharacterId { get; set; } = string.Empty;
    public long RecordedAt { get; set; }
    public CombatStats Stats { get; set; } = new();
    public Currencies Currencies { get; set; } = new();
    public List<EquipmentItem> Equipment { get; set; } = new();
    public List<InventoryItem> Inventory { get; set; } = new();
    public double Confidence { get; set; }
    public List<string> SourceScreens { get; set; } = new();
}

/// <summary>A single time-series point used by the 资产统计 charts.</summary>
public sealed class AssetPoint
{
    public string Date { get; set; } = string.Empty;

    /// <summary>
    /// Day total, or <c>null</c> when no capture that day showed 金币.
    /// Null is not zero: a 人物属性 screenshot renders no currency at all, and
    /// treating that as 0 produced a fake "金币归零".
    /// </summary>
    public long? Gold { get; set; }

    public long? Silver { get; set; }
}

/// <summary>Aggregate figures for the top of the 资产统计 page.</summary>
public sealed class AssetSummary
{
    public long GoldTotal { get; set; }
    public long SilverTotal { get; set; }
    public long CharacterCount { get; set; }

    /// <summary>Percentage change vs. the previous snapshot, e.g. 3.2 for +3.2%.</summary>
    public double? GoldChangePercent { get; set; }
    public double? SilverChangePercent { get; set; }
    public long? CharacterChange { get; set; }
}

/// <summary>One bar group in the per-character currency comparison chart.</summary>
public sealed class CharacterAssetBar
{
    public string CharacterId { get; set; } = string.Empty;
    public string Nickname { get; set; } = string.Empty;

    /// <summary>Values are in 万元 (10k) to match the chart's unit label.</summary>
    public double GoldWan { get; set; }
    public double SilverWan { get; set; }
}

/// <summary>Everything the assets dashboard needs in one call.</summary>
public sealed class StatisticsPayload
{
    public AssetSummary Summary { get; set; } = new();
    public List<CharacterAssetBar> Characters { get; set; } = new();
    public List<AssetPoint> Recent { get; set; } = new();
}

/// <summary>User-facing settings, persisted alongside the database.</summary>
public sealed class Settings
{
    /// <summary>Show editable fields before saving a recognition.</summary>
    public bool EditableBeforeSave { get; set; } = true;

    /// <summary>Jump to 角色管理 automatically after a successful save.</summary>
    public bool AutoNavigateAfterSave { get; set; }

    /// <summary>Directory holding the database and exported blobs.</summary>
    public string DataDir { get; set; } = SettingsDefaults.DataDir();

    /// <summary>
    /// OCR performance profile: "low" (2 threads), "balanced" (4), "high"
    /// (physical cores).
    ///
    /// The backend maps this onto ONNX Runtime's intra-op thread count and the
    /// recognition batch size, then rebuilds the engine in place — no restart.
    /// </summary>
    public string OcrProfile { get; set; } = "balanced";
}

public static class SettingsDefaults
{
    public static string DataDir()
    {
        var baseDir = Environment.GetEnvironmentVariable("LOCALAPPDATA") ?? ".";
        return System.IO.Path.Combine(baseDir, "MhRecorder");
    }
}

/// <summary>Result of an export call.</summary>
public sealed class ExportResult
{
    public string Path { get; set; } = string.Empty;
    public long Bytes { get; set; }
}

/// <summary>Payload of the `ping` method.</summary>
public sealed class PingResult
{
    public string Status { get; set; } = string.Empty;
    public string Version { get; set; } = string.Empty;
    public int Protocol { get; set; }
}

/// <summary>Payload of the `version` method.</summary>
public sealed class VersionInfo
{
    public string Backend { get; set; } = string.Empty;
    public int Protocol { get; set; }
    public string Models { get; set; } = string.Empty;
}
