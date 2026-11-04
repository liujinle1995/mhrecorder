using System.Collections.Specialized;
using System.ComponentModel;
using System.Numerics;
using Microsoft.UI.Composition;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using MhRecorder.App.Controls;
using MhRecorder.App.Services;
using MhRecorder.Core.ViewModels;
using Windows.System;
using Windows.UI;

namespace MhRecorder.App.Views;

/// <summary>
/// Page 2 — 角色管理: search, Cover Flow carousel, pagination.
///
/// 分工：
///   * 数据（列表 / 搜索 / 当前项 / 删除）在 <see cref="CharactersViewModel"/>；
///   * 本文件只做一件事：把卡片摆到舞台上，并用 Composition 让它们滑/转/淡。
///
/// 三条硬约束（也是上一版跳动、闪烁、重建的根因）：
///   1. 动画期间**不重建**卡片——卡片按角色 Id 缓存在 <see cref="_pool"/> 里；
///   2. 动画结束后**不再整体 Render**——只把退出的那一张摘下来，其余卡片静态提交终态；
///   3. 动画的完成以 <see cref="CompositionScopedBatch.Completed"/> 为准，
///      固定时长的计时器只做「事件没来」的兜底。
///
/// 状态机：Idle → Preparing（算目标布局）→ Animating（Composition 在跑）
///        → Finalizing（提交终态、回收退出卡片）→ Idle。
/// 任何时刻只有一个状态，输入只进队列，所以连续点击不会互相打断。
/// </summary>
public sealed partial class CharactersPage : Page
{
    /// <summary>轮播控制器状态。任何时刻只处于其中一个。</summary>
    private enum FlowState
    {
        /// <summary>没有动画在跑，画面与 <see cref="_displayedIndex"/> 一致。</summary>
        Idle,
        /// <summary>正在算目标布局、挂载临时进入的卡片。</summary>
        Preparing,
        /// <summary>Composition 动画进行中。</summary>
        Animating,
        /// <summary>动画已结束，正在提交终态并回收退出卡片。</summary>
        Finalizing,
    }

    /// <summary>一张卡片 + 它在舞台上的当前几何。卡片本身跨步骤复用。</summary>
    private sealed class CardEntry
    {
        public CardEntry(CharacterCardView card, CharacterCardViewModel vm)
        {
            Card = card;
            Vm = vm;
        }

        public CharacterCardView Card { get; }
        public CharacterCardViewModel Vm { get; }

        /// <summary>当前槽位：0 中心，±1 左右，±2 临时进出位。</summary>
        public int Slot { get; set; }

        /// <summary>已经提交给 Canvas 的左上角（相对舞台），用于算动画起点。</summary>
        public double Left { get; set; }
        public double Top { get; set; }

        public bool Attached { get; set; }

        /// <summary>LRU 时间戳，用于回收长时间没出现的卡片。</summary>
        public long LastUsed { get; set; }
    }

    /// <summary>一步动画里「某张卡要去哪」。</summary>
    private readonly record struct StepMove(CardEntry Entry, int TargetSlot, bool WrapAround);

    private const int PoolCapacity = 7;

    private readonly Dictionary<string, CardEntry> _pool = new(StringComparer.Ordinal);
    private readonly List<CardEntry> _visible = new();
    private readonly Queue<int> _pending = new();

    private FlowState _state = FlowState.Idle;
    private int _displayedIndex;
    private int _stepTarget;
    private double _fit = 1.0;
    private double _cardHeight = 424;
    private long _useClock;
    private int _cardsBuilt;

    private bool _suppressVmReaction;
    private bool _hooked;
    private bool _resetQueued;
    /// <summary>LoadAsync 期间的数据变化不再单独排队——后面会直接 HardReset 一次。</summary>
    private bool _loading;

    private CompositionScopedBatch? _batch;
    private CompositionEasingFunction? _easing;
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _fallbackTimer;

    public CharactersViewModel ViewModel { get; } = AppHost.Characters;

    public CharactersPage()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
        KeyDown += OnPageKeyDown;
        CarouselCanvas.PointerWheelChanged += OnWheelChanged;
        AutomationProperties.SetAutomationId(Stage, "CarouselStage");
    }

    // ---------------- 生命周期 ----------------

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        Hook();
        try
        {
            _loading = true;
            await ViewModel.LoadAsync();
            _loading = false;
            if (!IsLoaded) return;
            _resetQueued = false;
            HardReset();
            Focus(FocusState.Programmatic);
        }
        catch (Exception ex)
        {
            _loading = false;
            App.Log($"CharactersPage.OnLoaded failed: {ex}");
        }
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        Unhook();
        Teardown();
    }

    private void Hook()
    {
        if (_hooked) return;
        _hooked = true;
        ViewModel.PropertyChanged += OnViewModelChanged;
        ViewModel.Cards.CollectionChanged += OnCardsChanged;
    }

    private void Unhook()
    {
        if (!_hooked) return;
        _hooked = false;
        ViewModel.PropertyChanged -= OnViewModelChanged;
        ViewModel.Cards.CollectionChanged -= OnCardsChanged;
    }

    /// <summary>页面离开时把动画、计时器、卡片全部收掉，不留悬挂的合成动画。</summary>
    private void Teardown()
    {
        _pending.Clear();
        CancelAnimations();
        foreach (var entry in _pool.Values.ToList()) RemoveEntry(entry);
        _visible.Clear();
        _pool.Clear();
        _batch = null;
        if (_fallbackTimer is not null)
        {
            _fallbackTimer.Stop();
            _fallbackTimer = null;
        }
        _state = FlowState.Idle;
        _displayedIndex = 0;
    }

    // ---------------- 数据变化 ----------------

    private void OnCardsChanged(object? sender, NotifyCollectionChangedEventArgs e) => ScheduleReset();

    private void OnViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(CharactersViewModel.CurrentIndex):
                // 翻页自己会推进索引，这里不能跟着重建。
                if (_suppressVmReaction) return;
                if (_state != FlowState.Idle) return;
                if (_displayedIndex != ViewModel.CurrentIndex) ScheduleReset();
                break;

            case nameof(CharactersViewModel.HasCards):
            case nameof(CharactersViewModel.Count):
                ScheduleReset();
                break;
        }
    }

    /// <summary>
    /// 把同一轮里的多次数据变化（Clear + 多次 Add）合并成一次重建。
    /// 不合并的话搜索一次会重建三四遍，白白重新解码头像。
    /// </summary>
    private void ScheduleReset()
    {
        if (_resetQueued || _loading || !IsLoaded) return;
        _resetQueued = true;
        if (!DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
            {
                _resetQueued = false;
                if (IsLoaded) HardReset();
            }))
        {
            _resetQueued = false;
        }
    }

    // ---------------- 重建（数据变更时唯一允许整树重排的地方） ----------------

    private void HardReset()
    {
        CancelAnimations();
        _pending.Clear();
        _state = FlowState.Idle;

        var cards = ViewModel.Cards;
        _displayedIndex = Clamp(ViewModel.CurrentIndex, cards.Count);

        // 已经不在列表里的角色，卡片一并丢掉。
        foreach (var entry in _pool.Values.ToList())
        {
            if (!cards.Any(c => string.Equals(c.Id, entry.Vm.Id, StringComparison.Ordinal)))
                RemoveEntry(entry);
        }

        _visible.Clear();
        EmptyCharactersText.Visibility = cards.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        if (cards.Count == 0)
        {
            RenderDots();
            return;
        }

        if (cards.Count <= 2)
        {
            // 1–2 张没有环可转，并排展示；槽位就是序号。
            for (var i = 0; i < cards.Count; i++)
            {
                var entry = GetOrCreate(cards[i]);
                entry.Slot = i;
                Attach(entry);
                _visible.Add(entry);
            }
        }
        else
        {
            foreach (var slot in new[] { -1, 0, 1 })
            {
                var entry = GetOrCreate(cards[Wrap(_displayedIndex + slot, cards.Count)]);
                entry.Slot = slot;
                Attach(entry);
                _visible.Add(entry);
            }
        }

        EnsureMetrics();
        PrepareStage();
        ApplyStaticLayout();
        ApplyZOrder();
        TrimPool();
        RenderDots();
        App.Log($"carousel: reset (cards {cards.Count}, index {_displayedIndex}, pool {_pool.Count})");
    }

    // ---------------- 输入 ----------------

    private void OnPrevClick(object sender, RoutedEventArgs e) => Step(-1);

    private void OnNextClick(object sender, RoutedEventArgs e) => Step(1);

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

    private void OnCardDeleteRequested(object? sender, CharacterCardViewModel vm)
    {
        // 删除会走 LoadAsync → Cards 变化 → 合并成一次 HardReset，
        // HardReset 里会先取消正在跑的动画，不会出现"边删边转"。
        if (ViewModel.DeleteCommand.CanExecute(vm)) ViewModel.DeleteCommand.Execute(vm);
    }

    private void Step(int delta)
    {
        if (delta == 0) return;
        var count = ViewModel.Cards.Count;
        if (count == 0) return;

        if (count <= 2)
        {
            // 并排布局没有环形：只换选中项与圆点，卡片不需要动。
            _suppressVmReaction = true;
            try { ViewModel.Step(delta); }
            finally { _suppressVmReaction = false; }
            _displayedIndex = ViewModel.CurrentIndex;
            RenderDots();
            return;
        }

        if (_pending.Count >= CoverFlow.MaxQueuedSteps) return;
        _pending.Enqueue(delta);
        Pump();
    }

    /// <summary>队列里还有一步就开一步；动画中绝不并发启动第二条动画。</summary>
    private void Pump()
    {
        if (_state != FlowState.Idle) return;
        if (!_pending.TryDequeue(out var delta)) return;
        if (ViewModel.Cards.Count < 3) { HardReset(); return; }
        BeginStep(delta);
    }

    // ---------------- 一步动画 ----------------

    private void BeginStep(int delta)
    {
        _state = FlowState.Preparing;

        var cards = ViewModel.Cards;
        var count = cards.Count;
        var to = Wrap(_displayedIndex + delta, count);
        _stepTarget = to;

        // 视图模型跟着走：圆点、选中态、副标题都以它为准。
        _suppressVmReaction = true;
        try { ViewModel.CurrentIndex = to; }
        finally { _suppressVmReaction = false; }

        var slideMs = _pending.Count >= CoverFlow.FastSlideThreshold
            ? CoverFlow.FastSlideMs
            : CoverFlow.SlideMs;

        var enterSlot = delta > 0 ? 1 : -1;
        var enterIndex = Wrap(to + delta, count);
        var entering = _visible.FirstOrDefault(e => string.Equals(e.Vm.Id, cards[enterIndex].Id, StringComparison.Ordinal));

        var moves = new List<StepMove>(_visible.Count + 1);
        foreach (var entry in _visible) moves.Add(new StepMove(entry, entry.Slot - delta, false));

        if (entering is not null)
        {
            // 只有 3 张时，"从右边进来"的那张恰好就是"从左边出去"的那张：
            // 它是同一个对象，不能同时出现在两个槽位上。走"淡出 → 透明时换边 → 淡入"。
            moves.RemoveAll(m => ReferenceEquals(m.Entry, entering));
            moves.Add(new StepMove(entering, enterSlot, true));
        }
        else
        {
            var entry = GetOrCreate(cards[enterIndex]);
            entry.Slot = delta > 0 ? 2 : -2;
            Attach(entry);
            // 先落在起始槽位、透明，再进入：FadeIn 由通用的「读当前值 → 动画到目标」自然完成。
            CommitStatic(entry,
                CoverFlow.StateForSlot(entry.Slot).WithOpacity(0f),
                SlotLeft(entry.Slot),
                -_cardHeight / 2.0,
                _fit);
            moves.Add(new StepMove(entry, enterSlot, false));
            // 立刻纳入可见集合：FinishStep 靠它决定谁留下、谁被摘下。
            _visible.Add(entry);
        }

        var compositor = StageCompositor();
        _batch = compositor.CreateScopedBatch(CompositionBatchTypes.Animation);

        foreach (var move in moves)
        {
            var slot = move.TargetSlot;
            var target = Math.Abs(slot) > 1
                ? CoverFlow.StateForSlot(slot).WithOpacity(0f) // 退出位：滑到外侧并淡出
                : CoverFlow.StateForSlot(slot);

            // 退出中的卡片已经透明，不该再抢鼠标（否则点它会落在不可见的东西上）。
            if (Math.Abs(slot) > 1) move.Entry.Card.IsHitTestVisible = false;

            if (move.WrapAround)
                AnimateWrap(move.Entry, move.Entry.Slot, slot, target, slideMs);
            else
                AnimateTo(move.Entry, slot, target, slideMs);

            move.Entry.Slot = slot;
        }

        // 先置状态、再订阅、最后 End()：End() 有可能立刻触发 Completed，
        // 这时状态机已经就绪，且回调只做 Dispatcher 排队，不会在这里重入。
        _state = FlowState.Animating;
        _batch.Completed += OnBatchCompleted;
        _batch.End();

        App.Log($"carousel: step {delta:+#;-#;0} → {to} ({slideMs:0}ms, queued {_pending.Count})");
        ArmFallback(slideMs);
    }

    private void OnBatchCompleted(object sender, CompositionBatchCompletedEventArgs args)
    {
        // 合成器事件不保证在 UI 线程，回到 Dispatcher 再改可视化树。
        DispatcherQueue.TryEnqueue(FinishStep);
    }

    /// <summary>
    /// 兜底：正常完成靠 <see cref="CompositionScopedBatch.Completed"/>；
    /// 这个计时器只在事件没来（动画被外部打断）时把状态机拉回 Idle。
    /// </summary>
    private void ArmFallback(double slideMs)
    {
        if (_fallbackTimer is null)
        {
            _fallbackTimer = DispatcherQueue.CreateTimer();
            _fallbackTimer.IsRepeating = false;
            _fallbackTimer.Tick += OnFallbackTick;
        }

        _fallbackTimer.Stop();
        _fallbackTimer.Interval = TimeSpan.FromMilliseconds(Math.Max(CoverFlow.SettleFallbackMs, slideMs * 2));
        _fallbackTimer.Start();
    }

    private void OnFallbackTick(Microsoft.UI.Dispatching.DispatcherQueueTimer sender, object args)
    {
        sender.Stop();
        if (_state != FlowState.Animating) return;
        App.Log("carousel: batch.Completed 未到达，兜底收口");
        FinishStep();
    }

    /// <summary>提交终态：只回收到退出槽位的卡片，其余卡片原地静态提交，绝不重建。</summary>
    private void FinishStep()
    {
        if (_state != FlowState.Animating && _state != FlowState.Preparing) return;
        _state = FlowState.Finalizing;

        if (_batch is not null)
        {
            _batch.Completed -= OnBatchCompleted;
            _batch = null;
        }
        _fallbackTimer?.Stop();

        var survivors = new List<CardEntry>(_visible.Count);
        foreach (var entry in _visible)
        {
            if (Math.Abs(entry.Slot) > 1)
            {
                // 已经淡出到 0，摘下来放进缓存，下次轮到它时直接复用（头像不重新解码）。
                Detach(entry);
                continue;
            }

            CommitStatic(entry,
                CoverFlow.StateForSlot(entry.Slot),
                SlotLeft(entry.Slot),
                -_cardHeight / 2.0,
                _fit);
            survivors.Add(entry);
        }

        _visible.Clear();
        _visible.AddRange(survivors);

        _displayedIndex = _stepTarget;
        ApplyZOrder();
        TrimPool();
        RenderDots();

        _state = FlowState.Idle;
        Pump();
    }

    // ---------------- 动画 ----------------

    private void AnimateTo(CardEntry entry, int slot, CardVisualState target, double slideMs)
    {
        var card = entry.Card;
        var visual = card.Visual;

        // 起点一律读当前真实的合成状态：卡片可能停在上一步的半途（被缩放、
        // 被取消打断），不能假定它正好停在旧槽位上。
        var fromCenter = visual.CenterPoint;
        var fromScale = visual.Scale;
        var fromRotation = visual.RotationAngleInDegrees;
        var fromOpacity = visual.Opacity;

        var left = SlotLeft(slot);
        var top = -_cardHeight / 2.0;

        // 位置：先把布局坐标提交到终点，再用 CenterPoint 的支点偏移表达"还差多少"。
        // 这样动画期间不触发布局测量，也不会有 Offset 被布局覆盖的问题。
        // CenterPoint 同时是「变换支点」和「3D 位移」：把支点平移 d，整张卡片
        // 就平移 d，而旋转/缩放仍绕卡片自身中心——于是位移和旋转可以共用一条属性。
        var pivotX = (float)(CoverFlow.CardWidth / 2.0);
        var pivotY = (float)(_cardHeight / 2.0);
        var dx = (float)(entry.Left + (fromCenter.X - pivotX) - left);
        Canvas.SetLeft(card, left);
        Canvas.SetTop(card, top);
        entry.Left = left;
        entry.Top = top;

        var compositor = visual.Compositor;
        var easing = Easing(compositor);
        var duration = TimeSpan.FromMilliseconds(slideMs);

        var center = compositor.CreateVector3KeyFrameAnimation();
        center.InsertKeyFrame(0f, new Vector3(pivotX + dx, pivotY, fromCenter.Z));
        center.InsertKeyFrame(1f, new Vector3(pivotX, pivotY, target.DepthZ), easing);
        center.Duration = duration;
        center.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("CenterPoint", center);

        var scaled = (float)(target.Scale * _fit);
        var scale = compositor.CreateVector3KeyFrameAnimation();
        scale.InsertKeyFrame(0f, fromScale);
        scale.InsertKeyFrame(1f, new Vector3(scaled, scaled, 1f), easing);
        scale.Duration = duration;
        scale.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("Scale", scale);

        var rotation = compositor.CreateScalarKeyFrameAnimation();
        rotation.InsertKeyFrame(0f, fromRotation);
        rotation.InsertKeyFrame(1f, target.RotationYDegrees, easing);
        rotation.Duration = duration;
        rotation.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("RotationAngleInDegrees", rotation);

        var opacity = compositor.CreateScalarKeyFrameAnimation();
        opacity.InsertKeyFrame(0f, fromOpacity);
        opacity.InsertKeyFrame(1f, target.Opacity, easing);
        opacity.Duration = TimeSpan.FromMilliseconds(Math.Min(CoverFlow.FadeMs, slideMs));
        opacity.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("Opacity", opacity);
    }


    /// <summary>
    /// 3 张角色的环：左边那张要变成右边那张，中间没有第四张可用。
    /// 于是先往外淡出，在完全透明的那一刻换到另一侧，再淡入——
    /// 全程只有一张卡片、一条动画，不需要第二张卡片接力。
    /// </summary>
    private void AnimateWrap(CardEntry entry, int fromSlot, int toSlot, CardVisualState target, double slideMs)
    {
        var card = entry.Card;
        var visual = card.Visual;

        var left = SlotLeft(toSlot);
        var top = -_cardHeight / 2.0;
        var startLeft = entry.Left;

        var outerLeft = SlotLeft(fromSlot < 0 ? -2 : 2);
        var farLeft = SlotLeft(toSlot > 0 ? 2 : -2);

        Canvas.SetLeft(card, left);
        Canvas.SetTop(card, top);
        entry.Left = left;
        entry.Top = top;

        var d0 = (float)(startLeft - left);
        var d1 = (float)(outerLeft - left);
        var d2 = (float)(farLeft - left);

        var compositor = visual.Compositor;
        var easing = Easing(compositor);
        var duration = TimeSpan.FromMilliseconds(slideMs);

        var pivotX = (float)(CoverFlow.CardWidth / 2.0);
        var pivotY = (float)(_cardHeight / 2.0);

        var center = compositor.CreateVector3KeyFrameAnimation();
        center.InsertKeyFrame(0.00f, new Vector3(pivotX + d0, pivotY, CoverFlow.SideDepthZ));
        center.InsertKeyFrame(0.26f, new Vector3(pivotX + d1, pivotY, CoverFlow.SideDepthZ), easing);
        center.InsertKeyFrame(0.28f, new Vector3(pivotX + d2, pivotY, CoverFlow.SideDepthZ));
        center.InsertKeyFrame(1.00f, new Vector3(pivotX, pivotY, target.DepthZ), easing);
        center.Duration = duration;
        center.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("CenterPoint", center);

        var scaled = (float)(target.Scale * _fit);
        var scale = compositor.CreateVector3KeyFrameAnimation();
        scale.InsertKeyFrame(0f, visual.Scale);
        scale.InsertKeyFrame(1f, new Vector3(scaled, scaled, 1f), easing);
        scale.Duration = duration;
        scale.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("Scale", scale);

        // 旋转在换边的那一帧翻转：透明度此时为 0，看不到突变。
        var rotation = compositor.CreateScalarKeyFrameAnimation();
        rotation.InsertKeyFrame(0.00f, visual.RotationAngleInDegrees);
        rotation.InsertKeyFrame(0.26f, visual.RotationAngleInDegrees);
        rotation.InsertKeyFrame(0.28f, target.RotationYDegrees);
        rotation.InsertKeyFrame(1.00f, target.RotationYDegrees);
        rotation.Duration = duration;
        rotation.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("RotationAngleInDegrees", rotation);

        var opacity = compositor.CreateScalarKeyFrameAnimation();
        opacity.InsertKeyFrame(0.00f, CoverFlow.SideOpacity);
        opacity.InsertKeyFrame(0.24f, 0f);
        opacity.InsertKeyFrame(0.30f, 0f);
        opacity.InsertKeyFrame(1.00f, target.Opacity, easing);
        opacity.Duration = duration;
        opacity.StopBehavior = AnimationStopBehavior.SetToFinalValue;
        visual.StartAnimation("Opacity", opacity);
    }

    private CompositionEasingFunction Easing(Compositor compositor) =>
        _easing ??= compositor.CreateCubicBezierEasingFunction(CoverFlow.EaseControl1, CoverFlow.EaseControl2);

    // ---------------- 布局 ----------------

    private double CardAreaHeight =>
        CarouselHost.ActualHeight > 0 ? CarouselHost.ActualHeight : CarouselCanvas.ActualHeight;

    private double StageWidth =>
        CarouselCanvas.ActualWidth > 0 ? CarouselCanvas.ActualWidth : 900;

    /// <summary>舞台纵向原点的位置：卡片中心线。透视以它为消失点，缩放才对称。</summary>
    private double StageY => CoverFlow.TopPadding + _cardHeight * _fit / 2.0;

    /// <summary>
    /// 某个槽位的卡片左上角（舞台坐标）。间距同样乘上适配比例：
    /// 窗口变矮时卡片整体缩小，槽位间距不缩就会出现越来越大的缝。
    /// </summary>
    private double SlotLeft(int slot) =>
        CoverFlow.SlotX(slot) * _fit - CoverFlow.CardWidth / 2.0;

    private void EnsureMetrics()
    {
        if (_visible.Count > 0) _cardHeight = _visible[0].Card.CardHeight;
        else if (_pool.Count > 0) _cardHeight = _pool.Values.First().Card.CardHeight;

        var available = CardAreaHeight;
        if (available <= 0 || _cardHeight <= 0)
        {
            _fit = 1.0;
            return;
        }

        // 上下留白 + 卡片下沿的阴影伸出量都要让出来，否则阴影会顶到分页点、
        // 甚至把页面撑出滚动条。
        var usable = Math.Max(160.0, available - CoverFlow.TopPadding * 2 - CoverFlow.ShadowOverhang);
        _fit = Math.Min(1.0, usable / _cardHeight);
    }

    /// <summary>
    /// 给零尺寸舞台一个透视矩阵（M34 = -1/d）：卡片在 Z 上抬升 60 时按
    /// 1/(1-60/d) 放大，Y 轴旋转才有近大远小。舞台摆在卡片区中心，
    /// 因此消失点就是卡片中心线，左右两侧的透视是对称的。
    /// </summary>
    private void PrepareStage()
    {
        Canvas.SetLeft(Stage, StageWidth / 2.0);
        Canvas.SetTop(Stage, StageY);

        var perspective = Matrix4x4.Identity;
        perspective.M34 = -1f / CoverFlow.PerspectiveDistance;
        Stage.TransformMatrix = perspective;
    }

    private void ApplyStaticLayout()
    {
        if (_visible.Count == 0) return;

        var count = ViewModel.Cards.Count;
        if (count is > 0 and <= 2)
        {
            var totalWidth = count == 1
                ? CoverFlow.CardWidth
                : CoverFlow.CardWidth * 2 + CoverFlow.FlatGap;
            var startX = -totalWidth / 2.0;
            // 下面统一用 totalWidth * flatFit 计算半宽，间距自然跟着缩放。

            var available = CardAreaHeight;
            var centerY = available > 0 ? available / 2.0 : StageY;
            var top = centerY - StageY - _cardHeight / 2.0;

            // 并排时还要保证整排不超出可视宽度。
            var hostWidth = StageWidth;
            var flatFit = _fit;
            if (hostWidth > 0 && totalWidth > 0 && totalWidth * flatFit > hostWidth)
                flatFit = Math.Min(flatFit, hostWidth / totalWidth);

            for (var i = 0; i < _visible.Count; i++)
            {
                // 间距按 flatFit 一起缩：卡片缩小后间距不缩，两张卡就会越飘越远。
                var left = count == 1
                    ? -CoverFlow.CardWidth / 2.0
                    : (startX + i * (CoverFlow.CardWidth + CoverFlow.FlatGap)) * flatFit;
                CommitStatic(_visible[i], CardVisualState.Flat, left, top, flatFit);
            }
            return;
        }

        foreach (var entry in _visible)
        {
            CommitStatic(entry,
                CoverFlow.StateForSlot(entry.Slot),
                SlotLeft(entry.Slot),
                -_cardHeight / 2.0,
                _fit);
        }
    }

    /// <summary>
    /// 把一张卡片放到静态终态：先停掉它身上的合成动画，再写布局坐标与视觉属性。
    /// 写法与动画最后一帧完全一致，所以这一步在屏幕上看不出任何变化。
    /// </summary>
    private void CommitStatic(CardEntry entry, CardVisualState state, double left, double top, double fit)
    {
        var card = entry.Card;
        var visual = card.Visual;
        visual.StopAnimation("CenterPoint");
        visual.StopAnimation("Scale");
        visual.StopAnimation("RotationAngleInDegrees");
        visual.StopAnimation("Opacity");
        card.ShadowVisual.StopAnimation("Opacity");

        Canvas.SetLeft(card, left);
        Canvas.SetTop(card, top);
        entry.Left = left;
        entry.Top = top;

        card.SetVisualState(state, fit);
    }

    /// <summary>中心卡片压在最上面；越靠外越靠下，退出的卡片在最底层。</summary>
    private void ApplyZOrder()
    {
        foreach (var entry in _pool.Values)
            Canvas.SetZIndex(entry.Card, ZRank(entry.Slot));
    }

    private static int ZRank(int slot) => Math.Abs(slot) switch
    {
        0 => 30,
        1 => 20,
        _ => 10,
    };

    // ---------------- 卡片池 ----------------

    private CardEntry GetOrCreate(CharacterCardViewModel vm)
    {
        if (_pool.TryGetValue(vm.Id, out var existing))
        {
            if (ReferenceEquals(existing.Vm, vm))
            {
                existing.LastUsed = ++_useClock;
                return existing;
            }
            // 同一个角色但视图模型换了实例（重新加载过），旧卡片的文案已经过期。
            RemoveEntry(existing);
        }

        var card = new CharacterCardView(vm, CoverFlow.CardWidth);
        card.DeleteRequested += OnCardDeleteRequested;

        var entry = new CardEntry(card, vm) { LastUsed = ++_useClock };
        _pool[vm.Id] = entry;
        _cardsBuilt++;
        // 这张日志是「动画期间没有重建卡片」的硬证据：轮播 N 步后总数应当很小。
        App.Log($"carousel: card built for {vm.Nickname} (pool {_pool.Count}, total built {_cardsBuilt})");
        return entry;
    }

    private void RemoveEntry(CardEntry entry)
    {
        Detach(entry);
        entry.Card.DeleteRequested -= OnCardDeleteRequested;
        if (_pool.TryGetValue(entry.Vm.Id, out var current) && ReferenceEquals(current, entry))
            _pool.Remove(entry.Vm.Id);
        _visible.Remove(entry);
    }

    private void Attach(CardEntry entry)
    {
        if (entry.Attached) return;
        Stage.Children.Add(entry.Card);
        entry.Attached = true;
        entry.Card.IsHitTestVisible = true;
    }

    private void Detach(CardEntry entry)
    {
        if (!entry.Attached) return;
        Stage.Children.Remove(entry.Card);
        entry.Attached = false;
        entry.Card.IsHitTestVisible = false;
    }

    /// <summary>只保留最近用过的若干张卡片：轮播回退时头像不必重新解码。</summary>
    private void TrimPool()
    {
        var capacity = Math.Min(PoolCapacity, Math.Max(3, ViewModel.Cards.Count));
        if (_pool.Count <= capacity) return;

        foreach (var entry in _pool.Values
                     .Where(e => !_visible.Contains(e))
                     .OrderBy(e => e.LastUsed)
                     .ToList())
        {
            if (_pool.Count <= capacity) break;
            RemoveEntry(entry);
        }
    }

    // ---------------- 其他 ----------------

    private void CancelAnimations()
    {
        if (_batch is not null)
        {
            _batch.Completed -= OnBatchCompleted;
            _batch = null;
        }
        _fallbackTimer?.Stop();

        foreach (var entry in _pool.Values)
        {
            var visual = entry.Card.Visual;
            // 先把正在动的值冻结成静态值，再停动画，
            // 否则卡片会瞬间弹回上一次提交的位置。
            var center = visual.CenterPoint;
            var scale = visual.Scale;
            var rotation = visual.RotationAngleInDegrees;
            var opacity = visual.Opacity;

            visual.StopAnimation("CenterPoint");
            visual.StopAnimation("Scale");
            visual.StopAnimation("RotationAngleInDegrees");
            visual.StopAnimation("Opacity");
            entry.Card.ShadowVisual.StopAnimation("Opacity");

            // 把正在动的值写回 visual 作为新的静态值：位置与形态都停在原处，不会弹回去。
            visual.CenterPoint = center;
            visual.Scale = scale;
            visual.RotationAngleInDegrees = rotation;
            visual.Opacity = opacity;
        }

        // 半途退出的卡片不应该继续留在台上。
        foreach (var entry in _visible.Where(e => Math.Abs(e.Slot) > 1).ToList())
            Detach(entry);
        _visible.RemoveAll(e => Math.Abs(e.Slot) > 1);
    }

    /// <summary>窗口尺寸变化：动画中的先收口，再按新尺寸重排。</summary>
    private void OnCarouselSizeChanged(object sender, SizeChangedEventArgs e) => Relayout();

    private void OnCarouselHostSizeChanged(object sender, SizeChangedEventArgs e)
    {
        CarouselClip.Rect = new Windows.Foundation.Rect(0, 0, e.NewSize.Width, e.NewSize.Height);
        Relayout();
    }

    private void Relayout()
    {
        if (!IsLoaded) return;
        if (ViewModel.Cards.Count == 0) return;

        if (_state != FlowState.Idle)
        {
            // 尺寸变了，进行中的动画的绝对坐标已经失效：直接收到终点再重排。
            _pending.Clear();
            CancelAnimations();
            _displayedIndex = _stepTarget >= 0 && _stepTarget < ViewModel.Cards.Count
                ? _stepTarget
                : Clamp(ViewModel.CurrentIndex, ViewModel.Cards.Count);
            _state = FlowState.Idle;
            HardReset();
            return;
        }

        EnsureMetrics();
        PrepareStage();
        ApplyStaticLayout();
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
    /// 取应用的合成器。**不能**用 GetElementVisual(Stage)：Stage 上设了
    /// TransformMatrix（透视），框架会因此拒绝把它的 visual 交给 Composition。
    /// </summary>
    private Compositor StageCompositor() =>
        ElementCompositionPreview.GetElementVisual(CarouselCanvas).Compositor;

    private static int Wrap(int index, int count) => ((index % count) + count) % count;

    private static int Clamp(int index, int count) =>
        count <= 0 ? 0 : Math.Clamp(index, 0, count - 1);
}
