// JSON-RPC over the Windows named pipe `mhrecorder.v1`.
//
// One UTF-8 JSON object per line in each direction. The client owns a
// persistent connection, serialises requests, and correlates the echoed `id`.

using System.Collections.Concurrent;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.Core;

/// <summary>Talks to the Rust `mhservice` backend over the named pipe.</summary>
public sealed class NamedPipeClient : IDisposable
{
    private readonly string _pipeName;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private readonly ConcurrentDictionary<long, TaskCompletionSource<WireResponse>> _pending = new();
    private readonly TimeSpan _connectTimeout;
    private readonly TimeSpan _callTimeout;

    private NamedPipeClientStream? _stream;
    private StreamReader? _reader;
    private StreamWriter? _writer;
    private Task? _readLoop;
    private long _nextId;
    private volatile bool _disposed;

    public NamedPipeClient(
        string pipeName = ProtocolConstants.PipeName,
        TimeSpan? connectTimeout = null,
        TimeSpan? callTimeout = null)
    {
        _pipeName = pipeName;
        _connectTimeout = connectTimeout ?? TimeSpan.FromMilliseconds(1500);
        _callTimeout = callTimeout ?? TimeSpan.FromSeconds(30);
    }

    public bool IsConnected { get; private set; }

    /// <summary>Try to connect if not already connected. Returns false when no backend is up.</summary>
    public async Task<bool> TryConnectAsync(CancellationToken ct = default)
    {
        if (IsConnected) return true;

        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (IsConnected) return true;
            TearDown();

            var stream = new NamedPipeClientStream(
                ".", _pipeName, PipeDirection.InOut,
                PipeOptions.Asynchronous | PipeOptions.WriteThrough);

            try
            {
                await stream.ConnectAsync((int)_connectTimeout.TotalMilliseconds, ct).ConfigureAwait(false);
            }
            catch (Exception)
            {
                stream.Dispose();
                return false;
            }

            _stream = stream;
            _reader = new StreamReader(stream, new UTF8Encoding(false), false, 4096, leaveOpen: true);
            _writer = new StreamWriter(stream, new UTF8Encoding(false), 4096, leaveOpen: true);
            _writer.AutoFlush = true;
            IsConnected = true;
            _readLoop = Task.Run(ReadLoopAsync);
            return true;
        }
        catch (Exception)
        {
            TearDown();
            return false;
        }
        finally
        {
            _gate.Release();
        }
    }

    /// <summary>Send one request and await its response.</summary>
    public async Task<JsonElement?> CallAsync(string method, object? parameters, CancellationToken ct = default)
    {
        if (!IsConnected)
        {
            if (!await TryConnectAsync(ct).ConfigureAwait(false))
                throw new BackendUnavailableException($"后端服务未运行（管道 {_pipeName} 不可用）");
        }

        var id = Interlocked.Increment(ref _nextId);
        var tcs = new TaskCompletionSource<WireResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
        _pending[id] = tcs;

        try
        {
            var line = Json.Serialize(new WireRequest { Id = id, Method = method, Params = parameters });
            await _gate.WaitAsync(ct).ConfigureAwait(false);
            try
            {
                var writer = _writer ?? throw new BackendUnavailableException("管道已断开");
                await writer.WriteLineAsync(line).ConfigureAwait(false);
            }
            finally
            {
                _gate.Release();
            }

            using var timeoutCts = new CancellationTokenSource(_callTimeout);
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, timeoutCts.Token);
            var response = await tcs.Task.WaitAsync(linked.Token).ConfigureAwait(false);

            if (!response.Ok)
            {
                var err = response.Error;
                throw new BackendException(err?.Code ?? ErrorCodes.Internal, err?.Message ?? "未知后端错误");
            }

            return response.Result;
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            throw new BackendUnavailableException($"后端响应超时（{method}）");
        }
        finally
        {
            _pending.TryRemove(id, out _);
        }
    }

    /// <summary>Send one request and deserialise the result into <typeparam name="T"/>.</summary>
    public async Task<T?> CallAsync<T>(string method, object? parameters, CancellationToken ct = default)
    {
        var result = await CallAsync(method, parameters, ct).ConfigureAwait(false);
        if (result is null || result.Value.ValueKind is JsonValueKind.Null or JsonValueKind.Undefined)
            return default;
        return Json.DeserializeElement<T>(result.Value);
    }

    private async Task ReadLoopAsync()
    {
        var reader = _reader;
        if (reader is null) return;

        try
        {
            while (!_disposed)
            {
                var line = await reader.ReadLineAsync().ConfigureAwait(false);
                if (line is null) break;
                if (line.Length == 0) continue;

                WireResponse? response;
                try
                {
                    response = Json.Deserialize<WireResponse>(line);
                }
                catch (JsonException)
                {
                    continue;
                }

                if (response is null) continue;
                var tcs = _pending.TryRemove(response.Id, out var existing) ? existing : null;
                tcs?.TrySetResult(response);
            }
        }
        catch (Exception)
        {
            // Fall through to teardown below.
        }
        finally
        {
            IsConnected = false;
            TearDown();
            foreach (var kvp in _pending)
            {
                kvp.Value.TrySetException(new BackendUnavailableException("管道连接已断开"));
            }
            _pending.Clear();
        }
    }

    private void TearDown()
    {
        IsConnected = false;
        try { _writer?.Dispose(); } catch { }
        try { _reader?.Dispose(); } catch { }
        try { _stream?.Dispose(); } catch { }
        _writer = null;
        _reader = null;
        _stream = null;
    }

    public void Dispose()
    {
        _disposed = true;
        TearDown();
        _gate.Dispose();
    }

    // ---- Typed convenience wrappers over the frozen method list ----

    public Task<PingResult?> PingAsync(CancellationToken ct = default) =>
        CallAsync<PingResult>(Methods.Ping, new { }, ct);

    public Task<VersionInfo?> VersionAsync(CancellationToken ct = default) =>
        CallAsync<VersionInfo>(Methods.Version, new { }, ct);

    /// <summary>
    /// Raw `version` payload, including the nested `engine` plugin descriptor.
    /// Returns the whole object so the Settings page can render fields the
    /// strongly-typed <see cref="VersionInfo"/> does not model yet.
    /// </summary>
    public Task<JsonElement?> GetEngineInfoAsync(CancellationToken ct = default) =>
        CallAsync(Methods.Version, new { }, ct);

    /// <summary>
    /// 识别：`recognize.start` 拿 taskId，然后轮询 `tasks.get` 直到终态。
    /// </summary>
    /// <remarks>
    /// 为什么不能还用同步 `recognize`：服务端一条连接一个线程、请求串行处理，
    /// 同步识别会把连接占到底，`tasks.cancel` 排在它后面，等轮到时任务早跑完了。
    /// 所以取消在旧协议下**不可能**生效，不是实现 bug，是协议形状的问题。
    ///
    /// 这里的取消流程：ct 触发 → 发一次 `tasks.cancel` → 继续轮询到终态。
    /// 轮询本身不受 ct 影响（它就是要读终态的那个动作），只是把等待变成
    /// "正在停止…"而不是立刻返回——否则界面会不知道后端到底停没停。
    /// </remarks>
    public async Task<RecognitionResult?> RecognizeAsync(
        IEnumerable<RecognizeImage> images, CancellationToken ct = default)
    {
        // start 这一步**不接受取消**：它只是一个登记请求（几毫秒到几百毫秒，
        // 取决于要传多少 base64）。如果在半路放弃，会出现最糟的情况——后端已经
        // 建好任务并开始 OCR，而我们没拿到 taskId，既取消不了，还会让下一次
        // start 撞上 taskBusy。所以把 ct 留给后面的轮询，那里才是等待发生的地方。
        var started = await CallAsync(Methods.RecognizeStart, new { Images = images }, CancellationToken.None)
            .ConfigureAwait(false);
        var taskId = started is { } payload && payload.TryGetProperty("taskId", out var idElement)
            ? idElement.GetString()
            : null;
        if (string.IsNullOrEmpty(taskId))
            throw new BackendException(ErrorCodes.Internal, "后端没有返回 taskId，无法跟踪这次识别");

        var cancelSent = false;
        // 轮询间隔：前 20 次用 120 ms（短任务基本立刻出结果），之后退到 400 ms，
        // 免得一次几十秒的识别打出上千条请求。
        var interval = TimeSpan.FromMilliseconds(120);
        var polls = 0;

        while (true)
        {
            if (ct.IsCancellationRequested && !cancelSent)
            {
                cancelSent = true;
                try
                {
                    await CallAsync(Methods.TasksCancel, new { Id = taskId }, CancellationToken.None)
                        .ConfigureAwait(false);
                }
                catch (Exception ex)
                {
                    // 取消请求失败不影响继续读终态：任务可能刚好自己结束了。
                    AppLog.Write($"NamedPipeClient: tasks.cancel failed: {ex.Message}");
                }
            }

            JsonElement? snapshot;
            try
            {
                snapshot = await CallAsync(Methods.TasksGet, new { Id = taskId }, CancellationToken.None)
                    .ConfigureAwait(false);
            }
            catch (BackendException ex) when (ex.Code == ErrorCodes.TaskNotFound)
            {
                throw new BackendException(
                    ErrorCodes.Cancelled, "识别任务已结束或被清理（后端可能重启过）");
            }

            if (snapshot is not { } snap)
                throw new BackendException(ErrorCodes.Internal, "后端没有返回任务状态");

            var state = snap.TryGetProperty("state", out var s) ? s.GetString() ?? "" : "";
            switch (state)
            {
                case "completed":
                    if (snap.TryGetProperty("result", out var result) &&
                        result.ValueKind == JsonValueKind.Object)
                        return Json.DeserializeElement<RecognitionResult>(result);
                    throw new BackendException(ErrorCodes.Internal, "识别任务完成但没有结果");

                case "cancelled":
                    throw new BackendException(ErrorCodes.Cancelled, "识别已取消");

                case "timeout":
                    throw new BackendException(ErrorCodes.Timeout, "识别超时");

                case "failed":
                    var code = ErrorCodes.Internal;
                    var message = "识别失败";
                    if (snap.TryGetProperty("error", out var err) && err.ValueKind == JsonValueKind.Object)
                    {
                        if (err.TryGetProperty("code", out var c)) code = c.GetString() ?? code;
                        if (err.TryGetProperty("message", out var m)) message = m.GetString() ?? message;
                    }
                    throw new BackendException(code, message);
            }

            polls++;
            if (polls > 20) interval = TimeSpan.FromMilliseconds(400);
            // 不能传 ct：取消后还要继续读到终态。
            await Task.Delay(interval, CancellationToken.None).ConfigureAwait(false);
        }
    }

    /// <summary>查一次任务状态（调试/诊断用；正常路径由 <see cref="RecognizeAsync"/> 内部轮询）。</summary>
    public Task<JsonElement?> TaskGetAsync(string taskId, bool consume = false, CancellationToken ct = default) =>
        CallAsync(Methods.TasksGet, new { Id = taskId, Consume = consume }, ct);

    /// <summary>请求取消一个任务。返回后状态是 <c>cancelling</c>，等终态要再查。</summary>
    public Task<JsonElement?> TaskCancelAsync(string taskId, CancellationToken ct = default) =>
        CallAsync(Methods.TasksCancel, new { Id = taskId }, ct);

    /// <summary>
    /// 请后端退出。只有后端确认"没有别的客户端"时才真的会退，
    /// 所以两个窗口共用一个后端时，关掉其中一个不会把另一个的打掉。
    /// </summary>
    public Task<JsonElement?> ShutdownAsync(bool force = false, CancellationToken ct = default) =>
        CallAsync(Methods.SystemShutdown, new { Force = force }, ct);

    public Task<JsonElement?> SaveRecognitionAsync(RecognitionResult result, IEnumerable<string> screens, CancellationToken ct = default) =>
        CallAsync(Methods.RecognizeSave, new { Result = result, Screens = screens }, ct);

    public Task<PendingPayload?> ListPendingAsync(int limit = 50, CancellationToken ct = default) =>
        CallAsync<PendingPayload>(Methods.PendingList, new { Limit = limit }, ct);

    public Task<JsonElement?> AssignPendingAsync(string id, string characterId, CancellationToken ct = default) =>
        CallAsync(Methods.PendingAssign, new { Id = id, CharacterId = characterId }, ct);

    public Task DiscardPendingAsync(string id, CancellationToken ct = default) =>
        CallAsync(Methods.PendingDiscard, new { Id = id }, ct);

    public Task<CharactersPayload?> ListCharactersAsync(CancellationToken ct = default) =>
        CallAsync<CharactersPayload>(Methods.CharactersList, new { }, ct);

    public Task<CharacterDetail?> GetCharacterAsync(string id, CancellationToken ct = default) =>
        CallAsync<CharacterDetail>(Methods.CharactersGet, new { Id = id }, ct);

    public Task UpdateCharacterAsync(string id, string? nickname, string? gameId, long? level, CancellationToken ct = default) =>
        CallAsync(Methods.CharactersUpdate, new { Id = id, Nickname = nickname, GameId = gameId, Level = level }, ct);

    public Task DeleteCharacterAsync(string id, CancellationToken ct = default) =>
        CallAsync(Methods.CharactersDelete, new { Id = id }, ct);

    public Task ConfirmCharacterAsync(string id, double confidence, CancellationToken ct = default) =>
        CallAsync(Methods.CharactersConfirm, new { Id = id, Confidence = confidence }, ct);

    public Task<StatisticsPayload?> GetStatisticsAsync(int recentLimit = 5, CancellationToken ct = default) =>
        CallAsync<StatisticsPayload>(Methods.Statistics, new { RecentLimit = recentLimit }, ct);

    public Task<Settings?> GetSettingsAsync(CancellationToken ct = default) =>
        CallAsync<Settings>(Methods.SettingsGet, new { }, ct);

    public Task SetSettingsAsync(Settings settings, CancellationToken ct = default) =>
        CallAsync(Methods.SettingsSet, settings, ct);

    public Task<ExportResult?> ExportAsync(string method, string path, CancellationToken ct = default) =>
        CallAsync<ExportResult>(method, new { Path = path }, ct);

    public Task ClearDataAsync(CancellationToken ct = default) =>
        CallAsync(Methods.DataClear, new { }, ct);

    /// <summary>Full backup (data.json + database + manifest) written to <paramref name="path"/>.</summary>
    public Task<JsonElement?> BackupAsync(string path, bool includeDatabase = true, CancellationToken ct = default) =>
        CallAsync(Methods.ExportBackup, new { Path = path, IncludeDatabase = includeDatabase }, ct);

    /// <summary>Running recognitions plus the recent task history.</summary>
    public Task<JsonElement?> TasksAsync(int limit = 20, CancellationToken ct = default) =>
        CallAsync(Methods.TasksList, new { Limit = limit }, ct);

    /// <summary>
    /// Ask the backend to stop a running recognition. Cancellation is checked
    /// between images, so the call returns immediately and the task ends after
    /// the image currently being processed.
    /// </summary>
    public Task<JsonElement?> CancelTaskAsync(string taskId, CancellationToken ct = default) =>
        CallAsync(Methods.TasksCancel, new { Id = taskId }, ct);
}

/// <summary>One screenshot payload: name + base64 PNG/JPEG.</summary>
public sealed class RecognizeImage
{
    public string Name { get; set; } = string.Empty;
    public string Data { get; set; } = string.Empty;
}

/// <summary>Result envelope of `characters.list`.</summary>
public sealed class CharactersPayload
{
    public List<CharacterDetail> Characters { get; set; } = new();
}
