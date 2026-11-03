using System.Collections.ObjectModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.App.Views.Dialogs;

/// <summary>
/// 处理"识别出来了但不知道是谁的"记录。
///
/// 这些记录从前会直接变成「未命名角色」——它永远匹配不上任何后续截图，却会
/// 出现在角色列表和资产统计里。现在它们停在这里，指定一次归属就能补成正式快照。
/// </summary>
public sealed partial class PendingDialog : ContentDialog
{
    private readonly IAppService _api;
    private readonly ObservableCollection<PendingRow> _rows = new();

    /// <summary>已经点过一次"丢弃"的那条记录 —— 第二次点击才真的删。</summary>
    private string? _confirmDiscardId;

    public PendingDialog(IAppService api)
    {
        _api = api;
        InitializeComponent();
        Resources["ContentDialogMaxWidth"] = 620.0;
        Resources["ContentDialogMinWidth"] = 560.0;
        Resources["ContentDialogMaxHeight"] = 640.0;
        Loaded += OnLoaded;
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            await ReloadAsync();
        }
        catch (Exception ex)
        {
            App.Log($"PendingDialog.OnLoaded failed: {ex}");
            StatusText.Text = "读取待归属记录失败：" + ex.Message;
        }
    }

    private async Task ReloadAsync()
    {
        _rows.Clear();
        foreach (var item in await _api.ListPendingAsync())
        {
            _rows.Add(new PendingRow(item));
        }
        PendingList.ItemsSource = _rows;

        // 候选角色：归属只能在已有角色之间选。真·新角色请走录入页。
        var owners = (await _api.ListCharactersAsync())
            .Select(c => new OwnerOption(c.Character.Id, DescribeCharacter(c.Character)))
            .ToList();
        OwnerCombo.ItemsSource = owners;
        if (owners.Count > 0) OwnerCombo.SelectedIndex = 0;

        UpdateEmptyState();

        // 只有一条时直接选中：最常见的场景是"刚存进来一条，想马上归位"，
        // 让人先在空列表里点一下没必要。
        if (_rows.Count > 0) PendingList.SelectedIndex = 0;

        StatusText.Text = _rows.Count == 0
            ? string.Empty
            : $"共 {_rows.Count} 条待归属";
    }

    private static string DescribeCharacter(Character c)
    {
        var id = string.IsNullOrEmpty(c.GameId) ? "无ID" : c.GameId;
        var level = c.Level is { } lv ? $"Lv.{lv}" : "Lv.?";
        return $"{c.Nickname} · {level} · {id}";
    }

    private void UpdateEmptyState()
    {
        var empty = _rows.Count == 0;
        EmptyText.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
        PendingList.Visibility = empty ? Visibility.Collapsed : Visibility.Visible;
    }

    private void OnSelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        _confirmDiscardId = null;
        if (PendingList.SelectedItem is PendingRow row)
        {
            StatusText.Text = row.Reason;
        }
    }

    private async void OnAssignClick(object sender, RoutedEventArgs e)
    {
        if (PendingList.SelectedItem is not PendingRow row)
        {
            StatusText.Text = "先在上面选中一条记录";
            return;
        }
        if (OwnerCombo.SelectedItem is not OwnerOption owner)
        {
            StatusText.Text = "库里还没有角色可以归属：先录一张「人物属性」截图";
            return;
        }

        try
        {
            await _api.AssignPendingAsync(row.Id, owner.Id);
            _rows.Remove(row);
            UpdateEmptyState();
            StatusText.Text = $"已归到「{owner.Label}」，可以在角色管理里看到这条快照";
        }
        catch (Exception ex)
        {
            App.Log($"PendingDialog.Assign failed: {ex}");
            StatusText.Text = "归属失败：" + ex.Message;
        }
    }

    private async void OnDiscardClick(object sender, RoutedEventArgs e)
    {
        if (PendingList.SelectedItem is not PendingRow row)
        {
            StatusText.Text = "先在上面选中一条记录";
            return;
        }

        // 删除没有撤销：第一次点只是提示，再点一次才真删。
        if (_confirmDiscardId != row.Id)
        {
            _confirmDiscardId = row.Id;
            StatusText.Text = "再点一次「丢弃」就会永久删除这条记录（不可恢复）";
            return;
        }

        try
        {
            await _api.DiscardPendingAsync(row.Id);
            _rows.Remove(row);
            _confirmDiscardId = null;
            UpdateEmptyState();
            StatusText.Text = "已丢弃";
        }
        catch (Exception ex)
        {
            App.Log($"PendingDialog.Discard failed: {ex}");
            StatusText.Text = "丢弃失败：" + ex.Message;
        }
    }

    /// <summary>One parked recognition, flattened for the list.</summary>
    private sealed class PendingRow
    {
        public PendingRow(PendingRecognition item)
        {
            Id = item.Id;
            Reason = item.Reason;
            Title = BuildTitle(item);
            Detail = BuildDetail(item);
        }

        public string Id { get; }
        public string Reason { get; }
        public string Title { get; }
        public string Detail { get; }

        private static string BuildTitle(PendingRecognition item)
        {
            var screens = item.ScreenshotCount > 0
                ? $"{item.ScreenshotCount} 张截图"
                : "1 张截图";
            var money = item.HasCurrencies ? " · 含金币/银币" : string.Empty;
            return screens + money;
        }

        private static string BuildDetail(PendingRecognition item)
        {
            var when = DateTimeOffset.FromUnixTimeMilliseconds(item.CreatedAt).ToLocalTime();
            var files = item.Screens.Count > 0 ? string.Join("、", item.Screens) : "（未记录文件名）";
            return $"{when:MM-dd HH:mm} · {files}";
        }
    }

    private sealed class OwnerOption
    {
        public OwnerOption(string id, string label)
        {
            Id = id;
            Label = label;
        }

        public string Id { get; }
        public string Label { get; }
    }
}
