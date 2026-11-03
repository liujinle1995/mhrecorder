// 后端连接与选择的唯一入口。
//
// 以前这段逻辑写死在 App 的静态类里（AppServices），页面直接 `AppServices.Instance`
// 拿 IAppService。问题有两个：ViewModel 无法脱离 App 构造，且"当前是实时后端还是
// 演示数据"这件事没有归属——每个页面自己判断一次。
//
// 现在只有 BackendService 知道这些，ViewModel 拿到的是一个 IAppService 接口。
//
// **没有后端时不再悄悄换成演示数据。** 演示数据曾经是"连不上后端"的兜底，结果
// 是：用户以为自己在记录真实数据，实际写进内存，关掉就没了。现在分三种状态：
//   * Ready       连上本地识别引擎（唯一的正常状态）；
//   * Demo        调试构建专用，界面上有常驻警告；
//   * Unavailable 正式构建连不上后端——识别/保存/导出全部不可用，UI 明确报错。

using MhRecorder.Core.Models;

namespace MhRecorder.Core.Services;

/// <summary>How the app is talking to its data.</summary>
public enum BackendState
{
    /// <summary>A live backend answered on the pipe.</summary>
    Ready,

    /// <summary>Debug build without a backend: in-memory fake data, nothing persists.</summary>
    Demo,

    /// <summary>No backend and no demo fallback: every data operation will fail.</summary>
    Unavailable,
}

/// <summary>
/// Owns the active <see cref="IAppService"/> and tells the truth about it.
/// </summary>
public sealed class BackendService
{
    private IAppService _api;

    public BackendService(IAppService? initial = null)
    {
        if (initial is not null)
        {
            _api = initial;
            State = initial.IsRemote ? BackendState.Ready : BackendState.Demo;
            return;
        }

        var probed = Probe();
        _api = probed.Api;
        State = probed.State;
        UnavailableReason = probed.Reason;
    }

    /// <summary>The service every repository/view-model talks to.</summary>
    public IAppService Api => _api;

    public BackendState State { get; private set; }

    /// <summary>True when a live backend answered on the pipe.</summary>
    public bool IsRemote => State == BackendState.Ready;

    /// <summary>True when the in-memory demo service is standing in (debug only).</summary>
    public bool IsDemo => State == BackendState.Demo;

    /// <summary>False when nothing can read or write data.</summary>
    public bool IsAvailable => State != BackendState.Unavailable;

    /// <summary>Why the backend is unavailable, for the banner and the log.</summary>
    public string? UnavailableReason { get; private set; }

    /// <summary>Text for the status bar.</summary>
    public string StatusLabel => State switch
    {
        BackendState.Ready => "已连接离线识别引擎",
        BackendState.Demo => "演示数据（未连接后端）",
        _ => "未连接后端",
    };

    /// <summary>Longer sentence for the footer.</summary>
    public string StatusDetail => State switch
    {
        BackendState.Ready => "已离线运行 · 已连接本地识别引擎",
        BackendState.Demo => "已离线运行 · 演示数据不会保存",
        _ => "已离线运行 · 未连接本地识别引擎",
    };

    /// <summary>Raised whenever <see cref="Api"/> or <see cref="State"/> changes.</summary>
    public event EventHandler? Changed;

    /// <summary>Try to reach the real backend again; keeps the current one on failure.</summary>
    public bool TryReconnect(int connectTimeoutMs = 1200)
    {
        if (State == BackendState.Ready) return true;

        RemoteAppService? remote;
        try
        {
            remote = RemoteAppService.TryCreate(connectTimeoutMs);
        }
        catch (Exception ex)
        {
            AppLog.Write($"BackendService: reconnect failed: {ex.Message}");
            remote = null;
        }

        if (remote is null)
        {
            AppLog.Write("BackendService: reconnect found no backend");
            return false;
        }

        _api = remote;
        State = BackendState.Ready;
        UnavailableReason = null;
        Changed?.Invoke(this, EventArgs.Empty);
        return true;
    }

    private static (IAppService Api, BackendState State, string? Reason) Probe()
    {
        // 调试构建的 UI 开发开关：MH_FORCE_DEMO=1 在看板/布局调整时顶替真后端。
        // 正式构建**忽略**它——否则一个环境变量就能让发布版"看起来在记录"。
#if DEBUG
        if (Environment.GetEnvironmentVariable("MH_FORCE_DEMO") == "1")
        {
            AppLog.Write("BackendService: MH_FORCE_DEMO=1, using demo data (debug build)");
            return (new FakeAppService(), BackendState.Demo, null);
        }
#else
        if (Environment.GetEnvironmentVariable("MH_FORCE_DEMO") == "1")
        {
            AppLog.Write("BackendService: MH_FORCE_DEMO ignored in a release build");
        }
#endif

        try
        {
            var remote = RemoteAppService.TryCreate();
            if (remote is not null)
            {
                AppLog.Write("BackendService: connected to mhrecorder.v1 backend");
                return (remote, BackendState.Ready, null);
            }
        }
        catch (Exception ex)
        {
            AppLog.Write($"BackendService: remote connect failed: {ex.Message}");
        }

        const string reason =
            "没有连上本地识别引擎（mhservice.exe 的命名管道 mhrecorder.v1 无响应）。"
            + "识别、保存、导出都不可用。请关闭本程序后重新双击启动器；若仍失败，"
            + "请查看 %LOCALAPPDATA%\\MhRecorder\\mhservice.log。";

#if DEBUG
        AppLog.Write("BackendService: no backend, falling back to demo data (debug build)");
        return (new FakeAppService(), BackendState.Demo, reason);
#else
        AppLog.Write("BackendService: backend unavailable; demo fallback is disabled in release");
        return (new UnavailableAppService(reason), BackendState.Unavailable, reason);
#endif
    }
}

/// <summary>Every operation fails with the same explanation, instead of pretending.</summary>
public sealed class BackendUnavailableException : Exception
{
    public BackendUnavailableException(string message) : base(message)
    {
    }
}

/// <summary>
/// Stand-in used when no backend answered in a release build.
///
/// Returning empty data would look exactly like "a database with nothing in it",
/// so every call throws: the pages then show a real error, and the shell keeps a
/// banner up saying the app cannot record anything.
/// </summary>
public sealed class UnavailableAppService : IAppService
{
    public UnavailableAppService(string reason) => Reason = reason;

    public string Reason { get; }

    public bool IsRemote => false;

    public string StatusLabel => "未连接后端";

    private BackendUnavailableException Fail() => new(Reason);

    public Task<RecognitionResult?> RecognizeAsync(IReadOnlyList<RecognizeImage> images, CancellationToken ct = default)
        => Task.FromException<RecognitionResult?>(Fail());

    public Task<SaveRecognitionOutcome> SaveRecognitionAsync(RecognitionResult result, IReadOnlyList<string> screens, CancellationToken ct = default)
        => Task.FromException<SaveRecognitionOutcome>(Fail());

    public Task<IReadOnlyList<PendingRecognition>> ListPendingAsync(CancellationToken ct = default)
        => Task.FromException<IReadOnlyList<PendingRecognition>>(Fail());

    public Task AssignPendingAsync(string id, string characterId, CancellationToken ct = default)
        => Task.FromException(Fail());

    public Task DiscardPendingAsync(string id, CancellationToken ct = default)
        => Task.FromException(Fail());

    public Task<IReadOnlyList<CharacterDetail>> ListCharactersAsync(CancellationToken ct = default)
        => Task.FromException<IReadOnlyList<CharacterDetail>>(Fail());

    public Task<StatisticsPayload> GetStatisticsAsync(int recentLimit = 5, CancellationToken ct = default)
        => Task.FromException<StatisticsPayload>(Fail());

    public Task<Settings> GetSettingsAsync(CancellationToken ct = default)
        => Task.FromException<Settings>(Fail());

    public Task SetSettingsAsync(Settings settings, CancellationToken ct = default)
        => Task.FromException(Fail());

    public Task<ExportResult?> ExportAsync(ExportFormat format, string path, CancellationToken ct = default)
        => Task.FromException<ExportResult?>(Fail());

    public Task ClearDataAsync(CancellationToken ct = default)
        => Task.FromException(Fail());

    public Task DeleteCharacterAsync(string id, CancellationToken ct = default)
        => Task.FromException(Fail());

    public Task UpdateCharacterIdentityAsync(string id, string? nickname, string? gameId, long? level, CancellationToken ct = default)
        => Task.FromException(Fail());

    public Task ConfirmCharacterAsync(string id, double confidence, CancellationToken ct = default)
        => Task.FromException(Fail());

    public Task<CharacterDetail?> GetCharacterAsync(string id, CancellationToken ct = default)
        => Task.FromException<CharacterDetail?>(Fail());

    public Task<System.Text.Json.JsonElement?> GetEngineInfoAsync(CancellationToken ct = default)
        => Task.FromException<System.Text.Json.JsonElement?>(Fail());
}

/// <summary>
/// Logging seam. The app points this at its file logger at startup; the Core
/// project must not know where the log lives.
/// </summary>
public static class AppLog
{
    private static Action<string>? _sink;

    public static void Attach(Action<string> sink) => _sink = sink;

    public static void Write(string message)
    {
        try
        {
            _sink?.Invoke(message);
        }
        catch
        {
            // logging must never break a feature
        }
    }
}
