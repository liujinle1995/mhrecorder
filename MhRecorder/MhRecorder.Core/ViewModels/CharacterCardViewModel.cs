using MhRecorder.Core.Models;

namespace MhRecorder.Core.ViewModels;

/// <summary>Formatting helpers shared by every page so numbers look identical everywhere.</summary>
public static class Fmt
{
    public static string Number(long value) => value.ToString("N0");

    public static string Number(double value) => value.ToString("N0");

    public static string Signed(long value) =>
        value == 0 ? "0" : (value > 0 ? "+" + value.ToString("N0") : value.ToString("N0"));

    public static string Percent(double value) =>
        (value >= 0 ? "+" : "") + value.ToString("0.0") + "%";

    public static string LongDate(long epochMs)
    {
        if (epochMs <= 0) return "—";
        var dt = DateTimeOffset.FromUnixTimeMilliseconds(epochMs).ToLocalTime();
        return dt.ToString("yyyy/MM/dd HH:mm");
    }

    /// <summary>"今天 14:32" style stamp used in the footer and page subtitles.</summary>
    public static string FriendlyStamp(long epochMs)
    {
        if (epochMs <= 0) return "今天 14:32";
        var dt = DateTimeOffset.FromUnixTimeMilliseconds(epochMs).ToLocalTime();
        var today = DateTimeOffset.Now.Date;
        var prefix = dt.Date == today ? "今天" : dt.ToString("MM/dd");
        return $"{prefix} {dt:HH:mm}";
    }

    public static string AvatarInitial(string? nickname)
    {
        if (string.IsNullOrWhiteSpace(nickname)) return "角";
        var trimmed = nickname.Trim();
        // Skip any surrogate pair so a single grapheme stays intact.
        if (trimmed.Length >= 2 && char.IsSurrogatePair(trimmed[0], trimmed[1])) return trimmed[..2];
        return trimmed[..1];
    }

    public static string LevelText(long? level) => level is { } v ? $"{v}级" : "—";

    public static string IdLine(long? level, string? gameId)
    {
        var parts = new List<string>();
        if (level is { } l) parts.Add($"Lv.{l}");
        if (!string.IsNullOrWhiteSpace(gameId)) parts.Add($"游戏 ID {gameId}");
        return string.Join(" · ", parts);
    }
}

/// <summary>One character card on the 角色管理 page (page_02).</summary>
public sealed class CharacterCardViewModel : ObservableObject
{
    public CharacterCardViewModel(CharacterDetail detail)
    {
        Id = detail.Character.Id;
        Nickname = detail.Character.Nickname;
        Level = detail.Character.Level;
        GameId = detail.Character.GameId;
        AvatarInitial = Fmt.AvatarInitial(detail.Character.Nickname);
        // The backend stores the cropped portrait (base64 PNG) in the
        // avatarPath column, so the card can show the real character art.
        AvatarPngBase64 = detail.Character.AvatarPath;
        LevelText = Fmt.LevelText(detail.Character.Level);

        var stats = detail.Character.LatestStats;
        HpValue = stats.Hp?.Value ?? 0;
        MpValue = stats.Mp?.Value ?? 0;
        HpText = $"{Fmt.Number(HpValue)} / {Fmt.Number(HpValue)}";
        MpText = $"{Fmt.Number(MpValue)} / {Fmt.Number(MpValue)}";

        PhysicalDamage = Fmt.Number(stats.PhysicalDamage?.Value ?? 0);
        MagicDamage = Fmt.Number(stats.MagicDamage?.Value ?? 0);
        PhysicalDefense = Fmt.Number(stats.PhysicalDefense?.Value ?? 0);
        MagicDefense = Fmt.Number(stats.MagicDefense?.Value ?? 0);
        Speed = Fmt.Number(stats.Speed?.Value ?? 0);
        HealingPower = Fmt.Number(stats.HealingPower?.Value ?? 0);

        var currencies = detail.Character.LatestCurrencies;
        GoldValue = currencies.Gold?.Value ?? 0;
        SilverValue = currencies.Silver?.Value ?? 0;
        GoldText = Fmt.Number(GoldValue);
        SilverText = Fmt.Number(SilverValue);

        var deltas = Services.CharacterDeltas.For(detail.Character.Id);
        GoldDeltaText = Fmt.Signed(deltas.Gold);
        SilverDeltaText = Fmt.Signed(deltas.Silver);
        HasGoldDelta = deltas.Gold != 0;
        HasSilverDelta = deltas.Silver != 0;

        GameIdText = string.IsNullOrWhiteSpace(GameId) ? "ID 待确认" : $"ID {GameId}";
        SnapshotCount = detail.SnapshotCount;
        LastRecorded = detail.LastRecordedAt;
    }

    public string Id { get; }
    public string Nickname { get; }
    public long? Level { get; }
    public string? GameId { get; }
    public string AvatarInitial { get; }
    /// <summary>Base64 PNG of the portrait cropped from the screenshot, if any.</summary>
    public string? AvatarPngBase64 { get; }
    public string LevelText { get; }
    public string LevelBadge => LevelText;
    public string GameIdText { get; }
    public string IdLineText => SnapshotCount > 0 ? $"{GameIdText} · {SnapshotCount} 次记录" : GameIdText;
    public int SnapshotCount { get; }
    public long LastRecorded { get; }

    public long HpValue { get; }
    public long MpValue { get; }
    public string HpText { get; }
    public string MpText { get; }

    public string PhysicalDamage { get; }
    public string MagicDamage { get; }
    public string PhysicalDefense { get; }
    public string MagicDefense { get; }
    public string Speed { get; }
    public string HealingPower { get; }

    public long GoldValue { get; }
    public long SilverValue { get; }
    public string GoldText { get; }
    public string SilverText { get; }
    public string GoldDeltaText { get; }
    public string SilverDeltaText { get; }
    public bool HasGoldDelta { get; }
    public bool HasSilverDelta { get; }

    private bool _isCurrent;
    /// <summary>True for the raised card in the middle of the carousel.</summary>
    public bool IsCurrent
    {
        get => _isCurrent;
        set
        {
            if (!Set(ref _isCurrent, value)) return;
            Raise(nameof(AvatarIsCircle));
            Raise(nameof(CardScale));
        }
    }

    /// <summary>The focused card shows a round portrait tile; the neighbours show a square name tile.</summary>
    public bool AvatarIsCircle => IsCurrent;
    public double CardScale => IsCurrent ? 1.0 : 0.94;

    public bool Matches(string query)
    {
        if (string.IsNullOrWhiteSpace(query)) return true;
        var q = query.Trim();
        return Nickname.Contains(q, StringComparison.OrdinalIgnoreCase)
               || (GameId?.Contains(q, StringComparison.OrdinalIgnoreCase) ?? false);
    }
}
