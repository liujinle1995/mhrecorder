using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using MhRecorder.App.Services;
using MhRecorder.Core;
using MhRecorder.Core.Models;
using MhRecorder.Core.ViewModels;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage.Pickers;
using Windows.Storage.Streams;
using Windows.System;
using Windows.UI;

namespace MhRecorder.App.Views;

/// <summary>Page 1 — 录入识别: drag/drop zone on the left, editable result on the right.</summary>
public sealed partial class ImportPage : Page
{
    private RecognitionResult? _result;
    private List<RecognizeImage> _images = new();

    /// <summary>Bytes of the image currently shown in the left preview pane.</summary>
    private byte[]? _previewBytes;

    /// <summary>Guards against a second recognition starting while one is running.</summary>
    private bool _busy;

    public ImportPage()
    {
        InitializeComponent();
        Loaded += OnLoaded;

        // The button says Ctrl+V, so the shortcut has to work too. Scoped to the
        // page: it only fires while something on this page has focus.
        var pasteAccelerator = new KeyboardAccelerator
        {
            Key = VirtualKey.V,
            Modifiers = VirtualKeyModifiers.Control,
        };
        pasteAccelerator.Invoked += (_, args) =>
        {
            args.Handled = true;
            OnPasteClick(this, new RoutedEventArgs());
        };
        KeyboardAccelerators.Add(pasteAccelerator);
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            // With a live backend there are no canned images to send: an empty
            // payload is correctly rejected by the recogniser. Only the in-memory
            // demo service can synthesise a result from nothing.
            if (_result is null)
            {
                if (AppServices.Instance.IsRemote)
                {
                    ApplyResult(null, 0);
                }
                else
                {
                    await ShowDemoResultAsync();
                }
            }
        }
        catch (Exception ex)
        {
            App.Log($"ImportPage.OnLoaded failed: {ex}");
        }
    }

    private async Task ShowDemoResultAsync()
    {
        var images = new List<RecognizeImage>
        {
            new() { Name = "character_attributes.png", Data = string.Empty },
            new() { Name = "inventory.png", Data = string.Empty },
        };
        var result = await AppServices.Instance.RecognizeAsync(images);
        ApplyResult(result, images.Count);
    }

    private void ApplyResult(RecognitionResult? result, int imageCount)
    {
        _result = result;
        if (result is null)
        {
            // Cancels any in-flight picker load for the previous result.
            _ownerPickerGeneration++;
            OwnerCombo.Items.Clear();
            OwnerCombo.SelectedIndex = -1;
            OwnerPickerPanel.Visibility = Visibility.Collapsed;
            NoOwnerHintText.Visibility = Visibility.Collapsed;
            IdentityEvidenceText.Visibility = Visibility.Collapsed;
            AppearanceText.Visibility = Visibility.Collapsed;
            AppearanceHintText.Visibility = Visibility.Collapsed;
            EmptyResultState.Visibility = Visibility.Visible;
            ResultScroll.Visibility = Visibility.Collapsed;
            return;
        }

        EmptyResultState.Visibility = Visibility.Collapsed;
        ResultScroll.Visibility = Visibility.Visible;

        // "未识别角色" when nothing was resolved at all: calling it 未命名角色
        // implies we know it is someone, which for a bare 包裹 we do not.
        var nickname = result.Character.Nickname
            ?? (string.IsNullOrEmpty(result.CharacterId) ? "未识别角色" : "未命名角色");
        NicknameText.Text = nickname;
        LevelChipText.Text = result.Character.Level is { } lvl ? $"Lv.{lvl}" : "Lv.?";

        var screens = result.Screens.Count > 0
            ? string.Join(" + ", result.Screens.Select(ScreenLabel))
            : "人物属性";
        var screenCount = result.ScreenshotCount > 0 ? result.ScreenshotCount : imageCount;
        IdentityLineText.Text = Fmt.IdLine(result.Character.Level, result.Character.GameId) is { Length: > 0 } id
            ? $"{id} · {screens}"
            : $"游戏 ID 待确认 · {screens}";

        SourceChipText.Text = $"来源：{screens} · {screenCount} 张截图";

        // "why this character" — only present when the identity came from
        // evidence rather than from the panel itself.
        var evidence = result.IdentityEvidence ?? string.Empty;

        // 冲突：面板上的身份字段与视觉证据指向不同的人。后端已按游戏ID 定案，
        // 这里把矛盾摆出来 —— 视觉模型退化时能一眼看见，而不是悄悄错下去。
        if (result.Conflicts.Count > 0)
        {
            evidence = string.IsNullOrWhiteSpace(evidence)
                ? string.Join(Environment.NewLine, result.Conflicts)
                : evidence + Environment.NewLine + string.Join(Environment.NewLine, result.Conflicts);
        }

        // 候选：证据不足时不再猜，把排序后的候选连同各自依据交给你选。
        if (result.Candidates.Count > 0 && string.IsNullOrEmpty(result.CharacterId))
        {
            var list = string.Join(Environment.NewLine,
                result.Candidates.Select((c, i) => $"  {i + 1}. {c.Describe()}"));
            evidence = string.IsNullOrWhiteSpace(evidence)
                ? "候选：" + Environment.NewLine + list
                : evidence + Environment.NewLine + "候选：" + Environment.NewLine + list;
        }

        IdentityEvidenceText.Text = evidence;
        IdentityEvidenceText.Visibility = string.IsNullOrWhiteSpace(evidence)
            ? Visibility.Collapsed
            : Visibility.Visible;

        // 发色 / 武器 as recorded. Shown next to the evidence because it is the
        // weaker, appearance-based half of the identity: what future captures
        // get matched on, and the thing to check when a link looks wrong.
        if (result.Appearance is { } look)
        {
            AppearanceText.Text = $"外观特征：{look.Describe()}";
            AppearanceText.Visibility = Visibility.Visible;
        }
        else
        {
            AppearanceText.Visibility = Visibility.Collapsed;
        }

        _ = RefreshOwnerPickerAsync(result);

        ConfidenceValueText.Text = $"{result.Confidence * 100:0}%";

        // Identity at a glance: show the portrait the backend cropped straight
        // out of the screenshot. The old code special-cased one nickname and
        // loaded a bundled PNG, which meant every other character got a plain
        // initial — that is the "头像没有从截图裁切出来" complaint.
        var initial = Fmt.AvatarInitial(nickname);
        AvatarInitialText.Text = initial;
        AvatarImage.Source = null;
        AvatarImage.Visibility = Visibility.Collapsed;

        if (!string.IsNullOrEmpty(result.AvatarPngBase64))
        {
            try
            {
                var bytes = Convert.FromBase64String(result.AvatarPngBase64);
                // SetSourceAsync would need an async context; the decoder also
                // works synchronously through a stream, which keeps ApplyResult
                // callable from both sync and async paths.
                var bitmap = new BitmapImage(new Uri(
                    "data:image/png;base64," + result.AvatarPngBase64));
                AvatarImage.Source = bitmap;
                AvatarImage.Visibility = Visibility.Visible;
                AvatarInitialText.Text = string.Empty;
                var _ = bytes;
            }
            catch (Exception ex)
            {
                App.Log($"avatar decode failed: {ex.Message}");
            }
        }

        var stats = result.Stats;
        HpBar.Label = ValueText(stats.Hp);
        // A full bar next to "—" reads as "full HP": keep the track empty when
        // the capture simply did not show the value (a 包裹 panel never does).
        HpBar.Ratio = stats.Hp is null ? 0.0 : 1.0;
        MpBar.Label = ValueText(stats.Mp);
        MpBar.Ratio = stats.Mp is null ? 0.0 : 1.0;

        PhysicalDamageText.Text = ValueText(stats.PhysicalDamage);
        MagicDamageText.Text = ValueText(stats.MagicDamage);
        PhysicalDefenseText.Text = ValueText(stats.PhysicalDefense);
        MagicDefenseText.Text = ValueText(stats.MagicDefense);
        SpeedText.Text = ValueText(stats.Speed);
        HealingText.Text = ValueText(stats.HealingPower);

        GoldValueText.Text = ValueText(result.Currencies.Gold);
        SilverValueText.Text = ValueText(result.Currencies.Silver);
    }

    /// <summary>
    /// A field that was not read must render as "—", never as 0. "金币 0" reads
    /// as "this character has no gold", which is a different claim from "this
    /// capture did not show the 包裹 panel".
    /// </summary>
    private static string ValueText(AttributeValue? value) =>
        value is null ? "—" : Fmt.Number(value.Value);

    private static string ScreenLabel(ScreenKind kind) => kind switch
    {
        ScreenKind.CharacterAttributes => "人物属性",
        ScreenKind.Inventory => "背包",
        _ => "未识别",
    };

    // ---------------- Owner picker ----------------

    /// <summary>One character the user can attribute this capture to.</summary>
    private sealed record OwnerOption(string Id, string Label)
    {
        public override string ToString() => Label;
    }

    /// <summary>Guards against a slow character load landing on a newer result.</summary>
    private int _ownerPickerGeneration;

    /// <summary>
    /// Show the owner picker when the recogniser could not resolve who this
    /// capture belongs to.
    ///
    /// A 包裹 screenshot carries no 游戏ID and no nickname at all, so there is
    /// nothing to read — the first one has to be attributed by the user, by
    /// picking from the characters already in the library. Each row carries
    /// 昵称 + 游戏ID, which is what tells two same-named characters apart.
    /// </summary>
    private async Task RefreshOwnerPickerAsync(RecognitionResult result)
    {
        var generation = ++_ownerPickerGeneration;

        // The picker is only for captures that carry no identity of their own.
        // If the panel shows a 游戏ID or a nickname, the character is whoever
        // that says — offering a "pick an owner" list there would invite
        // attaching one character's data to another.
        var selfIdentified = !string.IsNullOrEmpty(result.Character.GameId)
            || !string.IsNullOrEmpty(result.Character.Nickname);
        if (!string.IsNullOrEmpty(result.CharacterId) || selfIdentified)
        {
            OwnerCombo.Items.Clear();
            OwnerCombo.SelectedIndex = -1;
            OwnerPickerPanel.Visibility = Visibility.Collapsed;
            return;
        }

        IReadOnlyList<CharacterDetail> characters;
        try
        {
            characters = await AppServices.Instance.ListCharactersAsync();
        }
        catch (Exception ex)
        {
            App.Log($"RefreshOwnerPickerAsync failed: {ex.Message}");
            return;
        }

        // A newer recognition may have replaced this one while loading.
        if (generation != _ownerPickerGeneration) return;

        UiThread.OnUi(() =>
        {
            OwnerCombo.Items.Clear();
            foreach (var c in characters)
            {
                var level = c.Character.Level is { } lvl ? $" Lv.{lvl}" : string.Empty;
                var gameId = string.IsNullOrWhiteSpace(c.Character.GameId)
                    ? string.Empty
                    : $" · {c.Character.GameId}";
                OwnerCombo.Items.Add(new OwnerOption(
                    c.Character.Id,
                    $"{c.Character.Nickname}{level}{gameId}"));
            }

            // Unambiguous when there is exactly one character to choose.
            OwnerCombo.SelectedIndex = OwnerCombo.Items.Count == 1 ? 0 : -1;
            OwnerPickerPanel.Visibility = Visibility.Visible;
            NoOwnerHintText.Visibility = OwnerCombo.Items.Count == 0
                ? Visibility.Visible
                : Visibility.Collapsed;

            // Say what is going to be remembered: the next bag of this character
            // is linked by these, not by the user picking again.
            AppearanceHintText.Text = result.Appearance is { } look
                ? $"会记住外观特征：{look.Describe()}（发色与武器的画面特征用于以后自动关联）"
                : "这张截图里没有读到角色模型，无法记录外观特征；本次仍会记住面板画面，物品变化后可能认不出。";
            AppearanceHintText.Visibility = Visibility.Visible;
        });
    }

    // ---------------- Drag & drop ----------------

    private void OnDropZoneDragOver(object sender, DragEventArgs e)
    {
        e.AcceptedOperation = DataPackageOperation.Copy;
        DropZoneOutline.Stroke = Theme.Resource("BrandBlueBrush", new SolidColorBrush(Colors.DodgerBlue));
        DropZoneOutline.StrokeThickness = 2;
    }

    private void OnDropZoneDragLeave(object sender, DragEventArgs e)
    {
        DropZoneOutline.Stroke = new SolidColorBrush(Color.FromArgb(255, 0xD9, 0xDA, 0xE0));
        DropZoneOutline.StrokeThickness = 1.2;
    }

    private async void OnDropZoneDrop(object sender, DragEventArgs e)
    {
        OnDropZoneDragLeave(sender, e);
        try
        {
            if (!e.DataView.Contains(StandardDataFormats.StorageItems)) return;
            var items = await e.DataView.GetStorageItemsAsync();
            var files = items.OfType<Windows.Storage.StorageFile>()
                .Where(f => f.FileType is ".png" or ".jpg" or ".jpeg" or ".bmp")
                .ToList();
            if (files.Count == 0) return;

            await RunRecognitionAsync(await ReadFilesAsync(files.Select(f => f.Path)));
        }
        catch (Exception ex)
        {
            App.Log($"OnDropZoneDrop failed: {ex}");
        }
    }

    private async void OnSelectFilesClick(object sender, RoutedEventArgs e)
    {
        try
        {
            var picker = new FileOpenPicker
            {
                ViewMode = PickerViewMode.Thumbnail,
                SuggestedStartLocation = PickerLocationId.PicturesLibrary,
            };
            picker.FileTypeFilter.Add(".png");
            picker.FileTypeFilter.Add(".jpg");
            picker.FileTypeFilter.Add(".jpeg");
            picker.FileTypeFilter.Add(".bmp");

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindowInstance);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var files = await picker.PickMultipleFilesAsync();
            if (files is null || files.Count == 0) return;

            await RunRecognitionAsync(await ReadFilesAsync(files.Select(f => f.Path)));
        }
        catch (Exception ex)
        {
            App.Log($"OnSelectFilesClick failed: {ex}");
        }
    }

    // ---------------- Clipboard paste ----------------

    /// <summary>
    /// Paste the clipboard bitmap and actually recognise it.
    ///
    /// 0.9.3 version of this handler only wrote a log line and replayed the demo
    /// batch, so against the live backend the button silently did nothing.
    /// </summary>
    private async void OnPasteClick(object sender, RoutedEventArgs e)
    {
        if (_busy) return;
        try
        {
            var clip = await ClipboardImage.ReadAsync();
            if (clip.Items.Count == 0)
            {
                ShowHint(clip.Message, InfoBarSeverity.Warning);
                return;
            }

            App.Log($"paste: {clip.Items.Count} image(s) from clipboard, first={clip.Items[0].Name} {clip.Items[0].Bytes.Length} bytes");
            await RunRecognitionAsync(clip.Items, sourceLabel: "剪贴板截图");
        }
        catch (Exception ex)
        {
            App.Log($"OnPasteClick failed: {ex}");
            ShowHint("粘贴失败：" + ex.Message, InfoBarSeverity.Error);
        }
    }

    // ---------------- Window capture ----------------

    private async void OnCaptureClick(object sender, RoutedEventArgs e)
    {
        if (_busy) return;
        try
        {
            var windows = WindowCapture.List();
            if (windows.Count == 0)
            {
                ShowHint("没有找到可截取的应用窗口。请先把游戏窗口从最小化恢复，再点一次", InfoBarSeverity.Warning);
                return;
            }

            // The game is what this button exists for, so preselect it.
            var preferred = windows.FindIndex(w =>
                w.Title.Contains("梦幻") ||
                w.ProcessName.Contains("MyGame", StringComparison.OrdinalIgnoreCase));
            if (preferred < 0) preferred = 0;

            var (dialog, list) = BuildWindowPickerDialog(windows, preferred);
            var choice = await dialog.ShowAsync();
            if (choice != ContentDialogResult.Primary) return;
            if ((list.SelectedItem as ListViewItem)?.Tag is not CapturedWindow target) return;

            ScanningOverlay.Visibility = Visibility.Visible;
            byte[]? png;
            try
            {
                png = await WindowCapture.CaptureAsync(target);
            }
            finally
            {
                ScanningOverlay.Visibility = Visibility.Collapsed;
                // Capturing a GPU window raises it above ours; come back so the
                // recognition result is actually on screen.
                WindowCapture.BringToFront(WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindowInstance));
            }

            if (png is null)
            {
                ShowHint($"没能截取「{target.Title}」。若是独占全屏的游戏窗口，请先切成窗口/无边框模式再试",
                    InfoBarSeverity.Warning);
                return;
            }

            var stem = string.IsNullOrWhiteSpace(target.ProcessName) ? "窗口截图" : target.ProcessName;
            App.Log($"capture: {target.Title} {target.Width}x{target.Height} -> {png.Length} bytes");
            var result = await RunRecognitionAsync(
                new[] { ($"{stem}-{DateTime.Now:HHmmss}.png", png) },
                reportNoResult: false,
                sourceLabel: target.Title);

            // A captured game window is usually *not* the character panel (the
            // player has to open 人物属性 / 背包 first), so say what to do next
            // instead of claiming success on an empty read.
            var usable = IsUsable(result);
            ShowHint(
                usable
                    ? "已截取窗口画面并完成识别"
                    : "截图已抓取，但没读到角色数值。先在游戏里打开「人物属性」或「背包」面板，再点一次「截图识别」",
                usable ? InfoBarSeverity.Success : InfoBarSeverity.Warning);
        }
        catch (Exception ex)
        {
            App.Log($"OnCaptureClick failed: {ex}");
            ShowHint("截图失败：" + ex.Message, InfoBarSeverity.Error);
        }
    }

    private (ContentDialog Dialog, ListView List) BuildWindowPickerDialog(
        List<CapturedWindow> windows, int preferredIndex)
    {
        var res = Application.Current.Resources;
        var secondary = (Brush)res["TextSecondaryBrush"];
        var tertiary = (Brush)res["TextTertiaryBrush"];

        var list = new ListView
        {
            SelectionMode = ListViewSelectionMode.Single,
            Height = 320,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        AutomationProperties.SetAutomationId(list, "WindowPickerList");

        foreach (var window in windows)
        {
            var text = new StackPanel { Spacing = 2 };
            text.Children.Add(new TextBlock
            {
                Text = window.Title,
                FontSize = 13.5,
                MaxWidth = 420,
                TextWrapping = TextWrapping.NoWrap,
                TextTrimming = TextTrimming.CharacterEllipsis,
            });
            text.Children.Add(new TextBlock
            {
                Text = string.IsNullOrEmpty(window.ProcessName)
                    ? $"{window.Width}×{window.Height}"
                    : $"{window.ProcessName} · {window.Width}×{window.Height}",
                FontSize = 11.5,
                Foreground = tertiary,
            });
            list.Items.Add(new ListViewItem
            {
                Content = text,
                Tag = window,
                HorizontalAlignment = HorizontalAlignment.Stretch,
            });
        }
        list.SelectedIndex = Math.Clamp(preferredIndex, 0, windows.Count - 1);

        var body = new StackPanel { Spacing = 10, Width = 470 };
        body.Children.Add(new TextBlock
        {
            Text = "选中窗口后点「截取该窗口」：会先把它切到前台，再抓取画面（不含标题栏）",
            FontSize = 12.5,
            LineHeight = 18,
            TextWrapping = TextWrapping.Wrap,
            Foreground = secondary,
        });
        body.Children.Add(list);

        var dialog = new ContentDialog
        {
            Title = new TextBlock
            {
                Text = "截图识别 · 选择窗口",
                FontFamily = (FontFamily)res["UiDisplayFontFamily"],
                FontSize = 17,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                Foreground = (Brush)res["TextPrimaryBrush"],
            },
            Content = body,
            PrimaryButtonText = "截取该窗口",
            CloseButtonText = "取消",
            // No default button: WinUI paints the *default* one with the system
            // accent and ignores the explicit *ButtonStyle, which looked wrong on
            // a green-accent desktop (same trap as the changelog dialog).
            DefaultButton = ContentDialogButton.None,
            PrimaryButtonStyle = (Style)res["PrimaryButtonStyle"],
            CloseButtonStyle = (Style)res["SecondaryButtonStyle"],
            IsPrimaryButtonEnabled = list.SelectedItem is not null,
            XamlRoot = XamlRoot,
            RequestedTheme = ElementTheme.Light,
            Background = (Brush)res["CardBackgroundBrush"],
            CornerRadius = new CornerRadius(16),
            Padding = new Thickness(24, 22, 24, 20),
        };
        dialog.Resources["ContentDialogMaxWidth"] = 580.0;
        dialog.Resources["ContentDialogMinWidth"] = 540.0;
        dialog.Resources["ContentDialogMaxHeight"] = 700.0;

        list.SelectionChanged += (_, _) =>
            dialog.IsPrimaryButtonEnabled = list.SelectedItem is not null;

        return (dialog, list);
    }

    // ---------------- Recognition ----------------

    private static async Task<List<(string Name, byte[] Bytes)>> ReadFilesAsync(IEnumerable<string> paths)
    {
        var items = new List<(string, byte[])>();
        foreach (var path in paths)
        {
            try
            {
                items.Add((Path.GetFileName(path), await File.ReadAllBytesAsync(path)));
            }
            catch (Exception ex)
            {
                App.Log($"read image failed ({path}): {ex.Message}");
            }
        }
        return items;
    }

    /// <summary>
    /// Run one recognition round over in-memory images. Every entry point (drop,
    /// file picker, clipboard, window capture) funnels through here so they all
    /// share the overlay, the busy guard and the error reporting.
    /// </summary>
    private async Task<RecognitionResult?> RunRecognitionAsync(
        IReadOnlyList<(string Name, byte[] Bytes)> items,
        bool reportNoResult = true,
        string? sourceLabel = null)
    {
        if (items.Count == 0 || _busy) return null;

        _busy = true;
        ScanningOverlay.Visibility = Visibility.Visible;
        try
        {
            // Show what we are about to read *before* the OCR round: a screenshot
            // nobody can see is a screenshot nobody can trust, and the overlay
            // takes a second or two on a real character panel.
            await ShowPreviewAsync(items, sourceLabel);

            var images = items
                .Select(i => new RecognizeImage
                {
                    Name = i.Name,
                    Data = Convert.ToBase64String(i.Bytes),
                })
                .ToList();
            _images = images;

            var result = await AppServices.Instance.RecognizeAsync(images);
            UiThread.OnUi(() =>
            {
                if (!IsUsable(result))
                {
                    // Showing a panel full of zeros is worse than showing nothing:
                    // it looks like a successful read and "识别并保存" would archive
                    // an empty character.
                    ApplyResult(null, 0);
                    if (reportNoResult)
                    {
                        ShowHint("这张截图里没读到角色数值。请确认截的是「人物属性」或「背包」面板，且角色信息完整可见",
                            InfoBarSeverity.Warning);
                    }
                    return;
                }

                ApplyResult(result, images.Count);
                HintBar.IsOpen = false;
            });
            return result;
        }
        catch (Exception ex)
        {
            App.Log($"RunRecognitionAsync failed: {ex}");
            UiThread.OnUi(() =>
            {
                ApplyResult(null, 0);
                ShowHint("识别失败：" + ex.Message, InfoBarSeverity.Error);
            });
            return null;
        }
        finally
        {
            _busy = false;
            UiThread.OnUi(() => ScanningOverlay.Visibility = Visibility.Collapsed);
        }
    }

    /// <summary>
    /// True when a recognition result carries at least one real number.
    ///
    /// Deliberately ignores <see cref="RecognitionResult.Screens"/>: the screen
    /// kind is a label for the UI, not a verdict. Keying the success message off
    /// it meant a capture whose eight stats had all been read correctly still
    /// reported "没读到角色数值", because the pixel classifier said `Unknown`.
    /// </summary>
    private static bool IsUsable(RecognitionResult? result)
    {
        if (result is null) return false;

        return result.Stats.Hp?.Value > 0
            || result.Stats.Mp?.Value > 0
            || result.Stats.PhysicalDamage?.Value > 0
            || result.Stats.PhysicalDefense?.Value > 0
            || result.Stats.MagicDamage?.Value > 0
            || result.Stats.MagicDefense?.Value > 0
            || result.Stats.Speed?.Value > 0
            || result.Stats.HealingPower?.Value > 0
            || result.Currencies.Gold?.Value > 0
            || result.Currencies.Silver?.Value > 0;
    }

    // ---------------- Preview ----------------

    /// <summary>
    /// Decode the image and show it in the left pane. Uses a stream rather than a
    /// data: URI (which only works for PNG) and awaits <c>SetSourceAsync</c>, so
    /// the frame is only shown once it is genuinely decoded.
    /// </summary>
    private async Task ShowPreviewAsync(IReadOnlyList<(string Name, byte[] Bytes)> items, string? sourceLabel)
    {
        var first = items[0];
        var label = string.IsNullOrWhiteSpace(sourceLabel) ? first.Name : sourceLabel;
        _previewBytes = first.Bytes;

        try
        {
            var bitmap = new BitmapImage();
            using (var stream = new InMemoryRandomAccessStream())
            {
                var writer = new DataWriter(stream.GetOutputStreamAt(0));
                writer.WriteBytes(first.Bytes);
                await writer.StoreAsync();
                await writer.FlushAsync();
                writer.DetachStream();
                await bitmap.SetSourceAsync(stream);
            }

            PreviewImage.Source = bitmap;
            var extra = items.Count > 1 ? $"（共 {items.Count} 张）" : string.Empty;
            PreviewCaptionText.Text = $"{label} · {bitmap.PixelWidth}×{bitmap.PixelHeight}{extra}";
            DropEmptyPanel.Visibility = Visibility.Collapsed;
            PreviewPanel.Visibility = Visibility.Visible;
        }
        catch (Exception ex)
        {
            // A preview failure must not break recognition.
            App.Log($"ShowPreview failed: {ex.Message}");
        }
    }

    private void ResetPreview()
    {
        _previewBytes = null;
        PreviewImage.Source = null;
        PreviewCaptionText.Text = string.Empty;
        PreviewPanel.Visibility = Visibility.Collapsed;
        DropEmptyPanel.Visibility = Visibility.Visible;
    }

    private void OnClearPreviewClick(object sender, RoutedEventArgs e) => ResetPreview();

    private void OnZoomPreviewTapped(object sender, TappedRoutedEventArgs e) => ShowPreviewDialog();

    private void OnZoomPreviewClick(object sender, RoutedEventArgs e) => ShowPreviewDialog();

    /// <summary>Full-size view of the current preview, so the capture can be checked before trusting it.</summary>
    private async void ShowPreviewDialog()
    {
        if (_previewBytes is null || PreviewImage.Source is null) return;
        try
        {
            var res = Application.Current.Resources;
            var image = new Image
            {
                Source = PreviewImage.Source,
                Stretch = Stretch.Uniform,
                MaxWidth = 980,
                MaxHeight = 600,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            };

            var body = new StackPanel { Spacing = 0 };
            body.Children.Add(new Border
            {
                Background = (Brush)res["SubtleFillBrush"],
                CornerRadius = new CornerRadius(10),
                Padding = new Thickness(8),
                Child = image,
            });
            body.Children.Add(new TextBlock
            {
                Text = PreviewCaptionText.Text,
                FontSize = 12,
                TextAlignment = TextAlignment.Center,
                TextWrapping = TextWrapping.Wrap,
                Margin = new Thickness(0, 10, 0, 0),
                Foreground = (Brush)res["TextTertiaryBrush"],
            });

            var dialog = new ContentDialog
            {
                Title = new TextBlock
                {
                    Text = "截图预览",
                    FontFamily = (FontFamily)res["UiDisplayFontFamily"],
                    FontSize = 17,
                    FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                    Foreground = (Brush)res["TextPrimaryBrush"],
                },
                Content = body,
                CloseButtonText = "关闭",
                DefaultButton = ContentDialogButton.None,
                CloseButtonStyle = (Style)res["SecondaryButtonStyle"],
                XamlRoot = XamlRoot,
                RequestedTheme = ElementTheme.Light,
                Background = (Brush)res["CardBackgroundBrush"],
                CornerRadius = new CornerRadius(16),
                Padding = new Thickness(20, 18, 20, 16),
            };
            dialog.Resources["ContentDialogMaxWidth"] = 1040.0;
            dialog.Resources["ContentDialogMaxHeight"] = 760.0;
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            App.Log($"ShowPreviewDialog failed: {ex}");
        }
    }

    // ---------------- Actions ----------------

    private async void OnSaveClick(object sender, RoutedEventArgs e)
    {
        if (_result is null) return;
        try
        {
            // An unidentified capture (a 包裹 screenshot has no 游戏ID) is saved
            // against whichever character the user picked — by nickname or by
            // 游戏ID. Without a pick there is nothing to attach to: saving would
            // invent a "未命名角色" row that no 人物属性 capture will match again.
            if (string.IsNullOrEmpty(_result.CharacterId))
            {
                var selfIdentified = !string.IsNullOrEmpty(_result.Character.GameId)
                    || !string.IsNullOrEmpty(_result.Character.Nickname);
                if (!selfIdentified)
                {
                    if (OwnerCombo.SelectedItem is not OwnerOption owner)
                    {
                        ShowToast(OwnerCombo.Items.Count == 0
                            ? "还没有角色可选：先在「角色管理」里新建，或先录入一张「人物属性」截图"
                            : "请先选择这次识别归属的角色");
                        OwnerCombo.Focus(FocusState.Programmatic);
                        return;
                    }

                    _result.CharacterId = owner.Id;
                }
            }

            var screens = _images.Count > 0
                ? _images.Select(i => i.Name).ToList()
                : new List<string> { "character_attributes.png", "inventory.png" };

            var id = await AppServices.Instance.SaveRecognitionAsync(_result, screens);
            App.Log($"saved recognition -> {id}");

            // A capture that was attributed by hand has no name of its own; pull
            // the character the save landed on so the panel shows 昵称 / 游戏ID
            // instead of "未命名角色" right after a successful save.
            await FillIdentityFromLibraryAsync(_result);

            // Re-render from the now-resolved result: leaving the owner picker
            // on screen after a successful save reads like the save did nothing.
            ApplyResult(_result, _images.Count);

            var settings = await AppServices.Instance.GetSettingsAsync();
            if (settings.AutoNavigateAfterSave)
                App.MainWindowInstance?.NavigateTo(1);

            ShowToast("已保存本次识别");
        }
        catch (Exception ex)
        {
            App.Log($"OnSaveClick failed: {ex}");
            ShowToast("保存失败：" + ex.Message);
        }
    }

    /// <summary>
    /// Fill in whatever identity the result is missing from the library row it
    /// was saved against. A 包裹 capture has no nickname / 游戏ID / level of its
    /// own, so without this the panel would keep saying "未命名角色" even though
    /// the user just attributed it to 莫莫莫.
    /// </summary>
    private async Task FillIdentityFromLibraryAsync(RecognitionResult result)
    {
        if (string.IsNullOrEmpty(result.CharacterId)) return;
        try
        {
            var characters = await AppServices.Instance.ListCharactersAsync();
            var hit = characters.FirstOrDefault(c => c.Character.Id == result.CharacterId);
            if (hit is null) return;

            result.Character.Nickname ??= hit.Character.Nickname;
            result.Character.GameId ??= hit.Character.GameId;
            result.Character.Level ??= hit.Character.Level;
        }
        catch (Exception ex)
        {
            App.Log($"FillIdentityFromLibraryAsync failed: {ex.Message}");
        }
    }

    private void OnUndoClick(object sender, RoutedEventArgs e)
    {
        _result = null;
        _images = new List<RecognizeImage>();
        EmptyResultState.Visibility = Visibility.Visible;
        ResultScroll.Visibility = Visibility.Collapsed;
        ResetPreview();
        ShowToast("已撤销本次录入");
    }

    /// <summary>
    /// Page-level message (clipboard empty, capture failed, nothing recognised).
    /// Unlike <see cref="ShowToast"/> it stays visible when there is no result to
    /// display, which is exactly when the user needs the explanation.
    /// </summary>
    private void ShowHint(string message, InfoBarSeverity severity)
    {
        if (string.IsNullOrWhiteSpace(message)) return;
        try
        {
            HintBar.Severity = severity;
            HintBar.Message = message;
            HintBar.IsOpen = true;
        }
        catch (Exception ex)
        {
            App.Log($"ShowHint failed: {ex.Message}");
        }
    }

    private void ShowToast(string message)
    {
        try
        {
            var info = new InfoBar
            {
                Message = message,
                Severity = InfoBarSeverity.Success,
                IsOpen = true,
                IsClosable = true,
                Margin = new Thickness(0, 10, 0, 0),
            };
            if (ResultPanel.Children.Count > 0)
                ResultPanel.Children.Insert(0, info);
        }
        catch (Exception ex)
        {
            App.Log($"ShowToast failed: {ex.Message}");
        }
    }
}
