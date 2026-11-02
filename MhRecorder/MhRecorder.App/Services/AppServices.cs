using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.App.Services;

/// <summary>Process-wide service accessor: real pipe backend when available, fake otherwise.</summary>
public static class AppServices
{
    private static IAppService? _instance;
    private static bool _remote;

    public static IAppService Instance => _instance ??= Create();

    /// <summary>True when the live Rust backend answered on the pipe.</summary>
    public static bool IsRemote => _remote;

    /// <summary>Raised when the backend selection changes (startup or fallback swap).</summary>
    public static event EventHandler? Changed;

    public static void Initialize()
    {
        _instance = Create();
        Changed?.Invoke(null, EventArgs.Empty);
    }

    private static IAppService Create()
    {
        try
        {
            // Prefer the real backend; it is the whole point of the pipe protocol.
            var remote = RemoteAppService.TryCreate();
            if (remote is not null)
            {
                _remote = true;
                App.Log("AppServices: connected to mhrecorder.v1 backend");
                return remote;
            }
        }
        catch (Exception ex)
        {
            App.Log($"AppServices: remote connect failed: {ex.Message}");
        }

        _remote = false;
        App.Log("AppServices: backend unavailable, using in-memory fake");
        return new FakeAppService();
    }

    /// <summary>Last-known statistics payload, reused by the footer stamp.</summary>
    public static StatisticsPayload? LastStatistics { get; set; }
}
