using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using MhRecorder.App.Services;
using MhRecorder.App.Views;
using MhRecorder.Core.Infrastructure;
using MhRecorder.Core.ViewModels;
using Windows.Graphics;
using Windows.UI;

namespace MhRecorder.App;

public sealed partial class MainWindow : Window
{
    // Size taken from the design mock: the window body inside design/page_01.png
    // measures 2766 x 1850 px at 2.0836 px/pt, i.e. 1328 x 888 logical units.
    private const int DesignWidth = 1328;
    private const int DesignHeight = 888;

    // Below this the three-column pages stop being readable, so it is a hard
    // floor rather than a suggestion. 1024x700 still fits a 1366x768 laptop
    // with the taskbar and window chrome accounted for.
    private const int MinWidth = 1024;
    private const int MinHeight = 700;

    // Sidebar collapses to icons so the content keeps its width on small screens.
    private const double CompactBreakpoint = 1180;
    private const double SidebarWidth = 254;
    private const double SidebarCompactWidth = 76;

    private Button[] _navButtons = Array.Empty<Button>();
    private TextBlock[] _navTexts = Array.Empty<TextBlock>();
    private TextBlock[] _navIcons = Array.Empty<TextBlock>();

    public MainViewModel ViewModel { get; } = AppHost.Shell;

    public MainWindow()
    {
        InitializeComponent();

        Title = "梦幻手游记录助手";
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        ConfigureWindow();
        ApplyInitialSize();

        _navButtons = new[] { NavImport, NavCharacters, NavAssets, NavSettings };
        _navTexts = new[] { NavImportText, NavCharactersText, NavAssetsText, NavSettingsText };
        _navIcons = new[] { NavImportIcon, NavCharactersIcon, NavAssetsIcon, NavSettingsIcon };

        // The shell decides which page is current; the frame follows it.
        ViewModel.NavigationRequested += (_, page) => NavigateFrame(page);
        ViewModel.PropertyChanged += (_, e) =>
        {
            switch (e.PropertyName)
            {
                case nameof(MainViewModel.Current):
                    ApplyNavState();
                    break;
                case nameof(MainViewModel.BackendBadge):
                    BackendBadge.Text = ViewModel.BackendBadge;
                    break;
                case nameof(MainViewModel.FooterStatus):
                    FooterStatusText.Text = ViewModel.FooterStatus;
                    break;
                case nameof(MainViewModel.FooterSnapshot):
                    FooterSnapshotText.Text = ViewModel.FooterSnapshot;
                    break;
            }
        };

        BackendBadge.Text = ViewModel.BackendBadge;
        FooterStatusText.Text = ViewModel.FooterStatus;
        FooterSnapshotText.Text = ViewModel.FooterSnapshot;

        RootLayout.SizeChanged += (_, e) => ApplyResponsiveLayout(e.NewSize.Width);

        NavigateFrame(AppPage.Import);
        ApplyNavState();

        AppWindow.Closing += (_, _) => App.Log("MainWindow closing");
    }

    // ---------------- window sizing ----------------

    private void ConfigureWindow()
    {
        try
        {
            // The design size is a starting point, not a cage: forcing 1328x888
            // made the app unusable on 1366x768 and on high-DPI laptops, where the
            // window came out larger than the work area. Resizing and maximise
            // are enabled; the minimum below is what protects the layout.
            if (AppWindow.Presenter is OverlappedPresenter presenter)
            {
                presenter.IsResizable = true;
                presenter.IsMaximizable = true;
                presenter.IsMinimizable = true;
            }

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

    /// <summary>
    /// Start at the design size when the screen allows it, otherwise at the
    /// largest size that still fits the work area — never larger than the screen.
    /// </summary>
    private void ApplyInitialSize()
    {
        try
        {
            var scale = GetDpiScale();
            var work = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;

            // WorkArea is in physical pixels; the design constants are logical.
            var availableW = work.Width / scale;
            var availableH = work.Height / scale;

            var width = Math.Min(DesignWidth, Math.Max(MinWidth, availableW - 40));
            var height = Math.Min(DesignHeight, Math.Max(MinHeight, availableH - 40));

            var physicalW = (int)Math.Round(width * scale);
            var physicalH = (int)Math.Round(height * scale);
            var x = work.X + (work.Width - physicalW) / 2;
            var y = work.Y + (work.Height - physicalH) / 2;

            // SetWindowPos takes raw physical pixels; AppWindow.Resize divides by
            // the scale factor and cannot express this size reliably.
            SetWindowPos(WinRT.Interop.WindowNative.GetWindowHandle(this), IntPtr.Zero,
                x, y, physicalW, physicalH, SwpNoZOrder | SwpNoActivate);

            // Minimum size is enforced by the presenter, so the user cannot drag
            // the layout into a state that no longer fits its content. These are
            // applied after the size is set: doing it before made WinUI clamp the
            // frame to its stale default.
            if (AppWindow.Presenter is OverlappedPresenter presenter)
            {
                presenter.PreferredMinimumWidth = (int)Math.Round(MinWidth * scale);
                presenter.PreferredMinimumHeight = (int)Math.Round(MinHeight * scale);
            }

            App.Log($"window size {width}x{height} (work {availableW:0}x{availableH:0}, scale {scale:0.##})");
        }
        catch (Exception ex)
        {
            App.Log($"ApplyInitialSize failed: {ex.Message}");
        }
    }

    /// <summary>Collapse the sidebar to icons when the content needs the width.</summary>
    private void ApplyResponsiveLayout(double width)
    {
        var compact = width < CompactBreakpoint;
        if (compact == ViewModel.IsSidebarCompact) return;

        ViewModel.IsSidebarCompact = compact;
        SidebarColumn.Width = new GridLength(compact ? SidebarCompactWidth : SidebarWidth);
        SidebarCaption.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;

        foreach (var text in _navTexts)
            text.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;

        NavImport.Padding = compact ? new Thickness(0) : new Thickness(14, 0, 14, 0);
        NavCharacters.Padding = NavImport.Padding;
        NavAssets.Padding = NavImport.Padding;
        NavSettings.Padding = NavImport.Padding;

        foreach (var button in _navButtons)
            button.HorizontalContentAlignment =
                compact ? HorizontalAlignment.Center : HorizontalAlignment.Stretch;
    }

    // ---------------- navigation ----------------

    private void NavigateFrame(AppPage page)
    {
        var target = page switch
        {
            AppPage.Characters => typeof(CharactersPage),
            AppPage.Assets => typeof(AssetsPage),
            AppPage.Settings => typeof(SettingsPage),
            _ => typeof(ImportPage),
        };

        if (ContentFrame.CurrentSourcePageType != target) ContentFrame.Navigate(target);
        ApplyNavState();
    }

    /// <summary>Public hook for code that only knows the numeric index.</summary>
    public void NavigateTo(int index) => ViewModel.Select((AppPage)index);

    private void ApplyNavState()
    {
        var current = ViewModel.Current;
        for (var i = 0; i < _navButtons.Length; i++)
        {
            var selected = (AppPage)i == current;
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

    private void OnNavImportClick(object sender, RoutedEventArgs e) => ViewModel.Select(AppPage.Import);
    private void OnNavCharactersClick(object sender, RoutedEventArgs e) => ViewModel.Select(AppPage.Characters);
    private void OnNavAssetsClick(object sender, RoutedEventArgs e) => ViewModel.Select(AppPage.Assets);
    private void OnNavSettingsClick(object sender, RoutedEventArgs e) => ViewModel.Select(AppPage.Settings);

    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;

    [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(
        IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint flags);

    /// <summary>DPI scale factor of the monitor this window is on (1.0 = 96 DPI).</summary>
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
