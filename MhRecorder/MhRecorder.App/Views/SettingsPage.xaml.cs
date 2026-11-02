using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using MhRecorder.App.Services;
using MhRecorder.Core.Models;
using MhRecorder.Core;
using MhRecorder.Core.Services;
using System.Text.Json;
using Orientation = Microsoft.UI.Xaml.Controls.Orientation;
using VerticalAlignment = Microsoft.UI.Xaml.VerticalAlignment;

namespace MhRecorder.App.Views;

/// <summary>Page 4 — 设置: 识别 / 数据存储 / 关于 sections.</summary>
public sealed partial class SettingsPage : Page
{
    private Settings _settings = new();
    private bool _suppressEvents;

    public SettingsPage()
    {
        InitializeComponent();
        Loaded += OnLoaded;
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            _settings = await AppServices.Instance.GetSettingsAsync();
            ApplySettings(_settings);
            await LoadEngineInfoAsync();
            UpdateChangelogSummary();
        }
        catch (Exception ex)
        {
            App.Log($"SettingsPage.OnLoaded failed: {ex}");
        }
    }

    /// <summary>
    /// Ask the backend which recognition plugin is active and show its details.
    ///
    /// The recogniser is modelled as a pluggable unit, so the UI reports the
    /// concrete engine id/runtime instead of a vague "离线 OCR" label — that is
    /// what makes a recognition result attributable.
    /// </summary>
    private async Task LoadEngineInfoAsync()
    {
        try
        {
            var info = await AppServices.Instance.GetEngineInfoAsync();
            if (info is null || info.Value.ValueKind != JsonValueKind.Object)
            {
                EngineText.Text = "未连接（使用内置演示引擎）";
                EngineVersionText.Text = "—";
                PluginDetailText.Text = "后端未连接，无法读取插件信息";
                return;
            }

            // The `version` payload nests the plugin descriptor under "engine".
            var engine = info.Value.TryGetProperty("engine", out var e) &&
                         e.ValueKind == JsonValueKind.Object
                ? e
                : info.Value;

            string Str(string key) =>
                engine.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
                    ? v.GetString() ?? ""
                    : "";

            var active = engine.TryGetProperty("active", out var act) &&
                         act.ValueKind == JsonValueKind.True;
            var name = Str("name");
            EngineText.Text = active
                ? $"{(name.Length > 0 ? name : "OCR")} · 已加载"
                : "OCR · 未加载";
            EngineVersionText.Text = Str("version") is { Length: > 0 } ver ? ver : "—";

            var models = engine.TryGetProperty("models", out var arr) &&
                         arr.ValueKind == JsonValueKind.Array
                ? string.Join("、", arr.EnumerateArray().Select(x => x.GetString()))
                : "—";
            var runtime = Str("runtime");
            var id = Str("id");
            PluginDetailText.Text = $"{id}\n运行时：{runtime}\n模型：{models}";
        }
        catch (Exception ex)
        {
            App.Log($"SettingsPage.LoadEngineInfo failed: {ex}");
            PluginDetailText.Text = "读取失败：" + ex.Message;
        }
    }

    /// <summary>
    /// Render the in-app changelog into a modal layer. Every entry records what
    /// changed for recognition or the UI, so a user can tell which build produced
    /// a given result without digging through files.
    ///
    /// It used to be an inline card at the bottom of the page: four releases of
    /// history pushed the rest of the settings off-screen and made the page look
    /// like a document rather than a settings list. A dialog keeps the summary
    /// ("最新 0.9.3 · 共 4 个版本") visible and the wall of text one click away.
    /// </summary>
    private void UpdateChangelogSummary()
    {
        if (ChangelogSummaryText is null) return;
        var entries = Changelog.Entries;
        ChangelogSummaryText.Text = entries.Length == 0
            ? "—"
            : $"最新 {entries[0].Version} · 共 {entries.Length} 个版本";
    }

    private async void OnViewChangelogClick(object sender, RoutedEventArgs e)
    {
        try
        {
            await BuildChangelogDialog().ShowAsync();
        }
        catch (Exception ex)
        {
            App.Log($"OnViewChangelogClick failed: {ex}");
            if (StatusText is not null)
            {
                StatusText.Text = "打开更新记录失败：" + ex.Message;
            }
        }
    }

    private ContentDialog BuildChangelogDialog()
    {
        var res = Application.Current.Resources;
        var primary = (Brush)res["TextPrimaryBrush"];
        var secondary = (Brush)res["TextSecondaryBrush"];
        var tertiary = (Brush)res["TextTertiaryBrush"];
        var chipBackground = (Brush)res["ChipBackgroundBrush"];
        var brandBlue = (Brush)res["BrandBlueBrush"];

        var body = new StackPanel { Spacing = 16, Width = 470 };

        // ---- summary strip: which build am I on, and is it current? ----
        var summaryRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            VerticalAlignment = VerticalAlignment.Center,
        };
        summaryRow.Children.Add(new Microsoft.UI.Xaml.Shapes.Ellipse
        {
            Width = 7,
            Height = 7,
            Fill = (Brush)res["SuccessGreenBrush"],
            VerticalAlignment = VerticalAlignment.Center,
        });
        summaryRow.Children.Add(new TextBlock
        {
            Text = $"当前版本 {ProtocolConstants.AppVersion}（本地离线版）· 已是最新",
            FontSize = 12.5,
            Foreground = secondary,
            VerticalAlignment = VerticalAlignment.Center,
        });
        body.Children.Add(new Border
        {
            Background = (Brush)res["SubtleFillBrush"],
            CornerRadius = new CornerRadius(10),
            Padding = new Thickness(14, 10, 14, 10),
            Child = summaryRow,
        });

        // ---- one block per release, newest first ----
        var list = new StackPanel { Spacing = 20, Padding = new Thickness(0, 2, 4, 2) };
        foreach (var entry in Changelog.Entries)
        {
            var block = new StackPanel { Spacing = 7 };

            var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            header.Children.Add(new TextBlock
            {
                Text = entry.Version,
                FontSize = 14,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                Foreground = primary,
                VerticalAlignment = VerticalAlignment.Center,
            });
            if (entry == Changelog.Entries[0])
            {
                header.Children.Add(new Border
                {
                    Background = brandBlue,
                    CornerRadius = new CornerRadius(5),
                    Padding = new Thickness(6, 1, 6, 2),
                    VerticalAlignment = VerticalAlignment.Center,
                    Child = new TextBlock
                    {
                        Text = "当前",
                        FontSize = 10.5,
                        Foreground = new SolidColorBrush(Microsoft.UI.Colors.White),
                    },
                });
            }
            header.Children.Add(new TextBlock
            {
                Text = entry.Date,
                FontSize = 12,
                Foreground = tertiary,
                VerticalAlignment = VerticalAlignment.Center,
            });
            block.Children.Add(header);

            var changes = new StackPanel { Spacing = 5 };
            foreach (var line in entry.Changes)
            {
                changes.Children.Add(new TextBlock
                {
                    Text = "· " + line,
                    FontSize = 12.5,
                    LineHeight = 19,
                    TextWrapping = TextWrapping.Wrap,
                    Foreground = secondary,
                });
            }
            block.Children.Add(changes);
            list.Children.Add(block);
        }

        body.Children.Add(new ScrollViewer
        {
            Content = list,
            // 360 keeps the whole dialog under ContentDialogMaxHeight (640) so the
            // list is clipped by the scroll viewport, not by the card edge.
            MaxHeight = 360,
            VerticalScrollBarVisibility = ScrollBarVisibility.Visible,
            VerticalScrollMode = ScrollMode.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            HorizontalScrollMode = ScrollMode.Disabled,
            Margin = new Thickness(0, 0, -2, 0),
        });

        var dialog = new ContentDialog
        {
            // A TextBlock title (instead of a string) keeps the dialog on the
            // app's display font at a size that matches the settings page.
            Title = new TextBlock
            {
                Text = "版本更新记录",
                FontFamily = (FontFamily)res["UiDisplayFontFamily"],
                FontSize = 17,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                Foreground = primary,
            },
            Content = body,
            CloseButtonText = "关闭",
            // NOT ContentDialogButton.Close: making the close button the *default*
            // button makes WinUI paint it with the OS accent colour (#496925 on a
            // green-accent desktop) and ignore CloseButtonStyle, which clashed
            // hard with the app's blue/white chrome. Esc still closes the dialog.
            DefaultButton = ContentDialogButton.None,
            CloseButtonStyle = (Style)res["SecondaryButtonStyle"],
            XamlRoot = XamlRoot,
            RequestedTheme = ElementTheme.Light,
            Background = (Brush)res["CardBackgroundBrush"],
            CornerRadius = new CornerRadius(16),
            Padding = new Thickness(24, 22, 24, 20),
        };
        // Dialog sizing lives in template ThemeResources; overriding them on the
        // instance is the only supported way to widen a ContentDialog.
        dialog.Resources["ContentDialogMaxWidth"] = 560.0;
        dialog.Resources["ContentDialogMinWidth"] = 520.0;
        dialog.Resources["ContentDialogMaxHeight"] = 640.0;
        return dialog;
    }

    private void ApplySettings(Settings settings)
    {
        _suppressEvents = true;
        try
        {
            EditableBeforeSaveToggle.IsOn = settings.EditableBeforeSave;
            AutoNavigateToggle.IsOn = settings.AutoNavigateAfterSave;
            DataDirText.Text = settings.DataDir;
            VersionText.Text = $"{ProtocolConstants.AppVersion}（本地版）";
            EngineText.Text = AppServices.Instance.IsRemote
                ? "离线 OCR · 不做任何上传（已连接引擎）"
                : "离线 OCR · 不做任何上传";
        }
        finally
        {
            _suppressEvents = false;
        }
    }

    private async void OnEditableToggleChanged(object sender, RoutedEventArgs e)
    {
        if (_suppressEvents) return;
        _settings.EditableBeforeSave = EditableBeforeSaveToggle.IsOn;
        await PersistAsync();
    }

    private async void OnAutoNavigateToggleChanged(object sender, RoutedEventArgs e)
    {
        if (_suppressEvents) return;
        _settings.AutoNavigateAfterSave = AutoNavigateToggle.IsOn;
        await PersistAsync();
    }

    private async Task PersistAsync()
    {
        try
        {
            await AppServices.Instance.SetSettingsAsync(_settings);
            // StatusText may be detached if the user navigated away while the
            // save was in flight; writing it then throws NullReference.
            if (StatusText is not null)
            {
                StatusText.Text = $"设置已保存 · {DateTime.Now:HH:mm:ss}";
            }
        }
        catch (Exception ex)
        {
            App.Log($"PersistAsync failed: {ex}");
            if (StatusText is not null)
            {
                StatusText.Text = "保存设置失败：" + ex.Message;
            }
        }
    }

    private async void OnExportJsonClick(object sender, RoutedEventArgs e)
    {
        try
        {
            var picker = new Windows.Storage.Pickers.FileSavePicker
            {
                SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.DocumentsLibrary,
                SuggestedFileName = $"mhrecorder-export-{DateTime.Now:yyyyMMdd-HHmm}",
            };
            picker.FileTypeChoices.Add("JSON 数据", new List<string> { ".json" });

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindowInstance);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var file = await picker.PickSaveFileAsync();
            if (file is null)
            {
                StatusText.Text = "已取消导出";
                return;
            }

            var result = await AppServices.Instance.ExportAsync(ExportFormat.Json, file.Path);
            StatusText.Text = result is null
                ? "导出失败"
                : $"已导出 {result.Bytes:N0} 字节 → {result.Path}";
        }
        catch (Exception ex)
        {
            App.Log($"OnExportJsonClick failed: {ex}");
            StatusText.Text = "导出失败：" + ex.Message;
        }
    }

    private async void OnClearDataClick(object sender, RoutedEventArgs e)
    {
        try
        {
            var dialog = new ContentDialog
            {
                Title = "清空本地数据",
                Content = "这会删除全部角色与历史记录，且无法恢复。确定继续吗？",
                PrimaryButtonText = "清空",
                CloseButtonText = "取消",
                DefaultButton = ContentDialogButton.Close,
                XamlRoot = XamlRoot,
            };

            var choice = await dialog.ShowAsync();
            if (choice != ContentDialogResult.Primary) return;

            await AppServices.Instance.ClearDataAsync();
            StatusText.Text = "本地数据已清空";
        }
        catch (Exception ex)
        {
            App.Log($"OnClearDataClick failed: {ex}");
            StatusText.Text = "清空失败：" + ex.Message;
        }
    }

    private async void OnCheckUpdateClick(object sender, RoutedEventArgs e)
    {
        try
        {
            StatusText.Text = "检查中…";

            // This build is offline-first: there is no update server, so the
            // honest answer is "you are on the newest local build". We still
            // surface the engine plugin's own updatability so the user knows
            // whether a newer recogniser could be swapped in.
            var info = await AppServices.Instance.GetEngineInfoAsync();
            string updatable = "不可在线更新";
            string engineId = "—";
            if (info is { ValueKind: JsonValueKind.Object } obj)
            {
                var engine = obj.TryGetProperty("engine", out var engineEl) &&
                             engineEl.ValueKind == JsonValueKind.Object
                    ? engineEl
                    : obj;
                if (engine.TryGetProperty("updatable", out var up) &&
                    up.ValueKind == JsonValueKind.True)
                {
                    updatable = "支持在线更新";
                }
                if (engine.TryGetProperty("id", out var idEl) &&
                    idEl.ValueKind == JsonValueKind.String)
                {
                    engineId = idEl.GetString() ?? "—";
                }
            }

            StatusText.Text = AppServices.Instance.IsRemote
                ? $"已是最新版本 {ProtocolConstants.AppVersion}（本地离线版）· 识别插件 {engineId} {updatable}"
                : $"当前为演示模式，版本 {ProtocolConstants.AppVersion}· 识别插件未连接";
        }
        catch (Exception ex)
        {
            App.Log($"OnCheckUpdateClick failed: {ex}");
        }
    }
}
