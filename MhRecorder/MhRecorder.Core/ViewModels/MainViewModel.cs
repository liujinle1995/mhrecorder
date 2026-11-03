// 外壳（侧边栏 / 标题栏 / 状态栏）的 ViewModel。
//
// 导航状态原来是 MainWindow 里几个私有字段 + 一排 Click 处理器，页面之间要跳转
// 得先拿到窗口实例再调用。现在页面对 INavigationService 说"去角色管理"，而
// 选中哪一项由这里算。

using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using MhRecorder.Core.Infrastructure;
using MhRecorder.Core.Services;

namespace MhRecorder.Core.ViewModels;

public sealed partial class MainViewModel : ObservableObject
{
    private readonly BackendService _backend;
    private AppPage _current = AppPage.Import;

    public MainViewModel(BackendService backend)
    {
        _backend = backend;
        _backend.Changed += (_, _) => RefreshBackend();
        RefreshBackend();
    }

    [ObservableProperty]
    private string _backendBadge = "未连接后端";

    [ObservableProperty]
    private string _footerStatus = "已离线运行 · 未连接本地识别引擎";

    [ObservableProperty]
    private string _footerSnapshot = "最近资产快照：—";

    [ObservableProperty]
    private bool _isSidebarCompact;

    /// <summary>True when the shell must keep a banner up (no backend / demo data).</summary>
    [ObservableProperty]
    private bool _hasBackendNotice;

    [ObservableProperty]
    private string _backendNotice = string.Empty;

    /// <summary>"Error" / "Warning" — translated to InfoBarSeverity by the view.</summary>
    [ObservableProperty]
    private string _backendNoticeSeverity = "Warning";

    /// <summary>Label for the banner's single action ("重试连接").</summary>
    [ObservableProperty]
    private string _backendNoticeAction = "重试连接";

    [ObservableProperty]
    private bool _backendNoticeBusy;

    public AppPage Current => _current;

    /// <summary>Raised when the selected page changes; the shell navigates the Frame.</summary>
    public event EventHandler<AppPage>? NavigationRequested;

    public void Select(AppPage page)
    {
        if (_current == page) return;
        _current = page;
        OnPropertyChanged(nameof(Current));
        OnPropertyChanged(nameof(IsImportSelected));
        OnPropertyChanged(nameof(IsCharactersSelected));
        OnPropertyChanged(nameof(IsAssetsSelected));
        OnPropertyChanged(nameof(IsSettingsSelected));
        NavigationRequested?.Invoke(this, page);
    }

    public bool IsImportSelected => _current == AppPage.Import;
    public bool IsCharactersSelected => _current == AppPage.Characters;
    public bool IsAssetsSelected => _current == AppPage.Assets;
    public bool IsSettingsSelected => _current == AppPage.Settings;

    [RelayCommand]
    private void GoImport() => Select(AppPage.Import);

    [RelayCommand]
    private void GoCharacters() => Select(AppPage.Characters);

    [RelayCommand]
    private void GoAssets() => Select(AppPage.Assets);

    [RelayCommand]
    private void GoSettings() => Select(AppPage.Settings);

    private void RefreshBackend()
    {
        BackendBadge = _backend.StatusLabel;
        FooterStatus = _backend.StatusDetail;

        switch (_backend.State)
        {
            case BackendState.Unavailable:
                HasBackendNotice = true;
                BackendNoticeSeverity = "Error";
                BackendNoticeAction = "重试连接";
                BackendNotice = _backend.UnavailableReason
                    ?? "未连接本地识别引擎，识别与保存不可用。";
                break;

            case BackendState.Demo:
                // 演示数据在调试构建里是有意为之，但必须一眼看得出来：
                // 它不写库、不产生历史，关掉窗口就没了。
                HasBackendNotice = true;
                BackendNoticeSeverity = "Warning";
                BackendNoticeAction = "连接后端";
                BackendNotice = "演示数据：识别结果、修改与保存都不会落库，仅用于界面调试。";
                break;

            default:
                HasBackendNotice = false;
                BackendNotice = string.Empty;
                break;
        }
    }

    /// <summary>Banner action: try the pipe again, and say so either way.</summary>
    [RelayCommand]
    private async Task RetryBackendAsync()
    {
        BackendNoticeBusy = true;
        try
        {
            if (_backend.TryReconnect(2500))
            {
                RefreshBackend();
                BackendBadge = "已连接离线识别引擎";
                return;
            }

            // Reconnect is a blocking handshake; keep the UI thread out of it
            // when it fails (2.5s of nothing looks like a crash).
            await Task.Yield();
            RefreshBackend();
            BackendNotice = (_backend.UnavailableReason
                ?? "仍未连上本地识别引擎。") + " 请先退出本程序，再重新双击启动器。";
        }
        finally
        {
            BackendNoticeBusy = false;
        }
    }

    public void SetFooterSnapshot(string text) => FooterSnapshot = text;
}

/// <summary>Shell navigation implemented by the main window on top of the frame.</summary>
public sealed class ShellNavigationService : INavigationService
{
    private readonly MainViewModel _shell;

    public ShellNavigationService(MainViewModel shell) => _shell = shell;

    public AppPage Current => _shell.Current;

    public void NavigateTo(AppPage page) => _shell.Select(page);
}
