using System.Numerics;
using Microsoft.UI;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.UI.Xaml.Shapes;
using MhRecorder.Core.ViewModels;
using Windows.Storage.Streams;
using Windows.UI;
using MhRecorder.App;

namespace MhRecorder.App.Controls;

/// <summary>
/// 角色管理页上的一张卡片。
///
/// 只有一种结构：头像、昵称、等级、气血/魔法、战斗属性、货币、删除按钮。
/// 中心位与侧位**不再**是两种不同的卡片——差异只体现在
/// <see cref="SetVisualState"/> 写进去的 Scale / Rotation / CenterPoint(含 Z) / Opacity 上。
/// 这样轮播时一张卡片可以从侧位走到中心位再走回去，全程不重建、头像只解码一次。
/// </summary>
public sealed class CharacterCardView : UserControl
{
    private static readonly SolidColorBrush TextSecondary = new(Color.FromArgb(255, 0x6B, 0x6F, 0x76));
    private static readonly SolidColorBrush TextTertiary = new(Color.FromArgb(255, 0x9A, 0xA0, 0xA6));
    private static readonly SolidColorBrush GoldAccent = new(Color.FromArgb(255, 0xF6, 0xB8, 0x2B));
    private static readonly SolidColorBrush SilverAccent = new(Color.FromArgb(255, 0x8A, 0x93, 0xA5));
    private static readonly SolidColorBrush SuccessGreen = new(Color.FromArgb(255, 0x1F, 0xB4, 0x61));
    private static readonly SolidColorBrush DangerRed = new(Color.FromArgb(255, 0xE5, 0x48, 0x4D));
    private static readonly SolidColorBrush ChipBackground = new(Color.FromArgb(255, 0xF2, 0xF3, 0xF5));
    private static readonly SolidColorBrush AvatarTileBackground = new(Color.FromArgb(255, 0xE9, 0xED, 0xF6));

    /// <summary>内容设计高度，量不到时用它兜底（316 宽卡片约 424）。</summary>
    private const double FallbackHeight = 424;

    /// <summary>阴影渐变带的高度：一半藏在卡片后面，一半露出来做投影。</summary>
    private const double ShadowBandHeight = 44;

    private readonly Border _frame;
    private readonly Border _shadowPlate;
    private bool _avatarStarted;
    private Visual? _visual;
    private Visual? _shadowVisual;

    /// <summary>Raised when the user clicks 删除 on this card.</summary>
    public event EventHandler<CharacterCardViewModel>? DeleteRequested;

    public CharacterCardView(CharacterCardViewModel vm, double width)
    {
        ViewModel = vm;
        Width = width;

        // WinUI's Border is sealed, so the card is a UserControl wrapping one.
        // Everything visual (corner radius, hairline, shadow) stays on the Border.
        var frame = new Border
        {
            Background = Theme.Resource("CardBackgroundBrush", new SolidColorBrush(Colors.White)),
            BorderBrush = Theme.Resource("CardBorderBrush", new SolidColorBrush(Color.FromArgb(255, 0xE5, 0xE6, 0xEB))),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(14),
            Padding = new Thickness(18, 18, 18, 16),
            Child = BuildContent(vm),
        };
        _frame = frame;

        // 阴影板：一条贴在卡片下沿、比卡片窄一点的渐变带。上半截被卡片盖住，
        // 露在下面的那一截（最强处正好贴着卡片边缘，向下 22px 渐隐）就是投影。
        // 它是卡片内容的一部分，随卡片一起缩放/旋转；浓度由
        // CardVisualState.ShadowStrength 驱动，切换时与位移同步过渡。
        _shadowPlate = new Border
        {
            Height = ShadowBandHeight,
            VerticalAlignment = VerticalAlignment.Bottom,
            Margin = new Thickness(12, 0, 12, -(ShadowBandHeight / 2)),
            CornerRadius = new CornerRadius(ShadowBandHeight / 2),
            Background = new LinearGradientBrush
            {
                StartPoint = new Windows.Foundation.Point(0.5, 0),
                EndPoint = new Windows.Foundation.Point(0.5, 1),
                GradientStops =
                {
                    new GradientStop { Color = Color.FromArgb(0, 0, 0, 0), Offset = 0.0 },
                    new GradientStop { Color = Color.FromArgb(58, 0, 0, 0), Offset = 0.5 },
                    new GradientStop { Color = Color.FromArgb(0, 0, 0, 0), Offset = 1.0 },
                },
            },
            Opacity = CoverFlow.SideShadowStrength,
        };

        var root = new Grid();
        root.Children.Add(_shadowPlate);
        root.Children.Add(frame);
        Content = root;

        // 内容与宽度都确定，所以高度对同宽卡片恒定；页面只需要量一次。
        // 这里直接量已经装好内容的 Border：进入可视化树之前 ActualHeight 是 0，
        // 但 DesiredSize 是有效的。
        _frame.Measure(new Windows.Foundation.Size(width, double.PositiveInfinity));
        var measured = _frame.DesiredSize.Height;
        CardHeight = measured > 0 ? measured : FallbackHeight;

        // 注意：**不要**在这里或 SetVisualState 里写 Scale / Rotation / CenterPoint /
        // Translation 这些依赖属性。WinUI 规定同一个元素的三维变换只能由一方驱动：
        // 一旦 XAML 侧用了它们，ElementCompositionPreview.GetElementVisual 会直接抛
        // UnauthorizedAccessException（"currently has the CenterPoint property in use"）。
        // 本控件的三维属性全部交给下面的 Visual 用 Composition 写。

        AutomationProperties.SetAutomationId(this, $"CharacterCard_{vm.Id}");
        AutomationProperties.SetName(this, vm.Nickname);
    }

    public CharacterCardViewModel ViewModel { get; }

    /// <summary>这张卡片在未缩放时的高度（内容自然高度），页面用它算适配比例。</summary>
    public double CardHeight { get; }

    /// <summary>阴影板的 visual：页面用它做浓度动画（只动 Opacity，不碰三维变换）。</summary>
    internal Visual ShadowVisual =>
        _shadowVisual ??= ElementCompositionPreview.GetElementVisual(_shadowPlate);

    /// <summary>
    /// 这张卡片的合成 visual。**唯一**的三维变换入口：Scale / Rotation /
    /// RotationAxis / CenterPoint(含 Z) / Opacity 都写在它上面，页面动画也用它。
    /// 只取一次并缓存——GetElementVisual 对同一元素返回同一个对象。
    /// </summary>
    internal Visual Visual => _visual ??= ElementCompositionPreview.GetElementVisual(this);


    /// <summary>
    /// 静态提交一个视觉状态。动画运行时页面用 Composition 直接驱动同一组属性，
    /// 动画结束后再调回这里——写进去的值和动画最后一帧完全相同，因此没有跳变。
    /// </summary>
    /// <param name="state">目标视觉状态。</param>
    /// <param name="fitScale">高度适配系数（窗口矮的时候整体缩小，1.0 表示不缩）。</param>
    public void SetVisualState(CardVisualState state, double fitScale)
    {
        var scale = (float)(state.Scale * fitScale);
        var visual = Visual;

        visual.RotationAxis = Vector3.UnitY;
        // CenterPoint 既是旋转/缩放的支点，又是唯一的 3D 位移通道：
        // 支点整体平移 d，卡片就平移 d，而旋转仍绕卡片自身中心。
        visual.CenterPoint = new Vector3((float)(Width / 2.0), (float)(CardHeight / 2.0), state.DepthZ);
        visual.Scale = new Vector3(scale, scale, 1f);
        visual.RotationAngleInDegrees = state.RotationYDegrees;
        visual.Opacity = state.Opacity;
        _shadowPlate.Opacity = state.ShadowStrength;
    }


    private UIElement BuildContent(CharacterCardViewModel vm)
    {
        var root = new StackPanel { Spacing = 0 };

        // ---- identity row ----
        var identity = new Grid();
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var avatarTile = new Border
        {
            Width = 56,
            Height = 56,
            CornerRadius = new CornerRadius(28),
            Background = AvatarTileBackground,
        };

        avatarTile.Child = BuildInitial(vm.AvatarInitial, 22);
        if (!string.IsNullOrEmpty(vm.AvatarPngBase64)) LoadAvatar(avatarTile, vm);

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
            FontSize = 17,
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
        root.Children.Add(MakeBar(vm.HpText, true, 24));
        root.Children.Add(SectionLabel("魔法", 14));
        root.Children.Add(MakeBar(vm.MpText, false, 24));

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
        };
        AutomationProperties.SetAutomationId(deleteButton, $"Delete_{vm.Id}");
        AutomationProperties.SetName(deleteButton, $"删除 {vm.Nickname}");
        deleteButton.Click += (_, _) => DeleteRequested?.Invoke(this, vm);

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

        return root;
    }

    /// <summary>
    /// Feed a base64 PNG into a <see cref="BitmapImage"/> off the UI thread.
    ///
    /// BitmapImage exposes no synchronous byte loader, and blocking on
    /// SetSourceAsync inside the UI thread deadlocks. Decoding into a small
    /// in-memory stream and awaiting SetSourceAsync lets ImageOpened fire.
    ///
    /// 每张卡片只解码一次（<see cref="_avatarStarted"/>）：轮播期间卡片被复用，
    /// 这里不会再被调用。
    /// </summary>
    private void LoadAvatar(Border tile, CharacterCardViewModel vm)
    {
        if (_avatarStarted) return;
        _avatarStarted = true;

        try
        {
            var image = new Image { Stretch = Stretch.UniformToFill, Opacity = 0 };
            var bitmap = new BitmapImage();
            bitmap.ImageOpened += (_, _) =>
            {
                image.Source = bitmap;
                image.Opacity = 1;
                tile.Child = image;
            };
            bitmap.ImageFailed += (_, e) => App.Log($"card avatar decode failed: {e.ErrorMessage}");
            _ = LoadAvatarAsync(bitmap, vm.AvatarPngBase64!);
        }
        catch (Exception ex)
        {
            App.Log($"card avatar failed: {ex.Message}");
        }
    }

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
        new GradientProgressBar
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
}
