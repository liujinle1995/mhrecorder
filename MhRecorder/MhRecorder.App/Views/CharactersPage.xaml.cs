using System.Collections.Specialized;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using MhRecorder.App.Controls;
using MhRecorder.App.Services;
using MhRecorder.Core.ViewModels;
using Windows.System;
using Windows.UI;

namespace MhRecorder.App.Views;

/// <summary>
/// Page 2 — 角色管理: search, horizontal carousel, pagination.
///
/// 数据（列表 / 搜索 / 当前项 / 删除）在 <see cref="CharactersViewModel"/>；
/// 这里只剩下"把三张卡片摆在 Canvas 上并让它们滑起来"——纯粹的视图工作。
/// </summary>
public sealed partial class CharactersPage : Page
{
    private const double CardWidth = 316;
    private const double CardGap = 26;

    /// <summary>A card on the canvas, tagged with its carousel slot.</summary>
    private sealed record CardSlot(CharacterCardView Card, int Offset, int Target);

    private readonly List<CardSlot> _cardSlots = new();

    /// <summary>True while a slide animation owns the canvas.</summary>
    private bool _animating;

    private const int SlideDurationMs = 260;

    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _settleTimer;

    /// <summary>True while a step is moving the cursor and animating the trio.</summary>
    private bool _stepping;

    public CharactersViewModel ViewModel { get; } = AppHost.Characters;

    public CharactersPage()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
        KeyDown += OnPageKeyDown;
        CarouselCanvas.PointerWheelChanged += OnWheelChanged;

        ViewModel.PropertyChanged += OnViewModelChanged;
        ViewModel.Cards.CollectionChanged += OnCardsChanged;
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            await ViewModel.LoadAsync();
            Render();
            Focus(FocusState.Programmatic);
        }
        catch (Exception ex)
        {
            App.Log($"CharactersPage.OnLoaded failed: {ex}");
        }
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        ViewModel.PropertyChanged -= OnViewModelChanged;
        ViewModel.Cards.CollectionChanged -= OnCardsChanged;
    }

    private void OnViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(CharactersViewModel.CurrentIndex):
                // A page step drives its own animation; rendering here would
                // snap the canvas to the new arrangement before the slide starts.
                if (!_stepping) Render();
                break;
            case nameof(CharactersViewModel.HasCards):
            case nameof(CharactersViewModel.Count):
                Render();
                break;
        }
    }

    private void OnCardsChanged(object? sender, NotifyCollectionChangedEventArgs e) => Render();

    private void OnCarouselSizeChanged(object sender, SizeChangedEventArgs e) => Render();

    /// <summary>
    /// A Canvas never clips its children, so tall centre cards would spill over
    /// the pagination dots. Clip the host Grid to its own bounds.
    /// </summary>
    private void OnCarouselHostSizeChanged(object sender, SizeChangedEventArgs e)
    {
        CarouselClip.Rect = new Windows.Foundation.Rect(0, 0, e.NewSize.Width, e.NewSize.Height);
        Render();
    }

    // ---------------- carousel ----------------

    private void Render()
    {
        // A slide is in flight: the settle timer re-renders afterwards, so
        // dropping this call never leaves a stale geometry on screen.
        if (_animating) return;

        CarouselCanvas.Children.Clear();
        _cardSlots.Clear();
        EmptyCharactersText.Visibility = ViewModel.Cards.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        RenderDots();

        if (ViewModel.Cards.Count == 0) return;

        var boundsWidth = CarouselCanvas.ActualWidth > 0 ? CarouselCanvas.ActualWidth : 900;
        var centerX = boundsWidth / 2;

        // Layout rule:
        //   * 1–2 characters  -> flat, centred; nothing to page through.
        //   * 3+ characters   -> circular carousel: previous / centre / next.
        if (ViewModel.Cards.Count <= 2)
        {
            RenderFlat(centerX);
            return;
        }

        // Paint order: left neighbour, centre, then right neighbour last so it
        // lands above both of them.
        var slots = new (int Offset, bool IsCenter)[] { (-1, false), (0, true), (1, false) };
        var count = ViewModel.Cards.Count;

        foreach (var slot in slots)
        {
            // Wrap so every position always has a card.
            var target = ((ViewModel.CurrentIndex + slot.Offset) % count + count) % count;

            var card = BuildCard(ViewModel.Cards[target], slot.IsCenter);
            _cardSlots.Add(new CardSlot(card, slot.Offset, target));

            var available = CarouselHost.ActualHeight > 0
                ? CarouselHost.ActualHeight
                : CarouselCanvas.ActualHeight;
            var topPad = slot.IsCenter ? 8.0 : 34.0;
            var fit = 1.0;
            if (available > 0)
            {
                var cardHeight = card.ActualHeight > 0 ? card.ActualHeight : MeasureCardHeight(card);
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
                // Pull the neighbours in so a meaningful slice of each is visible.
                var neighbour = Math.Min(fit, 0.955);
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
    /// With so few cards there is nothing to page through, so the mock's
    /// "peek one card either side" layout reads as broken.
    /// </summary>
    private void RenderFlat(double centerX)
    {
        var count = ViewModel.Cards.Count;
        var available = CarouselHost.ActualHeight > 0
            ? CarouselHost.ActualHeight
            : CarouselCanvas.ActualHeight;

        var totalWidth = count == 1 ? CardWidth : CardWidth * 2 + CardGap;
        var startX = centerX - totalWidth / 2.0;

        for (var i = 0; i < count; i++)
        {
            var card = BuildCard(ViewModel.Cards[i], isCenter: true);

            var cardHeight = card.ActualHeight > 0 ? card.ActualHeight : MeasureCardHeight(card);

            var fit = 1.0;
            if (available > 0 && cardHeight > 0)
            {
                var usable = Math.Max(80.0, available - 12.0);
                fit = Math.Min(1.0, usable / cardHeight);
            }

            var hostWidth = CarouselCanvas.ActualWidth;
            if (hostWidth > 0 && totalWidth > 0 && totalWidth * fit > hostWidth)
                fit = Math.Min(fit, hostWidth / totalWidth);

            card.RenderTransform = new ScaleTransform { ScaleX = fit, ScaleY = fit };
            card.RenderTransformOrigin = new Windows.Foundation.Point(0.5, 0.0);

            var left = count == 1 ? centerX - CardWidth / 2.0 : startX + i * (CardWidth + CardGap);

            // Centre the row vertically: with one or two cards the host is far
            // taller than the card, and pinning it to the top leaves a dead band.
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

        var count = ViewModel.Cards.Count;
        if (count == 0) return;
        var maxDots = Math.Min(count, 8);
        var activeIndex = Math.Min(ViewModel.CurrentIndex, maxDots - 1);

        for (var i = 0; i < maxDots; i++)
        {
            var active = i == activeIndex;
            DotsPanel.Children.Add(new Border
            {
                Width = active ? 16 : 7,
                Height = 7,
                CornerRadius = new CornerRadius(3.5),
                Background = active
                    ? Theme.Resource("BrandBlueBrush", new SolidColorBrush(Color.FromArgb(255, 0, 116, 255)))
                    : new SolidColorBrush(Color.FromArgb(255, 0xD5, 0xD7, 0xDD)),
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(0, 0, 3, 0),
            });
        }
    }

    /// <summary>
    /// Measure a card that has not been laid out yet: before it is in the visual
    /// tree <c>ActualHeight</c> is 0.
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

    private CharacterCardView BuildCard(CharacterCardViewModel vm, bool isCenter)
    {
        var card = new CharacterCardView(vm, isCenter, CardWidth);
        card.DeleteRequested += (_, model) =>
        {
            if (ViewModel.DeleteCommand.CanExecute(model)) ViewModel.DeleteCommand.Execute(model);
        };
        return card;
    }

    // ---------------- navigation ----------------

    private void OnPrevClick(object sender, RoutedEventArgs e) => Step(-1);

    private void OnNextClick(object sender, RoutedEventArgs e) => Step(1);

    private void Step(int delta)
    {
        if (ViewModel.Cards.Count == 0) return;

        var from = ViewModel.CurrentIndex;
        _stepping = true;
        try
        {
            ViewModel.Step(delta);
        }
        finally
        {
            _stepping = false;
        }

        // A slide only makes sense between two distinct cards.
        if (ViewModel.Cards.Count > 2 && _cardSlots.Count > 0 && ViewModel.CurrentIndex != from)
            SlideTo(ViewModel.CurrentIndex, delta);
        else
            Render();
    }

    /// <summary>
    /// Animate from the current arrangement to the one centred on
    /// <paramref name="targetIndex"/>.
    ///
    /// Cards are reused rather than rebuilt: each existing card is moved to the
    /// slot it should occupy after the step, using a compositor animation, so
    /// paging reads as motion rather than a jump cut.
    /// </summary>
    private void SlideTo(int targetIndex, int delta)
    {
        var count = ViewModel.Cards.Count;
        var boundsWidth = CarouselCanvas.ActualWidth > 0 ? CarouselCanvas.ActualWidth : 900;
        var centerX = boundsWidth / 2;
        var peek = (CardWidth + CardGap) * 1.06;

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
        var entering = BuildCard(ViewModel.Cards[enteringIndex], isCenter: false);
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

        var storyboard = new Microsoft.UI.Xaml.Media.Animation.Storyboard();
        storyboard.Children.Add(anim);
        storyboard.Begin();
    }

    /// <summary>
    /// Re-layout once the slide has finished. A single one-shot timer keeps
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
    /// expressed as a translation on the element's visual: the final position is
    /// committed immediately and the animated translation starts at the inverse
    /// delta and eases to zero.
    /// </summary>
    private void AnimateCard(CharacterCardView card, double centerX, double peek,
        int slotOffset, double targetScale)
    {
        try
        {
            var targetLeft = centerX + slotOffset * peek - CardWidth / 2;
            var targetTop = slotOffset == 0 ? 8.0 : 34.0;

            var startLeft = Canvas.GetLeft(card);
            if (double.IsNaN(startLeft)) startLeft = targetLeft;

            var dx = startLeft - targetLeft; // inverse delta, eases to 0

            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(card);

            var offsetAnim = visual.Compositor.CreateVector3KeyFrameAnimation();
            var easing = visual.Compositor.CreateCubicBezierEasingFunction(
                new System.Numerics.Vector2(0.22f, 0.61f),
                new System.Numerics.Vector2(0.36f, 1f));
            offsetAnim.InsertKeyFrame(0f, new System.Numerics.Vector3((float)dx, 0f, 0f));
            offsetAnim.InsertKeyFrame(1f, System.Numerics.Vector3.Zero, easing);
            offsetAnim.Duration = TimeSpan.FromMilliseconds(SlideDurationMs);
            offsetAnim.Target = "Translation";
            visual.StartAnimation("Offset", offsetAnim);

            // Commit the destination geometry so the animated visual lands where
            // the static layout expects it.
            Canvas.SetLeft(card, targetLeft);
            Canvas.SetTop(card, targetTop);

            card.RenderTransform = new ScaleTransform { ScaleX = targetScale, ScaleY = targetScale };
            card.RenderTransformOrigin = new Windows.Foundation.Point(0.5, 0.0);
        }
        catch (Exception ex)
        {
            App.Log($"AnimateCard failed: {ex.Message}");
        }
    }

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

    private void OnNewCharacterClick(object sender, RoutedEventArgs e)
    {
        if (ViewModel.NewCharacterCommand.CanExecute(null))
            ViewModel.NewCharacterCommand.Execute(null);
    }
}
