using System.Text.Json;
using MhRecorder.Core.Models;

namespace MhRecorder.Core.Services;

/// <summary>Forwards every call to the Rust backend over the named pipe.</summary>
public sealed class RemoteAppService : IAppService
{
    private readonly NamedPipeClient _client;

    public RemoteAppService(NamedPipeClient client)
    {
        _client = client;
    }

    public bool IsRemote => true;

    public string StatusLabel => "已连接后端";

    public NamedPipeClient Client => _client;

    public static RemoteAppService? TryCreate(int connectTimeoutMs = 1200)
    {
        var client = new NamedPipeClient(connectTimeout: TimeSpan.FromMilliseconds(connectTimeoutMs));
        try
        {
            if (client.TryConnectAsync().GetAwaiter().GetResult()) return new RemoteAppService(client);
        }
        catch
        {
            // fall through
        }
        client.Dispose();
        return null;
    }

    public async Task<RecognitionResult?> RecognizeAsync(IReadOnlyList<RecognizeImage> images, CancellationToken ct = default) =>
        await _client.RecognizeAsync(images, ct).ConfigureAwait(false);

    public async Task<string?> SaveRecognitionAsync(RecognitionResult result, IReadOnlyList<string> screens, CancellationToken ct = default)
    {
        var payload = await _client.SaveRecognitionAsync(result, screens, ct).ConfigureAwait(false);
        if (payload is null) return null;
        return payload.Value.TryGetProperty("characterId", out var id) ? id.GetString() : null;
    }

    public async Task<IReadOnlyList<CharacterDetail>> ListCharactersAsync(CancellationToken ct = default)
    {
        var payload = await _client.ListCharactersAsync(ct).ConfigureAwait(false);
        return payload?.Characters ?? new List<CharacterDetail>();
    }

    public async Task<StatisticsPayload> GetStatisticsAsync(int recentLimit = 5, CancellationToken ct = default) =>
        await _client.GetStatisticsAsync(recentLimit, ct).ConfigureAwait(false) ?? new StatisticsPayload();

    public async Task<Settings> GetSettingsAsync(CancellationToken ct = default) =>
        await _client.GetSettingsAsync(ct).ConfigureAwait(false) ?? new Settings();

    public async Task SetSettingsAsync(Settings settings, CancellationToken ct = default) =>
        await _client.SetSettingsAsync(settings, ct).ConfigureAwait(false);

    public async Task<ExportResult?> ExportAsync(ExportFormat format, string path, CancellationToken ct = default)
    {
        var method = format switch
        {
            ExportFormat.Json => Methods.ExportJson,
            ExportFormat.Csv => Methods.ExportCsv,
            _ => Methods.ExportZip,
        };
        return await _client.ExportAsync(method, path, ct).ConfigureAwait(false);
    }

    public async Task ClearDataAsync(CancellationToken ct = default) =>
        await _client.ClearDataAsync(ct).ConfigureAwait(false);

    public async Task DeleteCharacterAsync(string id, CancellationToken ct = default) =>
        await _client.DeleteCharacterAsync(id, ct).ConfigureAwait(false);

    public async Task UpdateCharacterIdentityAsync(string id, string? nickname, string? gameId, long? level, CancellationToken ct = default) =>
        await _client.UpdateCharacterAsync(id, nickname, gameId, level, ct).ConfigureAwait(false);

    public async Task<CharacterDetail?> GetCharacterAsync(string id, CancellationToken ct = default) =>
        await _client.GetCharacterAsync(id, ct).ConfigureAwait(false);

    public async Task<System.Text.Json.JsonElement?> GetEngineInfoAsync(CancellationToken ct = default) =>
        await _client.GetEngineInfoAsync(ct).ConfigureAwait(false);
}
