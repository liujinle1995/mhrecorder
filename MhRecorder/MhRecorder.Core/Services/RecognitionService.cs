// 一次"识别 → 人工确认 → 存档"的业务流程。
//
// 这段流程以前整个写在 ImportPage.xaml.cs 里：判断结果是否可用、未识别时让用户
// 选归属、保存后回填身份、是否自动跳转。它全是业务规则，和控件没有关系，所以
// 挪到这里；出问题时可以单独看这一处，而不是在 900 行页面代码里找。

using MhRecorder.Core.Models;

namespace MhRecorder.Core.Services;

/// <summary>Outcome of one recognition round.</summary>
public sealed record RecognizeOutcome(RecognitionResult? Result, string? Message, bool IsWarning)
{
    public bool HasResult => Result is not null;

    public static RecognizeOutcome Fail(string message) => new(null, message, true);

    public static RecognizeOutcome Success(RecognitionResult result) => new(result, null, false);
}

public sealed class RecognitionService
{
    private readonly BackendService _backend;

    public RecognitionService(BackendService backend) => _backend = backend;

    /// <summary>
    /// Run the recogniser over a batch and decide whether the answer is worth
    /// showing. "Nothing read" is reported as a message rather than an empty
    /// panel: a result full of zeros reads like a successful read and would
    /// archive an empty character.
    /// </summary>
    public async Task<RecognizeOutcome> RecognizeAsync(
        IReadOnlyList<ImageFile> images, CancellationToken ct = default)
    {
        if (images.Count == 0) return RecognizeOutcome.Fail("没有可识别的图片");

        try
        {
            var wire = images.Select(i => i.ToWire()).ToList();
            var result = await _backend.Api.RecognizeAsync(wire, ct).ConfigureAwait(false);
            if (!IsUsable(result))
            {
                return RecognizeOutcome.Fail(
                    "这张截图里没读到角色数值。请确认截的是「人物属性」或「背包」面板，且角色信息完整可见");
            }
            return RecognizeOutcome.Success(result!);
        }
        catch (BackendException ex) when (ex.Code == ErrorCodes.Cancelled)
        {
            return new RecognizeOutcome(null, "识别已取消", true);
        }
        catch (BackendException ex) when (ex.Code == ErrorCodes.Timeout)
        {
            return new RecognizeOutcome(null, "识别超时：截图过多或分辨率过高，试试一次只导入 1–2 张", true);
        }
        catch (BackendException ex) when (ex.Code == ErrorCodes.TooLarge)
        {
            return new RecognizeOutcome(null, "图片过大：单张请控制在 40MP 以内，一次最多 8 张", true);
        }
        catch (Exception ex)
        {
            AppLog.Write($"RecognitionService.Recognize failed: {ex}");
            return RecognizeOutcome.Fail("识别失败：" + ex.Message);
        }
    }

    /// <summary>
    /// A result counts as usable when it carries at least one real number.
    ///
    /// Deliberately ignores <see cref="RecognitionResult.Screens"/>: the screen
    /// kind is a label for the UI, not a verdict. Keying the success message off
    /// it meant a capture whose eight stats had all been read correctly still
    /// reported "没读到角色数值", because the pixel classifier said Unknown.
    /// </summary>
    public static bool IsUsable(RecognitionResult? result)
    {
        if (result is null) return false;

        var s = result.Stats;
        return s.Hp?.Value > 0 || s.Mp?.Value > 0
            || s.PhysicalDamage?.Value > 0 || s.PhysicalDefense?.Value > 0
            || s.MagicDamage?.Value > 0 || s.MagicDefense?.Value > 0
            || s.Speed?.Value > 0 || s.HealingPower?.Value > 0
            || result.Currencies.Gold?.Value > 0 || result.Currencies.Silver?.Value > 0;
    }

    /// <summary>True when the capture carries no identity of its own (a 包裹 panel).</summary>
    public static bool NeedsOwner(RecognitionResult result) =>
        string.IsNullOrEmpty(result.CharacterId)
        && string.IsNullOrEmpty(result.Character.GameId)
        && string.IsNullOrEmpty(result.Character.Nickname);

    /// <summary>Persist one recognition against the character it belongs to.</summary>
    public async Task<string?> SaveAsync(
        RecognitionResult result, IReadOnlyList<string> screens, CancellationToken ct = default)
    {
        var id = await _backend.Api.SaveRecognitionAsync(result, screens, ct).ConfigureAwait(false);
        if (string.IsNullOrEmpty(id)) return null;

        result.CharacterId = id;
        await FillIdentityFromLibraryAsync(result, ct).ConfigureAwait(false);
        return id;
    }

    /// <summary>
    /// Fill in whatever identity the result is missing from the library row it
    /// was saved against. A 包裹 capture has no nickname / 游戏ID / level of its
    /// own, so without this the panel would keep saying "未命名角色" even though
    /// the user just attributed it to 莫莫莫.
    /// </summary>
    public async Task FillIdentityFromLibraryAsync(RecognitionResult result, CancellationToken ct = default)
    {
        if (string.IsNullOrEmpty(result.CharacterId)) return;
        try
        {
            var characters = await _backend.Api.ListCharactersAsync(ct).ConfigureAwait(false);
            var hit = characters.FirstOrDefault(c => c.Character.Id == result.CharacterId);
            if (hit is null) return;

            result.Character.Nickname ??= hit.Character.Nickname;
            result.Character.GameId ??= hit.Character.GameId;
            result.Character.Level ??= hit.Character.Level;
        }
        catch (Exception ex)
        {
            AppLog.Write($"FillIdentityFromLibrary failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Attribute an unattributed capture to a character the user picked, and tell
    /// the backend that this identity is now verified rather than guessed.
    /// </summary>
    public async Task AttributeToAsync(
        RecognitionResult result, string characterId, CancellationToken ct = default)
    {
        result.CharacterId = characterId;
        result.IdentitySource = IdentitySource.Manual;
        await FillIdentityFromLibraryAsync(result, ct).ConfigureAwait(false);
        try
        {
            await _backend.Api.ConfirmCharacterAsync(characterId, 1.0, ct).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            // Not fatal: the attribution still applies to this save.
            AppLog.Write($"confirm identity failed: {ex.Message}");
        }
    }
}
