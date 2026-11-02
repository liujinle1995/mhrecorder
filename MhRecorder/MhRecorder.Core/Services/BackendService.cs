// 后端连接与选择的唯一入口。
//
// 以前这段逻辑写死在 App 的静态类里（AppServices），页面直接 `AppServices.Instance`
// 拿 IAppService。问题有两个：ViewModel 无法脱离 App 构造，且"当前是实时后端还是
// 演示数据"这件事没有归属——每个页面自己判断一次。
//
// 现在只有 BackendService 知道这些，ViewModel 拿到的是一个 IAppService 接口。

using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.Core.Services;

/// <summary>
/// Owns the active <see cref="IAppService"/>: the live Rust backend when the pipe
/// answers, the in-memory demo service otherwise.
/// </summary>
public sealed class BackendService
{
    private IAppService _api;

    public BackendService(IAppService? initial = null)
    {
        _api = initial ?? Probe();
    }

    /// <summary>The service every repository/view-model talks to.</summary>
    public IAppService Api => _api;

    /// <summary>True when a live backend answered on the pipe.</summary>
    public bool IsRemote => _api.IsRemote;

    /// <summary>Text for the status bar: "已连接离线识别引擎" / "离线演示数据".</summary>
    public string StatusLabel => _api.IsRemote ? "已连接离线识别引擎" : "离线演示数据";

    /// <summary>Longer sentence for the footer.</summary>
    public string StatusDetail => _api.IsRemote
        ? "已离线运行 · 已连接本地识别引擎"
        : "已离线运行 · 使用内置演示数据";

    /// <summary>Raised whenever <see cref="Api"/> is replaced.</summary>
    public event EventHandler? Changed;

    /// <summary>Try to reach the real backend again; keeps the current one on failure.</summary>
    public bool TryReconnect(int connectTimeoutMs = 1200)
    {
        if (_api.IsRemote) return true;
        var remote = RemoteAppService.TryCreate(connectTimeoutMs);
        if (remote is null) return false;
        _api = remote;
        Changed?.Invoke(this, EventArgs.Empty);
        return true;
    }

    private static IAppService Probe()
    {
        // UI development switch: MH_FORCE_DEMO=1 keeps the review workspace
        // testable on a machine where the real backend is running (the demo
        // service synthesises one full result, boxes included).
        if (Environment.GetEnvironmentVariable("MH_FORCE_DEMO") == "1")
        {
            AppLog.Write("BackendService: MH_FORCE_DEMO=1, using demo data");
            return new FakeAppService();
        }

        try
        {
            var remote = RemoteAppService.TryCreate();
            if (remote is not null)
            {
                AppLog.Write("BackendService: connected to mhrecorder.v1 backend");
                return remote;
            }
        }
        catch (Exception ex)
        {
            AppLog.Write($"BackendService: remote connect failed: {ex.Message}");
        }

        AppLog.Write("BackendService: backend unavailable, using in-memory demo data");
        return new FakeAppService();
    }
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
