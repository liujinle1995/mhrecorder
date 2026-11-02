using Microsoft.UI.Text;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.UI.Xaml.Shapes;
using MhRecorder.App.Services;
using MhRecorder.Core.ViewModels;
using Windows.Storage.Streams;
using Windows.System;
using Windows.UI;

namespace MhRecorder.App.Views;

/// <summary>Page 2 — 角色管理: search, horizontal carousel of character cards, pagination.</summary>
public sealed partial class CharactersPage : Page
{
    private const double CardWidth = 316;
    private const double CardGap = 26;

    private static readonly SolidColorBrush CardBorder = new(Color.FromArgb(255, 0xE5, 0xE6, 0xEB));
    private static readonly SolidColorBrush TextSecondary = new(Color.FromArgb(255, 0x6B, 0x6F, 0x76));
    private static readonly SolidColorBrush TextTertiary = new(Color.FromArgb(255, 0x9A, 0xA0, 0xA6));
    private static readonly SolidColorBrush GoldAccent = new(Color.FromArgb(255, 0xF6, 0xB8, 0x2B));
    private static readonly SolidColorBrush SilverAccent = new(Color.FromArgb(255, 0x8A, 0x93, 0xA5));
    private static readonly SolidColorBrush SuccessGreen = new(Color.FromArgb(255, 0x1F, 0xB4, 0x61));
    private static readonly SolidColorBrush DangerRed = new(Color.FromArgb(255, 0xE5, 0x48, 0x4D));
    private static readonly SolidColorBrush ChipBackground = new(Color.FromArgb(255, 0xF2, 0xF3, 0xF5));
    private static readonly SolidColorBrush AvatarTileBackground = new(Color.FromArgb(255, 0xE9, 0xED, 0xF6));

    private List<CharacterCardViewModel> _all = new();
    private List<CharacterCardViewModel> _filtered = new();
    private int _index;

    /// <summary>
    /// A card currently placed on the canvas, tagged with the carousel slot it
    /// occupies so a slide can work out where it has to travel.
    /// </summary>
    private sealed record CardSlot(Border Card, int Offset, int Target);

    private readonly List<CardSlot> _cardSlots = new();

    /// <summary>True while a slide animation owns the canvas.</summary>
    private bool _animating;

    /// <summary>Set when Render was requested mid-animation.</summary>
    private bool _pendingRelayout;

    /// <summary>Duration of the carousel slide, in milliseconds.</summary>
    private const int SlideDurationMs = 260;

    /// <summary>One-shot timer that re-lays-out the trio after a slide.</summary>
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _settleTimer;

    public CharactersPage()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Unloaded += (_, _) => DetachKeys();
        KeyDown += OnPageKeyDown;
        CarouselCanvas.PointerWheelChanged += OnWheelChanged;
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            await LoadAsync();
            AttachKeys();
        }
        catch (Exception ex)
        {
            App.Log($"CharactersPage.OnLoaded failed: {ex}");
        }
    }

    private async Task LoadAsync()
    {
        var details = await AppServices.Instance.ListCharactersAsync();
        _all = details.Select(d => new CharacterCardViewModel(d)).ToList();

        var query = SearchBox?.Text ?? string.Empty;
        _filtered = _all.Where(c => c.Matches(query)).ToList();
        _index = _filtered.Count > 0 ? Math.Clamp(_index <= 0 ? 0 : _index, 0, _filtered.Count - 1) : 0;

        SubtitleText.Text = _all.Count == 0
            ? "导入的角色都会在这里以卡片呈现"
            : $"导入的角色都会在这里以卡片呈现，共 {_all.Count} 个角色 · 最近导入 {Fmt.FriendlyStamp(_all.Max(c => c.LastRecorded))}";

        Render();
    }

    private void OnSearchTextChanged(object sender, TextChangedEventArgs e)
    {
        var query = SearchBox.Text ?? string.Empty;
        _filtered = _all.Where(c => c.Matches(query)).ToList();
        _index = Math.Clamp(_index, 0, Math.Max(0, _filtered.Count - 1));
        Render();
    }

    private void OnCarouselSizeChanged(object sender, SizeChangedEventArgs e) => Render();

    /// <summary>
    /// A Canvas never clips its children, so tall centre cards would spill over
    /// the pagination dots and the footer hint. Clip the host Grid to its own
    /// bounds whenever it is laid out.
    /// </summary>
    private void OnCarouselHostSizeChanged(object sender, SizeChangedEventArgs e)
    {
        CarouselClip.Rect = new Windows.Foundation.Rect(0, 0, e.NewSize.Width, e.NewSize.Height);
        Render();
    }

    // ---------------- carousel ----------------

    private void Render()
    {
        if (_animating)
        {
            // A slide is in flight; let it finish and re-lay-out afterwards so
            // we never animate from a stale geometry.
            _pendingRelayout = true;
            return;
        }

        CarouselCanvas.Children.Clear();
        _cardSlots.Clear();
        EmptyCharactersText.Visibility = _filtered.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        RenderDots();

        if (_filtered.Count == 0) return;

        var boundsWidth = CarouselCanvas.ActualWidth > 0 ? CarouselCanvas.ActualWidth : 900;
        var centerX = boundsWidth / 2;

        // ------------------------------------------------------------------
        // Layout rule (per the product spec):
        //   * 1–2 characters  -> lay them out FLAT and centred; no carousel,
        //                        because there is nothing to page through.
        //   * 3+ characters   -> CIRCULAR carousel: previous / centre / next
        //                        neighbours always render (wrapped modulo the
        //                        count), so the first paint shows three cards
        //                        with the FIRST character centred. Skipping
        //                        out-of-range neighbours left only two cards
        //                        on the first page and made the row look like
        //                        it started from the second card.
        //                        The SECOND card (right neighbour at index 0)
        //                        sits on top in Z order: Canvas paints later
        //                        children above earlier ones.
        // ------------------------------------------------------------------
        if (_filtered.Count <= 2)
        {
            RenderFlat(centerX);
            return;
        }

        var slots = new (int Offset, bool IsCenter)[]
        {
            // Paint order: left neighbour, centre, then the right neighbour
            // LAST so it lands above both of them.
            (-1, false),
            (0, true),
            (1, false),
        };

        var count = _filtered.Count;
        foreach (var slot in slots)
        {
            // Wrap so every position always has a card: at _index 0 the left
            // neighbour is the last character, at the last index the right
            // neighbour is the first one. Matches the modulo paging of the
            // arrows / wheel.
            var target = ((_index + slot.Offset) % count + count) % count;

            var card = BuildCard(_filtered[target], slot.IsCenter);
            _cardSlots.Add(new CardSlot(card, slot.Offset, target));

            // Cards have an intrinsic height; shrink the whole carousel so the
            // tallest card always fits the available host height instead of
            // being clipped by the host's rectangle geometry.
            var available = CarouselHost.ActualHeight > 0
                ? CarouselHost.ActualHeight
                : CarouselCanvas.ActualHeight;
            var topPad = slot.IsCenter ? 8.0 : 34.0;
            var fit = 1.0;
            if (available > 0)
            {
                var cardHeight = card.ActualHeight > 0
                    ? card.ActualHeight
                    : MeasureCardHeight(card);
                if (cardHeight > 0)
                {
                    var usable = Math.Max(80.0, available - topPad);
                    fit = Math.Min(1.0, usable / cardHeight);
                }
            }

            if (slot.IsCenter)
            {
                card.RenderTransform = new ScaleTransform { ScaleX = fit, ScaleY = fit };
                Canvas.SetLeft(card, centerX - CardWidth / 2);
                Canvas.SetTop(card, topPad);
            }
            else
            {
                // Pull the neighbours in so a meaningful slice of each card is
                // visible, as in the mock, instead of just a sliver at the edge.
                var neighbour = Math.Min(fit, 0.955);
                // Neighbour offset. The mock shows a clear gap between cards,
                // so the peek distance must exceed the card width; 0.78*width
                // pulled them into an overlap.
                var peek = (CardWidth + CardGap) * 1.06;
                Canvas.SetLeft(card, centerX + slot.Offset * peek - CardWidth / 2);
                Canvas.SetTop(card, topPad);
                card.RenderTransform = new ScaleTransform { ScaleX = neighbour, ScaleY = neighbour };
            }
            card.RenderTransformOrigin = new Windows.Foundation.Point(0.5, 0.0);

            CarouselCanvas.Children.Add(card);
        }
    }

    /// <summary>
    /// Lay out one or two characters side by side, centred, with no carousel.
    ///
    /// With so few cards there is nothing to page through, so the mock's
    /// "peek one card either side" carousel reads as broken: the single card
    /// looks off-centre and the arrows are pointless. Both cards render at
    /// their natural size and are scaled only if they would not fit the host.
    /// </summary>
    private void RenderFlat(double centerX)
    {
        var count = _filtered.Count;
        var available = CarouselHost.ActualHeight > 0
            ? CarouselHost.ActualHeight
            : CarouselCanvas.ActualHeight;

        // Two cards plus the standard gap, or one card's width.
        var totalWidth = count == 1
            ? CardWidth
            : CardWidth * 2 + CardGap;
        var startX = centerX - totalWidth / 2.0;

        for (var i = 0; i < count; i++)
        {
            var card = BuildCard(_filtered[i], isCenter: true);

            // Natural metrics first: both the fit factor and the vertical offset
            // need the height, and a card outside the visual tree reports
            // ActualHeight == 0 until it is measured.
            var cardHeight = card.ActualHeight > 0
                ? card.ActualHeight
                : MeasureCardHeight(card);

            var fit = 1.0;
            if (available > 0 && cardHeight > 0)
            {
                var usable = Math.Max(80.0, available - 12.0);
                fit = Math.Min(1.0, usable / cardHeight);
            }

            // Shrink horizontally too, so two cards cannot overflow the host.
            var hostWidth = CarouselCanvas.ActualWidth;
            if (hostWidth > 0 && totalWidth > 0 && totalWidth * fit > hostWidth)
            {
                fit = Math.Min(fit, hostWidth / totalWidth);
            }

            card.RenderTransform = new ScaleTransform { ScaleX = fit, ScaleY = fit };
            card.RenderTransformOrigin = new Windows.Foundation.Point(0.5, 0.0);

            var left = count == 1
                ? centerX - CardWidth / 2.0
                : startX + i * (CardWidth + CardGap);

            // Centre the row vertically in the carousel area. With one or two
            // cards the host is far taller than the card, and pinning it to the
            // top left a band of dead space underneath — that imbalance is a big
            // part of why the page read as "不协调" at 0.9.2.
            var scaledHeight = cardHeight > 0 ? cardHeight * fit : 0;
            var top = available > 0 && scaledHeight > 0
                ? Math.Max(8.0, (available - scaledHeight) / 2.0)
                : 8.0;

            Canvas.SetLeft(card, left);
            Canvas.SetTop(card, top);

            CarouselCanvas.Children.Add(card);
        }
    }

    private void RenderDots()
    {
        if (DotsPanel is null) return;
        DotsPanel.Children.Clear();

        var count = _filtered.Count;
        if (count == 0) return;
        var maxDots = Math.Min(count, 8);
        var activeIndex = Math.Min(_index, maxDots - 1);

        for (var i = 0; i < maxDots; i++)
        {
            var active = i == activeIndex;
            var dot = new Border
            {
                Width = active ? 16 : 7,
                Height = 7,
                CornerRadius = new CornerRadius(3.5),
                Background = active
                    ? Theme.Resource("BrandBlueBrush", new SolidColorBrush(Color.FromArgb(255, 0, 116, 255)))
                    : new SolidColorBrush(Color.FromArgb(255, 0xD5, 0xD7, 0xDD)),
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(0, 0, 3, 0),
            };
            DotsPanel.Children.Add(dot);
        }
    }

    /// <summary>
    /// Measure a card that has not been laid out yet. Until it is in the visual
    /// tree <c>ActualHeight</c> is 0, so we ask for its natural size given the
    /// known fixed width.
    /// </summary>
    private static double MeasureCardHeight(FrameworkElement card)
    {
        try
        {
            card.Measure(new Windows.Foundation.Size(CardWidth, double.PositiveInfinity));
            return card.DesiredSize.Height;
        }
        catch
        {
            return 0;
        }
    }

    private Border BuildCard(CharacterCardViewModel vm, bool isCenter)
    {
        var card = new Border
        {
            Width = CardWidth,
            Background = Theme.Resource("CardBackgroundBrush", new SolidColorBrush(Colors.White)),
            BorderBrush = Theme.Resource("CardBorderBrush", CardBorder),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(14),
            // Generous vertical breathing room: at 0.9.2 the card came out ~381px
            // tall at 316px wide (ratio 1.21) while the mock is ~424px (1.34), so
            // every section was jammed against the next and the card read as
            // squashed. Current rhythm lands at ~445px (1.41): close to the mock,
            // and comfortably filling the carousel area instead of hugging the top.
            Padding = new Thickness(18, isCenter ? 18 : 16, 18, isCenter ? 16 : 14),
        };

        if (isCenter)
        {
            try
            {
                card.Translation = new System.Numerics.Vector3(0, 0, 32);
                card.Shadow = new ThemeShadow();
            }
            catch (Exception ex)
            {
                App.Log($"ThemeShadow unavailable: {ex.Message}");
            }
        }

        var root = new StackPanel { Spacing = 0 };

        // ---- identity row ----
        var identity = new Grid();
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var avatarTile = new Border
        {
            Width = isCenter ? 60 : 46,
            Height = isCenter ? 60 : 46,
            CornerRadius = new CornerRadius(isCenter ? 30 : 23),
            Background = AvatarTileBackground,
        };

        if (!string.IsNullOrEmpty(vm.AvatarPngBase64))
        {
            // Real portrait, cropped from the screenshot by the backend.
            // (The old code special-cased one nickname and loaded a bundled
            // PNG, so every other character fell back to an initial.)
            //
            // Decoding must be asynchronous: calling SetSourceAsync().GetResults()
            // on the UI thread deadlocks, which is why an earlier synchronous
            // attempt left the tile blank. Load via ImageOpened and only swap
            // the child once the bitmap is genuinely ready.
            try
            {
                avatarTile.Child = BuildInitial(vm.AvatarInitial, isCenter ? 24 : 18);
                var image = new Image { Stretch = Stretch.UniformToFill, Opacity = 0 };
                var bitmap = new BitmapImage();
                bitmap.ImageOpened += (_, _) =>
                {
                    image.Source = bitmap;
                    image.Opacity = 1;
                    avatarTile.Child = image;
                };
                bitmap.ImageFailed += (_, e) =>
                {
                    App.Log($"card avatar decode failed: {e.ErrorMessage}");
                };
                _ = LoadAvatarAsync(bitmap, vm.AvatarPngBase64);
            }
            catch (Exception ex)
            {
                App.Log($"card avatar failed: {ex.Message}");
                avatarTile.Child = BuildInitial(vm.AvatarInitial, isCenter ? 24 : 18);
            }
        }
        else
        {
            avatarTile.Child = BuildInitial(vm.AvatarInitial, isCenter ? 24 : 18);
        }

        Grid.SetColumn(avatarTile, 0);
        identity.Children.Add(avatarTile);

        var nameStack = new StackPanel
        {
            Margin = new Thickness(12, 0, 0, 0),
            Spacing = 3,
            VerticalAlignment = VerticalAlignment.Center,
        };

        var nameRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 7 };
        nameRow.Children.Add(new TextBlock
        {
            Text = vm.Nickname,
            FontSize = isCenter ? 17 : 16,
            FontWeight = FontWeights.SemiBold,
            FontFamily = Theme.Resource("UiDisplayFontFamily", new FontFamily("Segoe UI Variable Display")),
            VerticalAlignment = VerticalAlignment.Center,
        });
        nameRow.Children.Add(new Border
        {
            Background = ChipBackground,
            CornerRadius = new CornerRadius(5),
            Padding = new Thickness(6, 1, 6, 1),
            VerticalAlignment = VerticalAlignment.Center,
            Child = new TextBlock { Text = vm.LevelText, FontSize = 11.5, Foreground = TextSecondary },
        });
        nameStack.Children.Add(nameRow);
        nameStack.Children.Add(new TextBlock
        {
            Text = vm.GameIdText,
            FontSize = 12.5,
            Foreground = TextTertiary,
        });

        Grid.SetColumn(nameStack, 1);
        identity.Children.Add(nameStack);
        root.Children.Add(identity);

        // ---- HP / MP ----
        root.Children.Add(SectionLabel("气血", 18));
        root.Children.Add(MakeBar(vm.HpText, true, isCenter ? 24 : 20));
        root.Children.Add(SectionLabel("魔法", 14));
        root.Children.Add(MakeBar(vm.MpText, false, isCenter ? 24 : 20));

        // ---- combat stats ----
        root.Children.Add(SectionLabel("战斗属性", 18));
        var statGrid = new Grid { RowSpacing = 9, ColumnSpacing = 6 };
        statGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        statGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        statGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        statGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        for (var i = 0; i < 3; i++)
            statGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto, MinHeight = 21 });

        AddStat(statGrid, 0, 0, "物理伤害", vm.PhysicalDamage);
        AddStat(statGrid, 0, 1, "物理防御", vm.PhysicalDefense);
        AddStat(statGrid, 1, 0, "法术伤害", vm.MagicDamage);
        AddStat(statGrid, 1, 1, "法术防御", vm.MagicDefense);
        AddStat(statGrid, 2, 0, "速度", vm.Speed);
        AddStat(statGrid, 2, 1, "治疗强度", vm.HealingPower);
        root.Children.Add(statGrid);

        // ---- currencies ----
        var currencyGrid = new Grid { Margin = new Thickness(0, 18, 0, 0), RowSpacing = 10 };
        currencyGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto, MinHeight = 21 });
        currencyGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto, MinHeight = 21 });
        currencyGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        currencyGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        currencyGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        currencyGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        AddCurrency(currencyGrid, 0, "金币", vm.GoldText, GoldAccent, vm.GoldDeltaText, positive: false);
        AddCurrency(currencyGrid, 1, "银币", vm.SilverText, SilverAccent, vm.SilverDeltaText, positive: true);
        root.Children.Add(currencyGrid);

        // ---- delete ----
        var deleteButton = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Right,
            Margin = new Thickness(0, 10, 0, 0),
            Style = Theme.Resource<Style>("LinkButtonStyle", null!),
            Tag = vm.Id,
        };
        AutomationProperties.SetAutomationId(deleteButton, $"Delete_{vm.Id}");
        deleteButton.Click += OnDeleteCharacterClick;

        var deleteContent = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        deleteContent.Children.Add(new TextBlock
        {
            Text = "\uE74D",
            FontFamily = Theme.Resource("IconFontFamily", new FontFamily("Segoe Fluent Icons")),
            FontSize = 11,
            Foreground = TextTertiary,
            VerticalAlignment = VerticalAlignment.Center,
        });
        deleteContent.Children.Add(new TextBlock
        {
            Text = "删除",
            FontSize = 12.5,
            Foreground = TextTertiary,
            VerticalAlignment = VerticalAlignment.Center,
        });
        deleteButton.Content = deleteContent;
        root.Children.Add(deleteButton);

        card.Child = root;
        return card;
    }

    /// <summary>
    /// Feed a base64 PNG into a <see cref="BitmapImage"/> off the UI thread.
    ///
    /// BitmapImage exposes no synchronous byte loader, and blocking on
    /// SetSourceAsync inside the UI thread deadlocks. Decoding into a small
    /// in-memory stream and awaiting SetSourceAsync lets ImageOpened fire.
    /// </summary>
    private static async Task LoadAvatarAsync(BitmapImage target, string base64Png)
    {
        try
        {
            var bytes = Convert.FromBase64String(base64Png);
            var stream = new InMemoryRandomAccessStream();
            var writer = new DataWriter(stream.GetOutputStreamAt(0));
            writer.WriteBytes(bytes);
            await writer.StoreAsync();
            await target.SetSourceAsync(stream);
        }
        catch (Exception ex)
        {
            App.Log($"LoadAvatarAsync failed: {ex.Message}");
        }
    }

    private static UIElement BuildInitial(string initial, double fontSize) => new TextBlock
    {
        Text = initial,
        FontSize = fontSize,
        FontFamily = Theme.Resource("UiDisplayFontFamily", new FontFamily("Segoe UI Variable Display")),
        FontWeight = FontWeights.SemiBold,
        Foreground = new SolidColorBrush(Color.FromArgb(255, 0x7A, 0x86, 0xA8)),
        HorizontalAlignment = HorizontalAlignment.Center,
        VerticalAlignment = VerticalAlignment.Center,
    };

    private static TextBlock SectionLabel(string text, double topMargin) => new()
    {
        Text = text,
        FontSize = 12,
        Margin = new Thickness(0, topMargin, 0, 6),
        Foreground = TextSecondary,
    };

    private static UIElement MakeBar(string label, bool hp, double height) =>
        new Controls.GradientProgressBar
        {
            Label = label,
            Ratio = 1,
            BarHeight = height,
            UseHpGradient = hp,
        };

    private static void AddStat(Grid grid, int row, int pair, string label, string value)
    {
        var labelBlock = new TextBlock
        {
            Text = label,
            FontSize = 13,
            Foreground = TextSecondary,
            Margin = new Thickness(pair == 1 ? 16 : 0, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetRow(labelBlock, row);
        Grid.SetColumn(labelBlock, pair == 0 ? 0 : 2);
        grid.Children.Add(labelBlock);

        var valueBlock = new TextBlock
        {
            Text = value,
            FontSize = 13,
            FontWeight = FontWeights.SemiBold,
            HorizontalAlignment = HorizontalAlignment.Right,
            MinWidth = 54,
            TextAlignment = TextAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetRow(valueBlock, row);
        Grid.SetColumn(valueBlock, pair == 0 ? 1 : 3);
        grid.Children.Add(valueBlock);
    }

    private static void AddCurrency(Grid grid, int row, string label, string value,
        SolidColorBrush accent, string? deltaText, bool positive)
    {
        var dot = new Ellipse
        {
            Width = 7, Height = 7, Fill = accent,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetRow(dot, row); Grid.SetColumn(dot, 0); grid.Children.Add(dot);

        var labelBlock = new TextBlock
        {
            Text = label,
            FontSize = 13,
            Margin = new Thickness(8, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = TextSecondary,
        };
        Grid.SetRow(labelBlock, row); Grid.SetColumn(labelBlock, 1); grid.Children.Add(labelBlock);

        var valueBlock = new TextBlock
        {
            Text = value,
            FontSize = 15,
            FontWeight = FontWeights.SemiBold,
            Foreground = accent,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 8, 0),
        };
        Grid.SetRow(valueBlock, row); Grid.SetColumn(valueBlock, 2); grid.Children.Add(valueBlock);

        if (deltaText is null or "0") return;

        var pill = new Border
        {
            Background = new SolidColorBrush(positive
                ? Color.FromArgb(255, 0xE7, 0xF7, 0xEE)
                : Color.FromArgb(255, 0xFD, 0xEC, 0xEC)),
            CornerRadius = new CornerRadius(9),
            Padding = new Thickness(8, 2, 8, 2),
            VerticalAlignment = VerticalAlignment.Center,
            Child = new TextBlock
            {
                Text = deltaText,
                FontSize = 11.5,
                Foreground = positive ? SuccessGreen : DangerRed,
            },
        };
        Grid.SetRow(pill, row); Grid.SetColumn(pill, 3); grid.Children.Add(pill);
    }

    // ---------------- navigation ----------------

    private void OnPrevClick(object sender, RoutedEventArgs e) => Step(-1);
    private void OnNextClick(object sender, RoutedEventArgs e) => Step(1);

    private void Step(int delta)
    {
        if (_filtered.Count == 0) return;

        var from = _index;
        _index = (_index + delta + _filtered.Count) % _filtered.Count;

        // A slide only makes sense between two distinct cards; with one card
        // (or while nothing is laid out yet) just re-render.
        if (_filtered.Count > 2 && _cardSlots.Count > 0)
        {
            SlideTo(_index, delta);
        }
        else
        {
            Render();
        }

        _ = from;
    }

    /// <summary>
    /// Animate the carousel from its current arrangement to the one centred on
    /// <paramref name="targetIndex"/>.
    ///
    /// Cards are reused rather than rebuilt: each existing card is moved to the
    /// slot it should occupy after the step, using a Composition implicit
    /// offset animation so the compositor interpolates it. Sliding the old
    /// cards off and the new ones in (instead of a hard Children.Clear +
    /// rebuild) is what makes paging read as motion rather than a jump cut.
    /// </summary>
    private void SlideTo(int targetIndex, int delta)
    {
        var count = _filtered.Count;
        var boundsWidth = CarouselCanvas.ActualWidth > 0 ? CarouselCanvas.ActualWidth : 900;
        var centerX = boundsWidth / 2;
        var peek = (CardWidth + CardGap) * 1.06;

        // Keep the outgoing card and an incoming card in play so the row never
        // thins out mid-flight: the trio slides one slot, the card that just
        // entered the trio rides in from the far edge, and everything is
        // rebuilt once the slide settles (a full Render() then restores the
        // exact geometry and the required Z order for the new index).
        _animating = true;

        // 1) Slide the existing trio one slot.
        foreach (var slot in _cardSlots)
        {
            var newOffset = slot.Offset - delta;
            var targetScale = newOffset == 0 ? 1.0 : 0.955;
            AnimateCard(slot.Card, centerX, peek, newOffset, targetScale);
        }

        // 2) Bring the neighbour that just entered the trio in from the side.
        var enteringSlot = delta > 0 ? 2 : -2;
        var enteringIndex = ((targetIndex + delta) % count + count) % count;
        var entering = BuildCard(_filtered[enteringIndex], isCenter: false);
        entering.RenderTransformOrigin = new Windows.Foundation.Point(0.5, 0.0);
        entering.RenderTransform = new ScaleTransform { ScaleX = 0.955, ScaleY = 0.955 };
        Canvas.SetLeft(entering, centerX + enteringSlot * peek - CardWidth / 2);
        Canvas.SetTop(entering, 34);
        entering.Opacity = 0;

        // Insert behind the existing trio so the right neighbour keeps winning
        // the Z fight, matching the static layout.
        CarouselCanvas.Children.Insert(0, entering);
        FadeIn(entering);
        AnimateCard(entering, centerX, peek, delta > 0 ? 1 : -1, 0.955);

        // 3) Settle: rebuild at the new index (also fixes Z order).
        StartSettleTimer();
    }

    /// <summary>Fade an entering card in over the same duration as the slide.</summary>
    private static void FadeIn(UIElement element)
    {
        var anim = new Microsoft.UI.Xaml.Media.Animation.DoubleAnimation
        {
            From = 0.0,
            To = 1.0,
            Duration = new Duration(TimeSpan.FromMilliseconds(SlideDurationMs)),
            EasingFunction = new Microsoft.UI.Xaml.Media.Animation.CubicEase
            {
                EasingMode = Microsoft.UI.Xaml.Media.Animation.EasingMode.EaseOut,
            },
        };
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTarget(anim, element);
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTargetProperty(anim, "Opacity");

        var sb = new Microsoft.UI.Xaml.Media.Animation.Storyboard();
        sb.Children.Add(anim);
        sb.Begin();
    }

    /// <summary>
    /// Re-layout once the slide has finished.
    ///
    /// A single one-shot DispatcherQueue timer, restarted on each step, keeps
    /// rapid paging from stacking callbacks or leaving the canvas half-animated.
    /// </summary>
    private void StartSettleTimer()
    {
        if (_settleTimer is null)
        {
            _settleTimer = DispatcherQueue.CreateTimer();
            _settleTimer.IsRepeating = false;
            _settleTimer.Tick += (_, _) =>
            {
                _settleTimer!.Stop();
                _animating = false;
                _pendingRelayout = false;
                Render();
            };
        }

        _settleTimer.Stop();
        _settleTimer.Interval = TimeSpan.FromMilliseconds(SlideDurationMs + 30);
        _settleTimer.Start();
    }

    /// <summary>
    /// Move one card to the given slot with a compositor animation.
    ///
    /// Canvas.Left/Top cannot be animated directly, so the horizontal travel is
    /// expressed as a translation on the element's visual: the card's final
    /// Canvas position is committed immediately, and the animated translation
    /// starts at the inverse delta and eases to zero. The compositor runs this
    /// off the UI thread, so the slide stays smooth.
    /// </summary>
    private void AnimateCard(
        Border card,
        double centerX,
        double peek,
        int slotOffset,
        double targetScale)
    {
        try
        {
            var targetLeft = centerX + slotOffset * peek - CardWidth / 2;
            var targetTop = slotOffset == 0 ? 8.0 : 34.0;

            var startLeft = Canvas.GetLeft(card);
            if (double.IsNaN(startLeft)) startLeft = targetLeft;

            var dx = startLeft - targetLeft; // inverse delta, eases to 0

            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview
                .GetElementVisual(card);

            var offsetAnim = visual.Compositor.CreateVector3KeyFrameAnimation();
            var easing = visual.Compositor.CreateCubicBezierEasingFunction(
                new System.Numerics.Vector2(0.22f, 0.61f),
                new System.Numerics.Vector2(0.36f, 1f));
            offsetAnim.InsertKeyFrame(0f, new System.Numerics.Vector3((float)dx, 0f, 0f));
            offsetAnim.InsertKeyFrame(1f, System.Numerics.Vector3.Zero, easing);
            offsetAnim.Duration = TimeSpan.FromMilliseconds(SlideDurationMs);

            offsetAnim.Target = "Translation";
            visual.StartAnimation("Offset", offsetAnim);

            // Commit the destination geometry so the animated visual lands
            // exactly where the static layout expects it.
            Canvas.SetLeft(card, targetLeft);
            Canvas.SetTop(card, targetTop);

            var current = card.RenderTransform as ScaleTransform;
            var startScale = current is null ? 1.0 : current.ScaleX;
            card.RenderTransform = new ScaleTransform
            {
                ScaleX = targetScale,
                ScaleY = targetScale,
            };
            card.RenderTransformOrigin = new Windows.Foundation.Point(0.5, 0.0);

            _ = startScale;
        }
        catch (Exception ex)
        {
            App.Log($"AnimateCard failed: {ex.Message}");
        }
    }

    private void AttachKeys()
    {
        try
        {
            // The shell owns the frame, so route arrow keys through the page itself.
            Focus(FocusState.Programmatic);
        }
        catch (Exception ex)
        {
            App.Log($"AttachKeys failed: {ex.Message}");
        }
    }

    private void DetachKeys() { }

    private void OnPageKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Left) { Step(-1); e.Handled = true; }
        else if (e.Key == VirtualKey.Right) { Step(1); e.Handled = true; }
    }

    private void OnWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        var delta = e.GetCurrentPoint(CarouselCanvas).Properties.MouseWheelDelta;
        if (delta == 0) return;
        Step(delta > 0 ? -1 : 1);
        e.Handled = true;
    }

    // ---------------- actions ----------------

    private async void OnDeleteCharacterClick(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button || button.Tag is not string id) return;

        var dialog = new ContentDialog
        {
            Title = "删除角色",
            Content = "这会删除该角色的所有历史记录，操作不可撤销。",
            PrimaryButtonText = "删除",
            CloseButtonText = "取消",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = XamlRoot,
        };

        try
        {
            var result = await dialog.ShowAsync();
            if (result != ContentDialogResult.Primary) return;

            await AppServices.Instance.DeleteCharacterAsync(id);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            App.Log($"OnDeleteCharacterClick failed: {ex}");
        }
    }

    private void OnNewCharacterClick(object sender, RoutedEventArgs e)
    {
        try
        {
            (App.MainWindowInstance as MainWindow)?.NavigateTo(0);
        }
        catch (Exception ex)
        {
            App.Log($"OnNewCharacterClick failed: {ex}");
        }
    }
}
