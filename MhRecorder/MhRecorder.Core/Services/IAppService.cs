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

    /// <summary>
    /// Persist a (possibly user-corrected) recognition.
    ///
    /// The backend is the last line of defence for identity: when nothing says
    /// whose capture this is, it parks the result instead of guessing a row.
    /// Callers must check <see cref="SaveRecognitionOutcome.IsPending"/> rather
    /// than assuming an id came back.
    /// </summary>
    Task<SaveRecognitionOutcome> SaveRecognitionAsync(RecognitionResult result, IReadOnlyList<string> screens, CancellationToken ct = default);

    /// <summary>Recognitions parked because nobody (not even the matcher) knew whose they were.</summary>
    Task<IReadOnlyList<PendingRecognition>> ListPendingAsync(CancellationToken ct = default);

    Task AssignPendingAsync(string id, string characterId, CancellationToken ct = default);

    Task DiscardPendingAsync(string id, CancellationToken ct = default);

    Task<IReadOnlyList<CharacterDetail>> ListCharactersAsync(CancellationToken ct = default);

    Task<StatisticsPayload> GetStatisticsAsync(int recentLimit = 5, CancellationToken ct = default);

    Task<Settings> GetSettingsAsync(CancellationToken ct = default);

    Task SetSettingsAsync(Settings settings, CancellationToken ct = default);

    Task<ExportResult?> ExportAsync(ExportFormat format, string path, CancellationToken ct = default);

    Task ClearDataAsync(CancellationToken ct = default);

    Task DeleteCharacterAsync(string id, CancellationToken ct = default);

    Task UpdateCharacterIdentityAsync(string id, string? nickname, string? gameId, long? level, CancellationToken ct = default);

    /// <summary>
    /// Mark a character's identity as verified by the user.
    ///
    /// The review workspace lets a human accept a candidate the matcher was not
    /// sure about; recording that raises the row's confidence so the same doubt
    /// is not raised again on every later capture.
    /// </summary>
    Task ConfirmCharacterAsync(string id, double confidence, CancellationToken ct = default);

    Task<CharacterDetail?> GetCharacterAsync(string id, CancellationToken ct = default);

    /// <summary>
    /// Describe the active recognition plugin (engine id, runtime, models,
    /// whether it is loaded). Lets the Settings page show exactly which
    /// recogniser is in use instead of a generic "离线 OCR" line.
    /// Returns null when the backend is unreachable.
    /// </summary>
    Task<JsonElement?> GetEngineInfoAsync(CancellationToken ct = default);
}

/// <summary>Where a save landed.</summary>
/// <param name="CharacterId">The row it was written to; null when pending.</param>
/// <param name="IsPending">
/// True when the result had no identity at all. Not an error: the data is kept,
/// it just has no owner yet.
/// </param>
/// <param name="PendingId">Id of the parked record, for the 待归属 list.</param>
/// <param name="Message">Human explanation from the backend, when pending.</param>
public sealed record SaveRecognitionOutcome(
    string? CharacterId,
    bool IsPending,
    string? PendingId,
    string? Message)
{
    /// <summary>True when something was actually written to a character row.</summary>
    public bool IsSaved => !string.IsNullOrEmpty(CharacterId);
}

public enum ExportFormat
{
    Json,
    Csv,
    Zip,

    /// <summary>Restorable backup: data.json + a VACUUM INTO snapshot + manifest.</summary>
    Backup,
}
