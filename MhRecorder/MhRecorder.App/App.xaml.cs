using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using MhRecorder.App.Services;
using Windows.UI;

namespace MhRecorder.App;

public partial class App : Application
{
    private static MainWindow? _window;

    /// <summary>The single top-level window, for pickers and page-to-shell navigation.</summary>
    public static MainWindow? MainWindowInstance => _window;

    public App()
    {
        // The design is light-theme only; never follow the system dark theme.
        RequestedTheme = ApplicationTheme.Light;

        InitializeComponent();

        // Never let one bad layout/bind take the whole app down: log and keep going.
        UnhandledException += (_, e) =>
        {
            Log($"UnhandledException: {e.Exception}");
            e.Handled = true;
        };
        AppDomain.CurrentDomain.UnhandledException += (_, e) => Log($"AppDomain: {e.ExceptionObject}");

        UiThread.Initialize();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        AppHost.Initialize();

        _window = new MainWindow();
        _window.Activate();
        Log("OnLaunched: window activated");
    }
    /// <summary>Append to %LOCALAPPDATA%\MhRecorder\ui.log (never throws).</summary>
    public static void Log(string message)
    {
        try
        {
            var dir = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "MhRecorder");
            Directory.CreateDirectory(dir);
            File.AppendAllText(Path.Combine(dir, "ui.log"), $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff}] {message}\n");
        }
        catch
        {
            // logging must never break the app
        }
    }
}

/// <summary>Single place every background continuation uses to hop back to the UI thread.</summary>
public static class UiThread
{
    private static DispatcherQueue? _dispatcher;

    public static void Initialize() => _dispatcher = DispatcherQueue.GetForCurrentThread();

    public static bool HasThreadAccess => _dispatcher?.HasThreadAccess ?? false;

    public static void OnUi(Action action)
    {
        var dispatcher = _dispatcher;
        if (dispatcher is null)
        {
            action();
            return;
        }
        if (dispatcher.HasThreadAccess) action();
        else dispatcher.TryEnqueue(() => action());
    }
}

public static class Theme
{
    public static Color BrandBlue => Color.FromArgb(255, 0, 116, 255);
    public static SolidColorBrush Brush(Color color) => new(color);

    public static T Resource<T>(string key, T fallback) where T : class
    {
        try
        {
            if (Application.Current.Resources.TryGetValue(key, out var value) && value is T typed)
                return typed;
        }
        catch
        {
            // ignore
        }
        return fallback;
    }
}

