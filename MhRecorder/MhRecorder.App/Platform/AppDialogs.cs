// ContentDialog / 窗口选择器的实现。
//
// 这两件事必须在这里做：ContentDialog 需要 XamlRoot，而 XamlRoot 只有窗口有。
// ViewModel 只发问，不关心它长什么样。

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using MhRecorder.App.Views.Dialogs;
using MhRecorder.Core.Infrastructure;
using MhRecorder.Core.Services;

namespace MhRecorder.App.Platform;

public sealed class AppDialogs : IDialogService, IWindowPicker
{
    private static XamlRoot? Root => App.MainWindowInstance?.Content?.XamlRoot;

    public async Task<bool> ConfirmAsync(
        string title, string message, string primaryText, string closeText = "取消")
    {
        var root = Root;
        if (root is null) return false;

        var dialog = new ContentDialog
        {
            Title = title,
            Content = message,
            PrimaryButtonText = primaryText,
            CloseButtonText = closeText,
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = root,
        };
        return await dialog.ShowAsync() == ContentDialogResult.Primary;
    }

    public async Task AlertAsync(string title, string message)
    {
        var root = Root;
        if (root is null) return;

        var dialog = new ContentDialog
        {
            Title = title,
            Content = message,
            CloseButtonText = "知道了",
            DefaultButton = ContentDialogButton.None,
            XamlRoot = root,
        };
        await dialog.ShowAsync();
    }

    public async Task<WindowTarget?> PickAsync(IReadOnlyList<WindowTarget> windows)
    {
        var root = Root;
        if (root is null || windows.Count == 0) return null;

        var dialog = new WindowPickerDialog(windows) { XamlRoot = root };
        var result = await dialog.ShowAsync();
        return result == ContentDialogResult.Primary ? dialog.Selected : null;
    }
}
