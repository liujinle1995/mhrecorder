using System.Numerics;

namespace MhRecorder.App.Controls;

/// <summary>
/// Cover Flow 的全部可调参数。**所有**时长、缩放、角度、深度、透明度、
/// 槽位间距与缓动控制点都只在这里定义一次，页面与卡片两侧都从这里取值，
/// 避免同一个数字出现在多个方法里各自漂移。
/// </summary>
internal static class CoverFlow
{
    // ---------------- 几何 ----------------

    /// <summary>卡片宽度（逻辑像素，设计稿 316）。</summary>
    public const double CardWidth = 316;

    /// <summary>1–2 张卡片并排布局时的间距。</summary>
    public const double FlatGap = 26;

    /// <summary>卡片区顶部留白（同时也是底部留白）。</summary>
    public const double TopPadding = 8;

    /// <summary>阴影带伸出卡片下沿的高度，算高度适配时要把它留出来。</summary>
    public const double ShadowOverhang = 22;

    /// <summary>
    /// 相邻槽位的水平中心距。取 0.95 倍卡宽：旋转后的侧卡横向占宽约
    /// 0.88×cos(12°)=0.86 倍，所以两卡视觉上刚好挨着而不互相遮挡
    /// （遮挡会让侧卡的「删除」按钮点不到）。
    /// </summary>
    public const double SlotStep = CardWidth * 0.95;

    /// <summary>±2 槽位在 ±1 之外再外扩的距离。</summary>
    public const double OuterSlotStep = CardWidth * 0.55;

    /// <summary>
    /// 透视距离（逻辑像素）。Y 轴旋转必须配合透视才看得出来：没有透视时
    /// 12° 旋转只等于 2% 的横向压缩，肉眼不可见。中心卡片 Z=60 时约放大 3.9%。
    /// </summary>
    public const float PerspectiveDistance = 1600f;

    // ---------------- 时长 ----------------

    /// <summary>位移 / 缩放 / 旋转 / 深度 的统一时长。</summary>
    public const double SlideMs = 420;

    /// <summary>淡入淡出时长。</summary>
    public const double FadeMs = 250;

    /// <summary>队列积压时（连点、滚轮狂转）单步缩短的时长，避免越点越滞后。</summary>
    public const double FastSlideMs = 240;

    /// <summary>积压超过这个步数就切到 <see cref="FastSlideMs"/>。</summary>
    public const int FastSlideThreshold = 2;

    /// <summary>队列上限，防止滚轮一直滚把队列堆到几十步。</summary>
    public const int MaxQueuedSteps = 12;

    /// <summary>
    /// 兜底超时：正常完成靠 CompositionScopedBatch.Completed，
    /// 计时器只在事件没来（被外部打断、合成器异常）时把状态机拉回 Idle。
    /// </summary>
    public const double SettleFallbackMs = 1500;

    // ---------------- 视觉状态 ----------------

    public const double CenterScale = 1.00;
    public const double SideScale = 0.88;

    /// <summary>侧卡绕 Y 轴的旋转角。左右符号相反，内侧边向后（形成弧面）。</summary>
    public const float SideAngleDegrees = 12f;

    public const float CenterDepthZ = 60f;
    public const float SideDepthZ = 0f;

    public const float CenterOpacity = 1.00f;
    public const float SideOpacity = 0.82f;

    // ---------------- 阴影 ----------------
    // 阴影是卡片内容里的一块渐变板（见 CharacterCardView），跟着卡片一起缩放/旋转。
    // 不用 ThemeShadow：它需要 XAML 侧的 Translation 提供 Z 深度，而本卡片的
    // 三维变换全部由 Composition 驱动，两者不能共存——实测完全不出阴影。
    // 浓度沿同一条动画曲线过渡，所以「中心较强 / 侧边较弱」是连续的。

    public const float CenterShadowStrength = 1.00f;
    public const float SideShadowStrength = 0.42f;

    /// <summary>Ease Out：起步快、收尾缓。x 是时间轴，y 是进度。</summary>
    public static readonly Vector2 EaseControl1 = new(0.16f, 0.84f);
    public static readonly Vector2 EaseControl2 = new(0.30f, 1.00f);

    // ---------------- 推导 ----------------

    /// <summary>槽位相对舞台中心的水平偏移。0 = 中心，负 = 左。</summary>
    public static double SlotX(int slot) =>
        slot == 0 ? 0 : Math.Sign(slot) * (SlotStep + (Math.Abs(slot) - 1) * OuterSlotStep);

    /// <summary>某个槽位的静止视觉状态（不含高度适配系数）。</summary>
    public static CardVisualState StateForSlot(int slot) => slot switch
    {
        0 => new CardVisualState(CenterScale, 0f, CenterDepthZ, CenterOpacity, CenterShadowStrength),
        < 0 => new CardVisualState(SideScale, SideAngleDegrees, SideDepthZ, SideOpacity, SideShadowStrength),
        _ => new CardVisualState(SideScale, -SideAngleDegrees, SideDepthZ, SideOpacity, SideShadowStrength),
    };
}

/// <summary>
/// 一张卡片的视觉状态：只有数值，没有任何 UI 对象。
/// 同一个值既用于「动画终点」，也用于「动画结束后静态提交」，
/// 两端因此永远一致——这是消除结束瞬间跳变的前提。
/// </summary>
/// <param name="Scale">缩放（还会再乘上高度适配系数）。</param>
/// <param name="RotationYDegrees">绕 Y 轴的角度，正数=右边缘向后。</param>
/// <param name="DepthZ">Z 轴深度，正值靠近观察者（同时驱动 ThemeShadow 的深浅）。</param>
/// <param name="Opacity">整体透明度。</param>
public readonly record struct CardVisualState(
    double Scale,
    float RotationYDegrees,
    float DepthZ,
    float Opacity,
    float ShadowStrength = 0.42f)
{
    /// <summary>1–2 张卡片并排展示用的状态：不旋转、不抬升。</summary>
    public static CardVisualState Flat { get; } = new(1.0, 0f, 0f, 1f, 0.55f);

    public CardVisualState WithOpacity(float opacity) => this with { Opacity = opacity };
}
