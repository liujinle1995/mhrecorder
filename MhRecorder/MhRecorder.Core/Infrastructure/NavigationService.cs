// 页面外壳与对话框的抽象。
//
// 这一层存在的唯一理由：ViewModel 必须能在没有 WinUI 的情况下被构造和测试。
// NavigationService 描述"去哪一页"，DialogService 描述"问用户一句"，两者都由
// App 层实现（那里才拿得到 Frame 和 XamlRoot）。

namespace MhRecorder.Core.Infrastructure;

/// <summary>The four top-level pages of the shell.</summary>
public enum AppPage
{
    Import = 0,
    Characters = 1,
    Assets = 2,
    Settings = 3,
}

/// <summary>Shell navigation, implemented by the main window.</summary>
public interface INavigationService
{
    void NavigateTo(AppPage page);

    AppPage Current { get; }
}

/// <summary>No-op shell used by tests and by design-time constructors.</summary>
public sealed class NullNavigationService : INavigationService
{
    public AppPage Current { get; private set; } = AppPage.Import;

    public void NavigateTo(AppPage page) => Current = page;
}
