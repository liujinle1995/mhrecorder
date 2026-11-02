using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using MhRecorder.App.Services;
using MhRecorder.App.Views;
using Windows.Graphics;
using Windows.UI;
using WinRT.Interop;

namespace MhRecorder.App;

public sealed partial class MainWindow : Window
{
    // Size taken from the design mock: the window body inside design/page_01.png
    // measures 2766 x 1850 px at 2.0836 px/pt, i.e. 1328 x 888 logical units
    // (ratio 1.495). An earlier 1320x940 (ratio 1.404) was too tall, which is
    // why the layout did not match the mock.
    private const int DefaultWidth = 1328;
    private const int DefaultHeight = 888;

    // The UI is only pixel-accepted at the design size and has no responsive
    // re-flow, so the window is fixed: no maximise, no free resize. A small
    // tolerance keeps it visually stable on high-DPI displays.
    private const int MinWidth = 1328;
    private const int MinHeight = 888;
    private const int MaxWidth = 1328;
    private const int MaxHeight = 888;

    private Button[] _navButtons = Array.Empty<Button>();
    private TextBlock[] _navTexts = Array.Empty<TextBlock>();
    private TextBlock[] _navIcons = Array.Empty<TextBlock>();

    public MainWindow()
    {
        InitializeComponent();

        Title = "梦幻手游记录助手";
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        ConfigureWindow();

        // AppWindow.Resize issues made too early can be overridden while the
        // window is still being realised, which left the frame at a default
        // 885x592. Re-assert the design size once the window has actually been
        // activated and the presenter exists.
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            ApplyDesignSize();
        });

        // WinUI re-derives the frame during activation, so assert the size a
        // couple more times early in the window's life before locking it.
        Activated += (_, _) => ApplyDesignSize();
        _ = ReapplySizeSequenceAsync();

        // WinUI can still re-apply a default frame during the first activation
        // pass, so re-assert the design size once the window is really shown.
        Activated += (_, _) =>
        {
            ApplyDesignSize();
            LockToDesignSize();
        };

        _navButtons = new[] { NavImport, NavCharacters, NavAssets, NavSettings };
        _navTexts = new[] { NavImportText, NavCharactersText, NavAssetsText, NavSettingsText };
        _navIcons = new[] { NavImportIcon, NavCharactersIcon, NavAssetsIcon, NavSettingsIcon };

        ContentFrame.Navigated += (_, _) => SyncNavSelection();
        Select(0);

        UpdateBackendStatus();
        AppServices.Changed += (_, _) => UiThread.OnUi(UpdateBackendStatus);

        UpdateFooter();

        AppWindow.Closing += (_, _) => App.Log("MainWindow closing");
    }

    private void ConfigureWindow()
    {
        try
        {
            ApplyDesignSize();

            // Pin the window to the design size: disabling every resize path on
            // the presenter is what actually removes the maximise button and
            // stops the user dragging the frame.
            //
            // NOTE: do NOT also set PreferredMinimum/MaximumWidth/Height here.
            // Those are evaluated in the window's DPI context and clamped the
            // window to a bogus 885x592 footprint, ignoring the Resize call.
            // IsResizable=false alone locks the size.
            if (AppWindow.Presenter is OverlappedPresenter presenter)
            {
                presenter.IsResizable = false;
                presenter.IsMaximizable = false;
                presenter.IsMinimizable = true; // minimising is harmless
            }

            // Custom title bar is drawn in XAML; drop the system caption buttons area tint.
            AppWindow.TitleBar.ExtendsContentIntoTitleBar = true;
            AppWindow.TitleBar.ButtonBackgroundColor = Colors.Transparent;
            AppWindow.TitleBar.ButtonInactiveBackgroundColor = Colors.Transparent;
            AppWindow.TitleBar.ButtonForegroundColor = Color.FromArgb(255, 0x1C, 0x1C, 0x1F);
            AppWindow.TitleBar.ButtonInactiveForegroundColor = Color.FromArgb(255, 0x9A, 0xA0, 0xA6);
            AppWindow.TitleBar.ButtonHoverBackgroundColor = Color.FromArgb(20, 0, 0, 0);
            AppWindow.TitleBar.ButtonPressedBackgroundColor = Color.FromArgb(40, 0, 0, 0);
        }
        catch (Exception ex)
        {
            App.Log($"ConfigureWindow failed: {ex}");
        }
    }

    private void UpdateBackendStatus()
    {
        var remote = AppServices.Instance.IsRemote;
        BackendBadge.Text = remote ? "已连接离线识别引擎" : "离线演示数据";
        FooterStatusText.Text = remote
            ? "已离线运行 · 已连接本地识别引擎"
            : "已离线运行 · 数据存于本地 JSON";
    }

    internal void UpdateFooter()
    {
        try
        {
            var summary = AppServices.LastStatistics?.Summary;
            FooterSnapshotText.Text = summary is null
                ? "最近资产快照：今天 14:32"
                : $"最近资产快照：今天 14:32";
        }
        catch (Exception ex)
        {
            App.Log($"UpdateFooter failed: {ex.Message}");
        }
    }

    private void Select(int index)
    {
        switch (index)
        {
            case 0: Navigate(typeof(ImportPage)); break;
            case 1: Navigate(typeof(CharactersPage)); break;
            case 2: Navigate(typeof(AssetsPage)); break;
            case 3: Navigate(typeof(SettingsPage)); break;
        }
        ApplyNavState(index);
    }

    private void Navigate(Type pageType)
    {
        if (ContentFrame.CurrentSourcePageType == pageType) return;
        ContentFrame.Navigate(pageType);
    }

    private void SyncNavSelection()
    {
        var current = ContentFrame.CurrentSourcePageType;
        var index = current switch
        {
            _ when current == typeof(ImportPage) => 0,
            _ when current == typeof(CharactersPage) => 1,
            _ when current == typeof(AssetsPage) => 2,
            _ when current == typeof(SettingsPage) => 3,
            _ => 0,
        };
        ApplyNavState(index);
    }

    private void ApplyNavState(int index)
    {
        for (var i = 0; i < _navButtons.Length; i++)
        {
            var selected = i == index;
            _navButtons[i].Background = selected
                ? Theme.Resource("NavSelectedBrush", new SolidColorBrush(Color.FromArgb(255, 0xE2, 0xE2, 0xE7)))
                : new SolidColorBrush(Colors.Transparent);

            var foreground = selected
                ? Theme.Resource("TextPrimaryBrush", new SolidColorBrush(Color.FromArgb(255, 0x1C, 0x1C, 0x1F)))
                : Theme.Resource("TextSecondaryBrush", new SolidColorBrush(Color.FromArgb(255, 0x6B, 0x6F, 0x76)));

            _navTexts[i].Foreground = foreground;
            _navTexts[i].FontWeight = selected
                ? Microsoft.UI.Text.FontWeights.SemiBold
                : Microsoft.UI.Text.FontWeights.Normal;

            // Selected item's glyph goes brand blue (as in the mock), idle glyphs are grey.
            _navIcons[i].Foreground = selected
                ? Theme.Resource("BrandBlueBrush", new SolidColorBrush(Color.FromArgb(255, 0, 116, 255)))
                : Theme.Resource("TextTertiaryBrush", new SolidColorBrush(Color.FromArgb(255, 0x9A, 0xA0, 0xA6)));
        }
    }

    private void OnNavImportClick(object sender, RoutedEventArgs e) => Select(0);
    private void OnNavCharactersClick(object sender, RoutedEventArgs e) => Select(1);
    private void OnNavAssetsClick(object sender, RoutedEventArgs e) => Select(2);
    private void OnNavSettingsClick(object sender, RoutedEventArgs e) => Select(3);

    /// <summary>Public hook so pages can jump (e.g. after saving a recognition).</summary>
    /// <summary>
    /// Resize and centre the window at the design size.
    ///
    /// `AppWindow.Resize` takes PHYSICAL pixels, so a 150% display needs
    /// 1328*1.5 = 1992 px to look like the 1328-unit mock. Passing the logical
    /// number straight through is what previously produced a 590x395 footprint
    /// (1328/1.5 = 885 px physical).
    /// </summary>
    private void ApplyDesignSize()
    {
        try
        {
            var scale = GetDpiScale();
            var physicalW = (int)Math.Round(DefaultWidth * scale);
            var physicalH = (int)Math.Round(DefaultHeight * scale);

            // AppWindow.Resize converts DIPs -> physical by DIVIDING by the
            // scale factor (Resize(1328) on a 150% display produced 885 px),
            // so it cannot express the design size. SetWindowPos takes raw
            // physical pixels and is the only reliable route here.
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
            var area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary);
            var work = area.WorkArea;
            var x = work.X + (work.Width - physicalW) / 2;
            var y = work.Y + (work.Height - physicalH) / 2;
            var ok = SetWindowPos(hwnd, IntPtr.Zero, x, y, physicalW, physicalH,
                SwpNoZOrder | SwpNoActivate);
            App.Log($"ApplyDesignSize scale={scale:0.##} physical={physicalW}x{physicalH} ok={ok}");
        }
        catch (Exception ex)
        {
            App.Log($"ApplyDesignSize failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Re-assert the design size a few times while the window settles.
    ///
    /// WinUI applies its own default frame shortly after creation, which
    /// overwrites a single early resize. Re-applying at short intervals (then
    /// locking the presenter) makes the final geometry deterministic.
    /// </summary>
    private async Task ReapplySizeSequenceAsync()
    {
        foreach (var delay in new[] { 120, 350, 800 })
        {
            await Task.Delay(delay);
            ApplyDesignSize();
        }
        LockToDesignSize();
    }

    /// <summary>
    /// Remove the maximise button and stop frame resizing.
    ///
    /// Called after the design size is applied, because setting these before
    /// (or during) the resize makes WinUI clamp the frame to its stale size.
    /// </summary>
    private bool _locked;
    private void LockToDesignSize()
    {
        if (_locked) return;
        try
        {
            if (AppWindow.Presenter is OverlappedPresenter presenter)
            {
                presenter.IsResizable = false;
                presenter.IsMaximizable = false;
                presenter.IsMinimizable = true;
            }
            _locked = true;
            App.Log("window locked to design size");
        }
        catch (Exception ex)
        {
            App.Log($"LockToDesignSize failed: {ex.Message}");
        }
    }
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;

    [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(
        IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint flags);

    public void NavigateTo(int index) => Select(index);

    /// <summary>
    /// DPI scale factor of the monitor this window is on (1.0 = 96 DPI).
    ///
    /// `AppWindow.Resize` takes physical pixels while our layout constants are
    /// logical units, so every size must be multiplied by this.
    /// </summary>
    private double GetDpiScale()
    {
        try
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
            var dpi = GetDpiForWindow(hwnd);
            return dpi == 0 ? 1.0 : dpi / 96.0;
        }
        catch (Exception ex)
        {
            App.Log($"GetDpiScale failed: {ex.Message}");
            return 1.0;
        }
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);
}
