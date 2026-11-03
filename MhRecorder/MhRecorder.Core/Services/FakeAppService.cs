// In-memory implementation of IAppService.
//
// The UI must be demo-able before the Rust backend is finished, so this fake
// carries the exact dataset the design mock shows (page_01..page_04) and mimics
// the latency/structure of the real pipe protocol.

using MhRecorder.Core.Models;

namespace MhRecorder.Core.Services;

/// <summary>Thread-safe in-memory back end used when the pipe is unavailable.</summary>
public sealed class FakeAppService : IAppService
{
    private readonly object _lock = new();

    /// <summary>Parked recognitions; empty in the demo backend unless a test seeds one.</summary>
    private readonly List<PendingRecognition> _pending = new();

    private List<FakeCharacter> _characters;
    private Settings _settings = new()
    {
        EditableBeforeSave = true,
        AutoNavigateAfterSave = false,
        DataDir = "~/Documents/梦幻记录/data.json",
    };

    private RecognitionResult? _lastResult;

    public FakeAppService()
    {
        _characters = BuildSeedCharacters();
    }

    public bool IsRemote => false;

    public string StatusLabel => "离线模式";

    private static long Now() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

    // ------------------------------------------------------------------
    // Recognition
    // ------------------------------------------------------------------

    public Task<RecognitionResult?> RecognizeAsync(IReadOnlyList<RecognizeImage> images, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();

        var result = BuildDesignRecognition(images.Count == 0 ? 2 : images.Count);
        lock (_lock)
        {
            _lastResult = result;
        }
        return Task.FromResult<RecognitionResult?>(result);
    }

    /// <summary>The exact recognition payload the design mock (page_01) displays.</summary>
    private static RecognitionResult BuildDesignRecognition(int screenshotCount)
    {
        var stats = new CombatStats
        {
            Hp = Attr(7721, 0.98, "7,721"),
            Mp = Attr(2215, 0.97, "2,215"),
            PhysicalDamage = Attr(758, 0.96, "758"),
            MagicDamage = Attr(582, 0.95, "582"),
            PhysicalDefense = Attr(1714, 0.96, "1,714"),
            MagicDefense = Attr(948, 0.95, "948"),
            Speed = Attr(892, 0.95, "892"),
            HealingPower = Attr(408, 0.93, "408"),
        };

        return new RecognitionResult
        {
            CharacterId = "c-haitaotao",
            Character = new RecognizedCharacter
            {
                Nickname = "海淘淘",
                GameId = "429277013",
                Level = 89,
            },
            Screens = new List<ScreenKind> { ScreenKind.CharacterAttributes, ScreenKind.Inventory },
            Stats = stats,
            Currencies = new Currencies
            {
                Gold = Attr(2942685, 0.97, "2,942,685"),
                Silver = Attr(14297169, 0.98, "14,297,169"),
            },
            Confidence = 0.96,
            IdentitySource = IdentitySource.IdField,
            ScreenshotCount = screenshotCount,
            CacheHits = new List<string>(),
            // 演示模式也要能看审核工作区：没有后端时这些框让"点字段→高亮区域"
            // 依然可用，否则审核页在离线演示下是一片空图。
            Regions = DemoRegions(),
            IdentityEvidence = "游戏ID 429277013 直接读取自人物属性面板",
            Appearance = new AppearanceSignature
            {
                HairName = "深蓝",
                HairRgb = new[] { 44, 62, 108 },
                WeaponName = "青碧",
                WeaponRgb = new[] { 90, 190, 170 },
            },
        };
    }

    /// <summary>Plausible boxes for a 1265x711 人物属性 capture.</summary>
    private static List<FieldRegion> DemoRegions() => new()
    {
        Region("nickname", "昵称", 399, 167, 90, 16, "海淘淘", null, FieldRegionKind.Identity),
        Region("level", "等级", 505, 167, 44, 14, "89级", 89, FieldRegionKind.Identity),
        Region("game_id", "游戏ID", 412, 194, 84, 13, "429277013", null, FieldRegionKind.Identity),
        Region("hp", "气血", 791, 173, 71, 13, "7,721/7,721", 7721, FieldRegionKind.Field),
        Region("mp", "魔法", 792, 211, 68, 13, "2,215/2,215", 2215, FieldRegionKind.Field),
        Region("physical_damage", "物伤", 650, 388, 104, 14, "物理伤害758", 758, FieldRegionKind.Field),
        Region("physical_defense", "物防", 794, 388, 111, 14, "物理防御1714", 1714, FieldRegionKind.Field),
        Region("magic_damage", "法伤", 649, 428, 72, 14, "法术伤害582", 582, FieldRegionKind.Field),
        Region("magic_defense", "法防", 794, 428, 111, 14, "法术防御948", 948, FieldRegionKind.Field),
        Region("speed", "速度", 716, 468, 48, 13, "892", 892, FieldRegionKind.Field),
        Region("healing_power", "治疗强度", 794, 468, 102, 14, "治疗强度408", 408, FieldRegionKind.Field),
        Region("gold", "金币", 700, 604, 82, 14, "2,942,685", 2942685, FieldRegionKind.Field),
        Region("silver", "银币", 900, 604, 92, 14, "14,297,169", 14297169, FieldRegionKind.Field),
        new FieldRegion
        {
            ScreenIndex = 0, Screen = ScreenKind.CharacterAttributes,
            Key = "feature:avatar", Label = "头像",
            X = 289, Y = 150, Width = 66, Height = 60,
            Confidence = 1.0, Kind = FieldRegionKind.Feature,
        },
    };

    private static FieldRegion Region(string key, string label, int x, int y, int w, int h,
        string raw, long? value, string kind) => new()
    {
        ScreenIndex = 0,
        Screen = ScreenKind.CharacterAttributes,
        Key = key,
        Label = label,
        X = x,
        Y = y,
        Width = w,
        Height = h,
        RawText = raw,
        Confidence = 0.96,
        Value = value,
        Kind = kind,
    };

    private static AttributeValue Attr(long value, double confidence, string raw) =>
        new() { Value = value, Confidence = confidence, RawText = raw };

    public Task<SaveRecognitionOutcome> SaveRecognitionAsync(RecognitionResult result, IReadOnlyList<string> screens, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();

        var id = result.CharacterId;
        lock (_lock)
        {
            var nickname = result.Character.Nickname ?? "未命名";
            var existing = _characters.FirstOrDefault(c => c.Id == id)
                           ?? _characters.FirstOrDefault(c => c.Nickname == nickname);

            if (existing is null)
            {
                existing = new FakeCharacter
                {
                    Id = "c-" + Guid.NewGuid().ToString("N")[..8],
                    Nickname = nickname,
                    Level = result.Character.Level,
                    GameId = result.Character.GameId,
                };
                _characters.Add(existing);
            }

            existing.Nickname = nickname;
            if (result.Character.Level is { } lvl) existing.Level = lvl;
            if (result.Character.GameId is { } gid) existing.GameId = gid;
            existing.Stats = result.Stats;
            existing.Currencies = result.Currencies;
            existing.GoldDelta = 0;
            existing.SilverDelta = 0;
            existing.SnapshotCount += 1;
            existing.LastRecordedAt = Now();
            existing.UpdatedAt = Now();

            id = existing.Id;
        }

        return Task.FromResult(new SaveRecognitionOutcome(id, false, null, null));
    }

    public Task<IReadOnlyList<PendingRecognition>> ListPendingAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            return Task.FromResult<IReadOnlyList<PendingRecognition>>(_pending.ToList());
        }
    }

    public Task AssignPendingAsync(string id, string characterId, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            _pending.RemoveAll(p => p.Id == id);
        }
        return Task.CompletedTask;
    }

    public Task DiscardPendingAsync(string id, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            _pending.RemoveAll(p => p.Id == id);
        }
        return Task.CompletedTask;
    }

    // ------------------------------------------------------------------
    // Characters
    // ------------------------------------------------------------------

    public Task<IReadOnlyList<CharacterDetail>> ListCharactersAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            var list = _characters.Select(ToDetail).ToList();
            return Task.FromResult<IReadOnlyList<CharacterDetail>>(list);
        }
    }

    public Task<CharacterDetail?> GetCharacterAsync(string id, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            var match = _characters.FirstOrDefault(c => c.Id == id);
            return Task.FromResult(match is null ? null : ToDetail(match));
        }
    }

    /// <summary>
    /// Demo engine descriptor, shaped exactly like the Rust backend's `version`
    /// payload so the Settings page renders identically with or without a
    /// live backend.
    /// </summary>
    public Task<System.Text.Json.JsonElement?> GetEngineInfoAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        var payload = System.Text.Json.JsonDocument.Parse(
            """
            {
              "backend": "0.9.2-demo",
              "protocol": 1,
              "models": "ppocrv4",
              "mode": "cpu",
              "engine": {
                "id": "ppocrv4-onnx",
                "name": "PP-OCRv4 (ONNX Runtime)",
                "version": "4.x",
                "kind": "ocr",
                "runtime": "onnxruntime-cpu",
                "models": ["ch_PP-OCRv4_det_infer.onnx", "ch_PP-OCRv4_rec_infer.onnx"],
                "active": true,
                "detail": "演示数据",
                "updatable": false
              }
            }
            """).RootElement.Clone();
        return Task.FromResult<System.Text.Json.JsonElement?>(payload);
    }

    public Task DeleteCharacterAsync(string id, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            _characters.RemoveAll(c => c.Id == id);
        }
        return Task.CompletedTask;
    }

    public Task UpdateCharacterIdentityAsync(string id, string? nickname, string? gameId, long? level, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            var match = _characters.FirstOrDefault(c => c.Id == id);
            if (match is null) return Task.CompletedTask;
            if (nickname is not null) match.Nickname = nickname;
            if (gameId is not null) match.GameId = gameId;
            if (level is not null) match.Level = level;
            match.IdentityConfirmed = true;
            match.IdentityConfidence = 1.0;
            match.UpdatedAt = Now();
        }
        return Task.CompletedTask;
    }

    public Task ConfirmCharacterAsync(string id, double confidence, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            var match = _characters.FirstOrDefault(c => c.Id == id);
            if (match is null) return Task.CompletedTask;
            match.IdentityConfirmed = true;
            match.IdentityConfidence = Math.Clamp(confidence, 0.0, 1.0);
            match.UpdatedAt = Now();
        }
        return Task.CompletedTask;
    }

    private static CharacterDetail ToDetail(FakeCharacter source) => new()
    {
        Character = new Character
        {
            Id = source.Id,
            Nickname = source.Nickname,
            GameId = source.GameId,
            Level = source.Level,
            IdentityConfidence = source.IdentityConfidence,
            IdentityConfirmed = source.IdentityConfirmed,
            CreatedAt = source.CreatedAt,
            UpdatedAt = source.UpdatedAt,
            LatestStats = source.Stats,
            LatestCurrencies = source.Currencies,
        },
        SnapshotCount = source.SnapshotCount,
        LastRecordedAt = source.LastRecordedAt,
    };

    // ------------------------------------------------------------------
    // Statistics (page_03 exact numbers)
    // ------------------------------------------------------------------

    public Task<StatisticsPayload> GetStatisticsAsync(int recentLimit = 5, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();

        var payload = new StatisticsPayload
        {
            Summary = new AssetSummary
            {
                GoldTotal = 2942685,
                SilverTotal = 14297169,
                CharacterCount = 5,
                GoldChangePercent = 3.2,
                SilverChangePercent = 1.8,
                CharacterChange = 1,
            },
            Characters = new List<CharacterAssetBar>
            {
                new() { CharacterId = "c-haitaotao", Nickname = "海淘淘", GoldWan = 294, SilverWan = 1430 },
                new() { CharacterId = "c-yunzhonghe", Nickname = "云中鹤", GoldWan = 186, SilverWan = 968 },
                new() { CharacterId = "c-xiaolingdang", Nickname = "小铃铛", GoldWan = 93, SilverWan = 512 },
                new() { CharacterId = "c-mingyuexin", Nickname = "明月心", GoldWan = 240, SilverWan = 1180 },
                new() { CharacterId = "c-tieshou", Nickname = "铁手", GoldWan = 129, SilverWan = 705 },
            },
            Recent = new List<AssetPoint>
            {
                new() { Date = "10/28", Gold = 1080, Silver = 5200 },
                new() { Date = "10/29", Gold = 1120, Silver = 5380 },
                new() { Date = "10/30", Gold = 1096, Silver = 5260 },
                new() { Date = "10/31", Gold = 1180, Silver = 5620 },
                new() { Date = "11/01", Gold = 1245, Silver = 5910 },
            },
        };

        var take = Math.Max(1, recentLimit);
        payload.Recent = payload.Recent.TakeLast(take).ToList();
        return Task.FromResult(payload);
    }

    // ------------------------------------------------------------------
    // Settings
    // ------------------------------------------------------------------

    public Task<Settings> GetSettingsAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            return Task.FromResult(CloneSettings(_settings));
        }
    }

    public Task SetSettingsAsync(Settings settings, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            _settings = CloneSettings(settings);
        }
        return Task.CompletedTask;
    }

    private static Settings CloneSettings(Settings source) => new()
    {
        EditableBeforeSave = source.EditableBeforeSave,
        AutoNavigateAfterSave = source.AutoNavigateAfterSave,
        DataDir = source.DataDir,
    };

    // ------------------------------------------------------------------
    // Export / clear
    // ------------------------------------------------------------------

    public Task<ExportResult?> ExportAsync(ExportFormat format, string path, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();

        Dictionary<string, object?> document;
        lock (_lock)
        {
            document = new Dictionary<string, object?>
            {
                ["version"] = ProtocolConstants.AppVersion,
                ["exportedAt"] = Now(),
                ["settings"] = _settings,
                ["characters"] = _characters.Select(ToDetail).ToList(),
            };
        }

        var bytes = System.Text.Json.JsonSerializer.SerializeToUtf8Bytes(document, Json.Options);
        var dir = System.IO.Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir)) System.IO.Directory.CreateDirectory(dir);
        System.IO.File.WriteAllBytes(path, bytes);

        return Task.FromResult<ExportResult?>(new ExportResult { Path = path, Bytes = bytes.Length });
    }

    public Task ClearDataAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        lock (_lock)
        {
            _characters.Clear();
            _lastResult = null;
        }
        return Task.CompletedTask;
    }

    // ------------------------------------------------------------------
    // Seed data lifted from the design mock
    // ------------------------------------------------------------------

    private sealed class FakeCharacter
    {
        public string Id = string.Empty;
        public string Nickname = string.Empty;
        public string? GameId;
        public long? Level;
        public double IdentityConfidence = 0.96;
        public bool IdentityConfirmed = true;
        public long CreatedAt;
        public long UpdatedAt;
        public int SnapshotCount = 1;
        public long LastRecordedAt;
        public CombatStats Stats = new();
        public Currencies Currencies = new();
        public long GoldDelta;
        public long SilverDelta;
    }

    private static CombatStats DesignStats(
        long hp, long mp, long pdmg, long mdmg, long pdef, long mdef, long speed, long heal) => new()
    {
        Hp = Attr(hp, 0.98, hp.ToString("N0")),
        Mp = Attr(mp, 0.97, mp.ToString("N0")),
        PhysicalDamage = Attr(pdmg, 0.96, pdmg.ToString("N0")),
        MagicDamage = Attr(mdmg, 0.95, mdmg.ToString("N0")),
        PhysicalDefense = Attr(pdef, 0.96, pdef.ToString("N0")),
        MagicDefense = Attr(mdef, 0.95, mdef.ToString("N0")),
        Speed = Attr(speed, 0.95, speed.ToString("N0")),
        HealingPower = Attr(heal, 0.93, heal.ToString("N0")),
    };

    internal static List<CharacterDetail> DesignCharacterDetails() => BuildSeedCharacters().Select(ToDetail).ToList();

    private static List<FakeCharacter> BuildSeedCharacters()
    {
        var now = Now();
        var oneHour = 3600_000L;

        return new List<FakeCharacter>
        {
            new()
            {
                Id = "c-haitaotao", Nickname = "海淘淘", Level = 89, GameId = "429277013",
                CreatedAt = now - oneHour * 24, UpdatedAt = now - oneHour,
                SnapshotCount = 3, LastRecordedAt = now,
                Stats = DesignStats(7721, 2215, 758, 582, 1714, 948, 892, 408),
                Currencies = new Currencies { Gold = Attr(2942685, 0.97, "2,942,685"), Silver = Attr(14297169, 0.98, "14,297,169") },
                GoldDelta = -120594, SilverDelta = 1297169,
            },
            new()
            {
                Id = "c-yunzhonghe", Nickname = "云中鹤", Level = 76, GameId = "412870335",
                CreatedAt = now - oneHour * 40, UpdatedAt = now - oneHour * 3,
                SnapshotCount = 2, LastRecordedAt = now - oneHour,
                Stats = DesignStats(7721, 2215, 758, 582, 1714, 948, 892, 408),
                Currencies = new Currencies { Gold = Attr(2942685, 0.97, "2,942,685"), Silver = Attr(14297169, 0.98, "14,297,169") },
                GoldDelta = -120594, SilverDelta = 1297169,
            },
            new()
            {
                Id = "c-xiaolingdang", Nickname = "小铃铛", Level = 64, GameId = "518204467",
                CreatedAt = now - oneHour * 60, UpdatedAt = now - oneHour * 5,
                SnapshotCount = 2, LastRecordedAt = now - oneHour * 2,
                Stats = DesignStats(4980, 1420, 438, 512, 1120, 680, 612, 286),
                Currencies = new Currencies { Gold = Attr(930000, 0.96, "930,000"), Silver = Attr(5120000, 0.97, "5,120,000") },
                GoldDelta = -120594, SilverDelta = 1297169,
            },
            new()
            {
                Id = "c-mingyuexin", Nickname = "明月心", Level = 82, GameId = "336105288",
                CreatedAt = now - oneHour * 70, UpdatedAt = now - oneHour * 6,
                SnapshotCount = 2, LastRecordedAt = now - oneHour * 3,
                Stats = DesignStats(6400, 1980, 612, 704, 1380, 812, 740, 352),
                Currencies = new Currencies { Gold = Attr(2400000, 0.96, "2,400,000"), Silver = Attr(11800000, 0.97, "11,800,000") },
                GoldDelta = -120594, SilverDelta = 1297169,
            },
            new()
            {
                Id = "c-tieshou", Nickname = "铁手", Level = 71, GameId = "275880146",
                CreatedAt = now - oneHour * 90, UpdatedAt = now - oneHour * 8,
                SnapshotCount = 1, LastRecordedAt = now - oneHour * 5,
                Stats = DesignStats(5200, 1600, 690, 402, 1590, 590, 520, 230),
                Currencies = new Currencies { Gold = Attr(1290000, 0.95, "1,290,000"), Silver = Attr(7050000, 0.96, "7,050,000") },
                GoldDelta = -120594, SilverDelta = 1297169,
            },
        };
    }
}

/// <summary>Deltas for the little +/- pills on the character cards (page_02).</summary>
public static class CharacterDeltas
{
    private static readonly Dictionary<string, (long gold, long silver)> Map = new()
    {
        ["c-haitaotao"] = (-120594, 1297169),
        ["c-yunzhonghe"] = (-120594, 1297169),
        ["c-xiaolingdang"] = (-120594, 1297169),
        ["c-mingyuexin"] = (-120594, 1297169),
        ["c-tieshou"] = (-120594, 1297169),
    };

    public static (long Gold, long Silver) For(string characterId) =>
        Map.TryGetValue(characterId, out var v) ? v : (0, 0);
}
