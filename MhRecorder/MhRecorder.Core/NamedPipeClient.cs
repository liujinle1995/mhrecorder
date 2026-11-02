// JSON-RPC over the Windows named pipe `mhrecorder.v1`.
//
// One UTF-8 JSON object per line in each direction. The client owns a
// persistent connection, serialises requests, and correlates the echoed `id`.

using System.Collections.Concurrent;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using MhRecorder.Core.Models;

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

    public Task<RecognitionResult?> RecognizeAsync(IEnumerable<RecognizeImage> images, CancellationToken ct = default) =>
        CallAsync<RecognitionResult>(Methods.Recognize, new { Images = images }, ct);

    public Task<JsonElement?> SaveRecognitionAsync(RecognitionResult result, IEnumerable<string> screens, CancellationToken ct = default) =>
        CallAsync(Methods.RecognizeSave, new { Result = result, Screens = screens }, ct);

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
