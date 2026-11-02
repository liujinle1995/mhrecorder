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
    private string _backendBadge = "离线演示数据";

    [ObservableProperty]
    private string _footerStatus = "已离线运行 · 使用内置演示数据";

    [ObservableProperty]
    private string _footerSnapshot = "最近资产快照：今天 14:32";

    [ObservableProperty]
    private bool _isSidebarCompact;

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
