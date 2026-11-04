// 让界面自己负责"后端在不在"。
//
// 以前只有 run.ps1 会先拉起 mhservice.exe 再开界面，所以：
//   * 双击 MhRecorder.exe（不带启动器）→ 管道没人应答 → 红色横幅"未连接后端"；
//   * 装进开始菜单 / 桌面快捷方式（Inno Setup 生成的就是直接启动 exe）→ 同样连不上；
//   * 用户看到的是"程序坏了"，但其实是后端根本没被拉起来。
//
// 现在 BackendService 在放弃之前会调用这里：同目录有 mhservice.exe 就拉起来、等管道就绪、
// 连上再继续。启动器仍然可用（它只是提前把后端拉好），两种入口行为一致。
//
// 反过来的责任也要担：如果后端是**我们**拉起来的，界面退出时把它关掉，不在用户机器上
// 留一个没人用的常驻进程。用户自己跑的（run.ps1 / 终端）不动。

using System.Diagnostics;

namespace MhRecorder.Core.Services;

/// <summary>Starts the Rust backend next to the app when nobody else did, and stops it again on exit.</summary>
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
    /// Connect to the pipe, starting the backend first if it is not running.
    /// Returns null when no backend could be reached within <paramref name="waitMs"/>.
    /// </summary>
    public static RemoteAppService? ConnectOrStart(int waitMs = 8000)
    {
        // 1. 已经在跑（启动器、上一次会话、或用户在终端里手动开的）——直接连。
        var existing = TryConnect(1200);
        if (existing is not null) return existing;

        var exe = ServicePath;
        if (!File.Exists(exe))
        {
            LastError = $"找不到后端程序：{exe}";
            AppLog.Write($"BackendLauncher: {LastError}");
            return null;
        }

        lock (Gate)
        {
            // 2. 进程在、管道还没就绪（启动中或刚崩溃重启）——等它，别重复拉起。
            var alreadyRunning = HasRunningInstance();
            if (!alreadyRunning)
            {
                if (!Spawn(exe)) return null;
            }

            // 3. 轮询管道直到超时。单次连接超时给短一点，才能多试几轮。
            var deadline = Environment.TickCount64 + waitMs;
            while (Environment.TickCount64 < deadline)
            {
                var remote = TryConnect(600);
                if (remote is not null)
                {
                    LastError = null;
                    AppLog.Write("BackendLauncher: backend is up");
                    return remote;
                }

                Thread.Sleep(200);
            }

            LastError = alreadyRunning
                ? "本地识别引擎没有响应（mhservice.exe 在运行但管道无应答）。"
                : "本地识别引擎启动超时（mhservice.exe 已拉起但没在 8 秒内就绪）。";
            AppLog.Write($"BackendLauncher: {LastError}");

            // 超时后不要留下一个半死不活的进程：是我们拉的就收回去。
            StopSpawned();
            return null;
        }
    }

    /// <summary>Kill the backend only if this process started it.</summary>
    public static void StopSpawned()
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
                    AppLog.Write("BackendLauncher: stopped the backend we started");
                }
            }
            catch (Exception ex)
            {
                AppLog.Write($"BackendLauncher: stop failed: {ex.Message}");
            }
            finally
            {
                _spawned.Dispose();
                _spawned = null;
            }
        }
    }

    private static RemoteAppService? TryConnect(int timeoutMs)
    {
        try
        {
            return RemoteAppService.TryCreate(timeoutMs);
        }
        catch (Exception ex)
        {
            AppLog.Write($"BackendLauncher: connect failed: {ex.Message}");
            return null;
        }
    }

    private static bool HasRunningInstance()
    {
        try
        {
            return Process.GetProcessesByName("mhservice").Length > 0;
        }
        catch
        {
            return false;
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
