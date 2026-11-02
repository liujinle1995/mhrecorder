// The single interface every page talks to. Two implementations exist:
//   * RemoteAppService  - the real Rust backend over the `mhrecorder.v1` pipe
//   * FakeAppService    - an in-memory implementation so the UI runs standalone
// The App picks whichever is usable at startup and can hot-swap later.

using System.Text.Json;
using MhRecorder.Core.Models;

namespace MhRecorder.Core.Services;

/// <summary>Backend-agnostic façade mirroring the frozen pipe protocol.</summary>
public interface IAppService
{
    /// <summary>True when this instance talks to the real Rust backend.</summary>
    bool IsRemote { get; }

    /// <summary>Short human label for the status bar, e.g. "离线模式" / "已连接后端".</summary>
    string StatusLabel { get; }

    Task<RecognitionResult?> RecognizeAsync(IReadOnlyList<RecognizeImage> images, CancellationToken ct = default);

    Task<string?> SaveRecognitionAsync(RecognitionResult result, IReadOnlyList<string> screens, CancellationToken ct = default);

    Task<IReadOnlyList<CharacterDetail>> ListCharactersAsync(CancellationToken ct = default);

    Task<StatisticsPayload> GetStatisticsAsync(int recentLimit = 5, CancellationToken ct = default);

    Task<Settings> GetSettingsAsync(CancellationToken ct = default);

    Task SetSettingsAsync(Settings settings, CancellationToken ct = default);

    Task<ExportResult?> ExportAsync(ExportFormat format, string path, CancellationToken ct = default);

    Task ClearDataAsync(CancellationToken ct = default);

    Task DeleteCharacterAsync(string id, CancellationToken ct = default);

    Task UpdateCharacterIdentityAsync(string id, string? nickname, string? gameId, long? level, CancellationToken ct = default);

    Task<CharacterDetail?> GetCharacterAsync(string id, CancellationToken ct = default);

    /// <summary>
    /// Describe the active recognition plugin (engine id, runtime, models,
    /// whether it is loaded). Lets the Settings page show exactly which
    /// recogniser is in use instead of a generic "离线 OCR" line.
    /// Returns null when the backend is unreachable.
    /// </summary>
    Task<JsonElement?> GetEngineInfoAsync(CancellationToken ct = default);
}

public enum ExportFormat
{
    Json,
    Csv,
    Zip,
}
