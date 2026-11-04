// 后端的连接与关停 —— **只管连和断，不管生死**。
//
// 0.15 的版本是"谁拉起来谁负责杀"：
//   * 用 `Process.GetProcessesByName("mhservice")` 判断后端在不在 → 会把别的目录、
//     别的用户会话、另一份便携版的同名进程都算进来；
//   * 前端退出时 `Kill()` 自己拉起来的那个 → 两个窗口共用一个后端时，先关的那
//     个会把后关的那个的连接一起打断。
//
// 现在生命周期归后端自己（见 mhservice 的实例锁 + 空闲退出 + system.shutdown）：
//   * 后端按**数据目录**加独占锁，同一目录只可能有一个实例；
//   * 连上以后用 `version` 返回的 dataDir / pid 校验"这是不是我该连的后端"；
//   * 关窗口时发 `system.shutdown`，后端自己判断还有没有别的客户端；
//   * 前端就算被强杀，后端数到"零连接 + 空闲超时"也会自己退出。
//
// 这样以后加托盘、后台常驻、多窗口，都不需要再动这里。

using System.Diagnostics;
using System.Text.Json;

namespace MhRecorder.Core.Services;

/// <summary>Starts the Rust backend next to the app when nobody else did, and asks it to stop on exit.</summary>
public static class BackendLauncher
{
    private static readonly object Gate = new();
    private static Process? _spawned;

    /// <summary>Why the last start attempt failed, for the banner text.</summary>
    public static string? LastError { get; private set; }

    /// <summary>Where the backend executable is expected to live.</summary>
    public static string ServicePath =>
        Environment.GetEnvironmentVariable("MH_SERVICE_PATH")
        ?? Path.Combine(AppContext.BaseDirectory, "mhservice.exe");

    /// <summary>
    /// 本进程期望的数据目录。必须和后端 `mhcore::default_data_dir()` 算的一致
    /// （`MH_DATA_DIR` 优先，否则 `%LOCALAPPDATA%\MhRecorder`），否则归属校验会
    /// 把正确的后端判成"别人的"。
    /// </summary>
    public static string ExpectedDataDir()
    {
        var configured = Environment.GetEnvironmentVariable("MH_DATA_DIR");
        var raw = string.IsNullOrWhiteSpace(configured)
            ? Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "MhRecorder")
            : configured;
        try
        {
            return Path.GetFullPath(raw).TrimEnd('\\', '/');
        }
        catch
        {
            return raw.TrimEnd('\\', '/');
        }
    }

    /// <summary>
    /// Connect to the pipe, starting the backend first if it is not running.
    /// Returns null when no backend could be reached within <paramref name="waitMs"/>.
    /// </summary>
    public static RemoteAppService? ConnectOrStart(int waitMs = 8000)
    {
        // 1. 已经在跑（启动器提前拉起、另一个窗口拉起的、上次没退干净的）。
        var probe = TryConnectVerified(1200);
        if (probe.Api is not null) return probe.Api;
        if (probe.Foreign is not null)
        {
            // 管道上有后端，但它服务的是别的数据目录。再拉起一个只会让两个
            // 后端抢同一个管道名，客户端连到哪个全看运气——宁可明确拒绝。
            LastError = probe.Foreign;
            AppLog.Write($"BackendLauncher: {LastError}");
            return null;
        }

        var exe = ServicePath;
        if (!File.Exists(exe))
        {
            LastError = $"找不到后端程序：{exe}";
            AppLog.Write($"BackendLauncher: {LastError}");
            return null;
        }

        lock (Gate)
        {
            if (!Spawn(exe)) return null;

            // 2. 轮询到管道可用。单次连接超时给短一点，才能多试几轮。
            var deadline = Environment.TickCount64 + waitMs;
            while (Environment.TickCount64 < deadline)
            {
                var attempt = TryConnectVerified(600);
                if (attempt.Api is not null)
                {
                    LastError = null;
                    AppLog.Write("BackendLauncher: backend is up");
                    return attempt.Api;
                }
                if (attempt.Foreign is not null)
                {
                    LastError = attempt.Foreign;
                    AppLog.Write($"BackendLauncher: {LastError}");
                    return null;
                }

                Thread.Sleep(200);
            }

            LastError = "本地识别引擎启动超时（mhservice.exe 已拉起但没在 8 秒内就绪）。";
            AppLog.Write($"BackendLauncher: {LastError}");
            return null;
        }
    }

    /// <summary>
    /// 请后端退出。后端只有在"没有别的客户端"时才照做，所以两个窗口共用后端时
    /// 关掉其中一个不会影响另一个。
    /// </summary>
    /// <returns>true 表示后端接受了关停（或本来就没在跑）。</returns>
    public static bool RequestShutdown(int timeoutMs = 1500)
    {
        var api = RemoteAppService.TryCreate(timeoutMs);
        if (api is null)
        {
            AppLog.Write("BackendLauncher: 关停时后端已不在，无需处理");
            return true;
        }

        try
        {
            var result = api.Client.ShutdownAsync().GetAwaiter().GetResult();
            var accepted = result is { } r
                           && r.TryGetProperty("shuttingDown", out var flag)
                           && flag.ValueKind == JsonValueKind.True;
            if (accepted)
            {
                AppLog.Write("BackendLauncher: 后端接受关停");
            }
            else
            {
                var clients = result is { } x && x.TryGetProperty("clients", out var c)
                    ? c.GetRawText()
                    : "?";
                AppLog.Write($"BackendLauncher: 后端拒绝关停（还有 {clients} 个客户端），保留");
            }
            return accepted;
        }
        catch (Exception ex)
        {
            // 后端可能正好在退出；这不是错误。
            AppLog.Write($"BackendLauncher: shutdown 请求失败：{ex.Message}");
            return true;
        }
        finally
        {
            api.Client.Dispose();
        }
    }

    /// <summary>Kill the spawned process — 只在"启动失败要收拾现场"时用，正常退出不用它。</summary>
    public static void KillSpawned()
    {
        lock (Gate)
        {
            if (_spawned is null) return;
            try
            {
                if (!_spawned.HasExited)
                {
                    _spawned.Kill(entireProcessTree: true);
                    _spawned.WaitForExit(3000);
                    AppLog.Write("BackendLauncher: killed a backend that never became ready");
                }
            }
            catch (Exception ex)
            {
                AppLog.Write($"BackendLauncher: kill failed: {ex.Message}");
            }
            finally
            {
                _spawned.Dispose();
                _spawned = null;
            }
        }
    }

    /// <summary>连接尝试的结果：要么拿到可用后端，要么知道"管道被别的数据目录占了"。</summary>
    private readonly record struct ProbeResult(RemoteAppService? Api, string? Foreign);

    private static ProbeResult TryConnectVerified(int timeoutMs)
    {
        var api = RemoteAppService.TryCreate(timeoutMs);
        if (api is null) return new ProbeResult(null, null);

        string? reject = null;
        try
        {
            var version = api.Client.GetEngineInfoAsync().GetAwaiter().GetResult();

            var protocol = version is { } v && v.TryGetProperty("protocol", out var p)
                ? p.GetInt32()
                : 0;
            var dataDir = version is { } d && d.TryGetProperty("dataDir", out var dd)
                ? dd.GetString()
                : null;
            var pid = version is { } i && i.TryGetProperty("pid", out var pp) ? pp.GetInt32() : 0;

            if (protocol != ProtocolConstants.Version)
            {
                reject = $"后端协议版本 v{protocol} 与本程序 v{ProtocolConstants.Version} 不一致。"
                         + "请确认安装目录里的 mhservice.exe 与界面是同一个版本（升级时先把旧进程关掉）。";
            }
            else if (!DataDirMatches(dataDir))
            {
                reject = $"管道上的后端（pid {pid}）服务的数据目录是 {dataDir}，"
                         + $"不是本程序要用的 {ExpectedDataDir()}。"
                         + "请关闭另一份 MhRecorder（或检查 MH_DATA_DIR 环境变量）后重试。";
            }
            else
            {
                AppLog.Write($"BackendLauncher: connected pid={pid} dataDir={dataDir}");
            }
        }
        catch (Exception ex)
        {
            // version 都问不出来，说明这条连接不能用；按"没连上"处理。
            AppLog.Write($"BackendLauncher: version probe failed: {ex.Message}");
            api.Client.Dispose();
            return new ProbeResult(null, null);
        }

        if (reject is not null)
        {
            api.Client.Dispose();
            return new ProbeResult(null, reject);
        }
        return new ProbeResult(api, null);
    }

    private static bool DataDirMatches(string? reported)
    {
        if (string.IsNullOrWhiteSpace(reported)) return true; // 老后端不报，放行
        try
        {
            var a = Path.GetFullPath(reported).TrimEnd('\\', '/');
            var b = ExpectedDataDir();
            return string.Equals(a, b, StringComparison.OrdinalIgnoreCase);
        }
        catch
        {
            return true;
        }
    }

    private static bool Spawn(string exe)
    {
        try
        {
            var info = new ProcessStartInfo(exe)
            {
                // 工作目录必须是 exe 所在目录：后端按"自己旁边"找 onnxruntime.dll
                // 和 models\，相对路径一律从这个目录算。
                WorkingDirectory = Path.GetDirectoryName(exe) ?? AppContext.BaseDirectory,
                UseShellExecute = false,
                CreateNoWindow = true,
                WindowStyle = ProcessWindowStyle.Hidden,
            };

            _spawned = Process.Start(info);
            AppLog.Write(_spawned is null
                ? "BackendLauncher: Process.Start returned null"
                : $"BackendLauncher: started backend pid {_spawned.Id}");
            return _spawned is not null;
        }
        catch (Exception ex)
        {
            LastError = $"启动本地识别引擎失败：{ex.Message}";
            AppLog.Write($"BackendLauncher: {LastError}");
            return false;
        }
    }
}
