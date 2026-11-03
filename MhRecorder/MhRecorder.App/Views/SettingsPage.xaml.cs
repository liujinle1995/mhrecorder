using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using MhRecorder.App.Services;
using MhRecorder.App.Views.Dialogs;
using MhRecorder.Core.ViewModels;

namespace MhRecorder.App.Views;

/// <summary>Page 4 — 设置: 识别 / 数据存储 / 关于。</summary>
public sealed partial class SettingsPage : Page
{
    public SettingsViewModel ViewModel { get; } = AppHost.Settings;

    public SettingsPage()
    {
        InitializeComponent();
        Loaded += OnLoaded;
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            await ViewModel.LoadAsync();
        }
        catch (Exception ex)
        {
            App.Log($"SettingsPage.OnLoaded failed: {ex}");
        }
    }

    /// <summary>
    /// 待归属记录：识别出来了但不知道是谁的截图。放在设置里而不是主流程上，
    /// 因为它是补救入口，不是日常路径。
    /// </summary>
    private async void OnManagePendingClick(object sender, RoutedEventArgs e)
    {
        try
        {
            if (XamlRoot is null) return;
            await new PendingDialog(AppHost.Backend.Api) { XamlRoot = XamlRoot }.ShowAsync();
            await ViewModel.LoadPendingAsync();
        }
        catch (Exception ex)
        {
            App.Log($"OnManagePendingClick failed: {ex}");
        }
    }

    /// <summary>
    /// Release notes are a dialog, not an inline card: four releases of history
    /// made the page scroll through a wall of text nobody re-reads.
    /// </summary>
    private async void OnViewChangelogClick(object sender, RoutedEventArgs e)
    {
        try
        {
            if (XamlRoot is null) return;
            await new ChangelogDialog { XamlRoot = XamlRoot }.ShowAsync();
        }
        catch (Exception ex)
        {
            App.Log($"OnViewChangelogClick failed: {ex}");
        }
    }
}
