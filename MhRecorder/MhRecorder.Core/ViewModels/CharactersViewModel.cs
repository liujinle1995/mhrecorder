// 角色管理页的 ViewModel：搜索、翻页、删除。
//
// 原来这三件事都长在 code-behind 里，和卡片动画混在一起：搜索框的 TextChanged
// 直接重建 Canvas，删除按钮自己弹 ContentDialog、自己重新加载。现在列表与
// 游标是状态，动画是视图的事——页面只订阅 CurrentIndex 变化去播转场。

using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using MhRecorder.Core.Infrastructure;
using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.Core.ViewModels;

public sealed partial class CharactersViewModel : ObservableObject
{
    private readonly CharacterService _characters;
    private readonly IDialogService _dialogs;
    private readonly INavigationService _navigation;

    private List<CharacterDetail> _all = new();

    public CharactersViewModel(
        CharacterService characters, IDialogService dialogs, INavigationService navigation)
    {
        _characters = characters;
        _dialogs = dialogs;
        _navigation = navigation;
    }

    [ObservableProperty]
    private ObservableCollection<CharacterCardViewModel> _cards = new();

    [ObservableProperty]
    private string _searchText = string.Empty;

    [ObservableProperty]
    private string _subtitle = "导入的角色都会在这里以卡片呈现";

    [ObservableProperty]
    private int _currentIndex;

    [ObservableProperty]
    private bool _isEmpty;

    partial void OnSearchTextChanged(string value) => ApplyFilter(resetIndex: true);

    public CharacterCardViewModel? Current =>
        CurrentIndex >= 0 && CurrentIndex < Cards.Count ? Cards[CurrentIndex] : null;

    public async Task LoadAsync()
    {
        _all = (await _characters.ListAsync()).ToList();

        Subtitle = _all.Count == 0
            ? "导入的角色都会在这里以卡片呈现"
            : $"导入的角色都会在这里以卡片呈现，共 {_all.Count} 个角色 · 最近导入 {Fmt.FriendlyStamp(_all.Max(c => c.LastRecordedAt))}";

        ApplyFilter(resetIndex: false);
    }

    private void ApplyFilter(bool resetIndex)
    {
        var previousId = Current?.Id;

        var filtered = _all
            .Where(d => CharacterService.Matches(d.Character, SearchText))
            .Select(d => new CharacterCardViewModel(d))
            .ToList();

        Cards.Clear();
        foreach (var card in filtered) Cards.Add(card);

        IsEmpty = Cards.Count == 0;

        if (resetIndex)
        {
            CurrentIndex = 0;
        }
        else
        {
            // Keep the same character selected across a reload when possible.
            var index = previousId is null ? -1 : filtered.FindIndex(c => c.Id == previousId);
            CurrentIndex = index >= 0 ? index : Math.Clamp(CurrentIndex, 0, Math.Max(0, Cards.Count - 1));
        }

        ApplyCurrentFlag();
        OnPropertyChanged(nameof(HasCards));
        OnPropertyChanged(nameof(Count));
    }

    public bool HasCards => Cards.Count > 0;
    public int Count => Cards.Count;

    /// <summary>Mark exactly one card as the raised centre card.</summary>
    private void ApplyCurrentFlag()
    {
        for (var i = 0; i < Cards.Count; i++) Cards[i].IsCurrent = i == CurrentIndex;
        OnPropertyChanged(nameof(Current));
    }

    partial void OnCurrentIndexChanged(int value) => ApplyCurrentFlag();

    /// <summary>Move by <paramref name="delta"/>, wrapping around the list.</summary>
    public void Step(int delta)
    {
        if (Cards.Count == 0) return;
        CurrentIndex = ((CurrentIndex + delta) % Cards.Count + Cards.Count) % Cards.Count;
    }

    [RelayCommand]
    private void Next() => Step(1);

    [RelayCommand]
    private void Prev() => Step(-1);

    [RelayCommand]
    private void NewCharacter() => _navigation.NavigateTo(AppPage.Import);

    [RelayCommand]
    private async Task DeleteAsync(CharacterCardViewModel? card)
    {
        if (card is null) return;

        var confirmed = await _dialogs.ConfirmAsync(
            "删除角色",
            $"「{card.Nickname}」的全部历史记录会一起删除，操作不可撤销。",
            "删除");
        if (!confirmed) return;

        await _characters.DeleteAsync(card.Id);
        await LoadAsync();
    }
}
