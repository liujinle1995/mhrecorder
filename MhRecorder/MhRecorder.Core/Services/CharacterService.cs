// 角色库的读写。
//
// 页面原来直接调 IAppService，过滤（搜索）逻辑写在 import 进来的 List 上，
// 删除写在按钮的事件处理器里。这里把"角色库"当成一个东西：列表、搜索、删除、
// 确认身份。

using MhRecorder.Core.Models;

namespace MhRecorder.Core.Services;

public sealed class CharacterService
{
    private readonly BackendService _backend;

    public CharacterService(BackendService backend) => _backend = backend;

    public async Task<IReadOnlyList<CharacterDetail>> ListAsync(CancellationToken ct = default)
    {
        try
        {
            return await _backend.Api.ListCharactersAsync(ct).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            AppLog.Write($"CharacterService.List failed: {ex}");
            return Array.Empty<CharacterDetail>();
        }
    }

    /// <summary>Case-insensitive match on 昵称 or 游戏ID — what the search box promises.</summary>
    public static bool Matches(Character character, string query)
    {
        if (string.IsNullOrWhiteSpace(query)) return true;
        var q = query.Trim();
        return character.Nickname.Contains(q, StringComparison.OrdinalIgnoreCase)
            || (character.GameId?.Contains(q, StringComparison.OrdinalIgnoreCase) ?? false);
    }

    public async Task<bool> DeleteAsync(string id, CancellationToken ct = default)
    {
        try
        {
            await _backend.Api.DeleteCharacterAsync(id, ct).ConfigureAwait(false);
            return true;
        }
        catch (Exception ex)
        {
            AppLog.Write($"CharacterService.Delete failed: {ex}");
            return false;
        }
    }

    public async Task ConfirmAsync(string id, double confidence, CancellationToken ct = default)
    {
        try
        {
            await _backend.Api.ConfirmCharacterAsync(id, confidence, ct).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            AppLog.Write($"CharacterService.Confirm failed: {ex}");
        }
    }
}
