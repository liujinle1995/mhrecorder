using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using MhRecorder.App.Services;
using MhRecorder.Core.Models;
using MhRecorder.Core.ViewModels;
using Windows.UI;

namespace MhRecorder.App.Views;

/// <summary>
/// Page 3 — 资产统计: three summary cards plus the two grouped bar charts.
///
/// 数字与环比来自 <see cref="AssetsViewModel"/>；这里只算"柱子画在哪"——那部分
/// 是纯像素计算，换成别的方式呈现（换成折线、改成表）时只该动这个文件。
/// </summary>
public sealed partial class AssetsPage : Page
{
    private static readonly SolidColorBrush GoldBrush = new(Color.FromArgb(255, 0xF6, 0xB8, 0x2B));
    private static readonly SolidColorBrush SilverBrush = new(Color.FromArgb(255, 0x8A, 0x93, 0xA5));
    private static readonly SolidColorBrush TextSecondary = new(Color.FromArgb(255, 0x6B, 0x6F, 0x76));
    private static readonly SolidColorBrush TextTertiary = new(Color.FromArgb(255, 0x9A, 0xA0, 0xA6));
    private static readonly SolidColorBrush BrandBlue = new(Color.FromArgb(255, 0x00, 0x74, 0xFF));

    public AssetsViewModel ViewModel { get; } = AppHost.Assets;

    private StatisticsPayload Payload => ViewModel.Payload;

    public AssetsPage()
    {
        InitializeComponent();
        Loaded += OnLoaded;
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            await ViewModel.LoadAsync(5);
            AppHost.Shell.SetFooterSnapshot(
                $"最近资产快照：{ViewModel.CharacterCountText} 个角色 · 金币 {ViewModel.GoldTotalText}");
            RedrawAll();
        }
        catch (Exception ex)
        {
            App.Log($"AssetsPage.OnLoaded failed: {ex}");
        }
    }

    private void RedrawAll()
    {
        DrawCharacterChart();
        DrawRecentChart();
    }

    private void OnCharacterChartSizeChanged(object sender, SizeChangedEventArgs e) => DrawCharacterChart();
    private void OnRecentChartSizeChanged(object sender, SizeChangedEventArgs e) => DrawRecentChart();

    // ---------------- chart 1: per-character gold vs silver ----------------

    private void DrawCharacterChart()
    {
        CharacterChartCanvas.Children.Clear();
        if (Payload.Characters.Count == 0) return;

        var width = CharacterChartCanvas.ActualWidth;
        var height = CharacterChartCanvas.ActualHeight;
        if (width <= 0 || height <= 0) return;

        var labelAreaHeight = 46;   // avatar + nickname below the bars
        var valueAreaHeight = 22;   // numbers above the bars
        var plotHeight = Math.Max(40, height - labelAreaHeight - valueAreaHeight);
        var maxValue = Payload.Characters.Max(c => Math.Max(c.GoldWan, c.SilverWan));
        if (maxValue <= 0) maxValue = 1;

        var groups = Payload.Characters.Count;
        var slotWidth = width / groups;

        for (var i = 0; i < groups; i++)
        {
            var item = Payload.Characters[i];
            var slotLeft = i * slotWidth;
            var center = slotLeft + slotWidth / 2;

            var goldRatio = item.GoldWan / maxValue;
            var silverRatio = item.SilverWan / maxValue;
            var barWidth = Math.Min(16, slotWidth * 0.13);
            var pairGap = 3;
            var pairWidth = barWidth * 2 + pairGap;

            var goldHeight = Math.Max(4, plotHeight * goldRatio);
            var silverHeight = Math.Max(4, plotHeight * silverRatio);

            var goldBar = new Rectangle
            {
                Width = barWidth,
                Height = goldHeight,
                Fill = GoldBrush,
                RadiusX = 3,
                RadiusY = 3,
            };
            Canvas.SetLeft(goldBar, center - pairWidth / 2);
            Canvas.SetTop(goldBar, valueAreaHeight + (plotHeight - goldHeight));
            CharacterChartCanvas.Children.Add(goldBar);

            var silverBar = new Rectangle
            {
                Width = barWidth,
                Height = silverHeight,
                Fill = SilverBrush,
                RadiusX = 3,
                RadiusY = 3,
            };
            Canvas.SetLeft(silverBar, center - pairWidth / 2 + barWidth + pairGap);
            Canvas.SetTop(silverBar, valueAreaHeight + (plotHeight - silverHeight));
            CharacterChartCanvas.Children.Add(silverBar);

            // Value labels above each bar
            AddLabel(CharacterChartCanvas, $"{item.GoldWan:0}",
                center - pairWidth / 2 - barWidth / 2 - 6,
                valueAreaHeight + (plotHeight - goldHeight) - 19, barWidth + 22, 16, TextSecondary, 11, TextAlignment.Center);

            AddLabel(CharacterChartCanvas, $"{item.SilverWan:0}",
                center - pairWidth / 2 + barWidth + pairGap - 6,
                valueAreaHeight + (plotHeight - silverHeight) - 19, barWidth + 22, 16, TextSecondary, 11, TextAlignment.Center);

            // Avatar dot + nickname under the bars
            var avatar = new Border
            {
                Width = 15,
                Height = 15,
                CornerRadius = new CornerRadius(7.5),
                Background = new SolidColorBrush(Color.FromArgb(255, 0xE9, 0xED, 0xF6)),
                Child = new TextBlock
                {
                    Text = Fmt.AvatarInitial(item.Nickname),
                    FontSize = 9,
                    Foreground = new SolidColorBrush(Color.FromArgb(255, 0x7A, 0x86, 0xA8)),
                    HorizontalAlignment = HorizontalAlignment.Center,
                    VerticalAlignment = VerticalAlignment.Center,
                },
            };
            Canvas.SetLeft(avatar, center - 34);
            Canvas.SetTop(avatar, valueAreaHeight + plotHeight + 10);
            CharacterChartCanvas.Children.Add(avatar);

            AddLabel(CharacterChartCanvas, item.Nickname,
                center - 18, valueAreaHeight + plotHeight + 9, 74, 17, TextSecondary, 11.5, TextAlignment.Left);
        }
    }

    // ---------------- chart 2: recent total assets ----------------

    private void DrawRecentChart()
    {
        RecentChartCanvas.Children.Clear();
        var recent = Payload.Recent;
        if (recent.Count == 0) return;

        var width = RecentChartCanvas.ActualWidth;
        var height = RecentChartCanvas.ActualHeight;
        if (width <= 0 || height <= 0) return;

        var labelAreaHeight = 30;
        var valueAreaHeight = 24;
        var plotHeight = Math.Max(40, height - labelAreaHeight - valueAreaHeight);
        // 未知（null）不是 0：当天没有任何一张截图带金币时，这里不画柱子也不
        // 报数字，避免"金币归零"的假信号。
        var maxValue = recent.Max(p => Math.Max(p.Gold ?? 0, p.Silver ?? 0));
        if (maxValue <= 0) maxValue = 1;

        var slotWidth = width / recent.Count;

        for (var i = 0; i < recent.Count; i++)
        {
            var point = recent[i];
            var center = i * slotWidth + slotWidth / 2;
            var isCurrent = i == recent.Count - 1;

            var barWidth = Math.Min(17, slotWidth * 0.16);
            var pairGap = 4;
            var pairWidth = barWidth * 2 + pairGap;
            var valueColor = isCurrent ? BrandBlue : TextSecondary;

            DrawBar(point.Gold, GoldBrush, center - pairWidth / 2, barWidth);
            DrawBar(point.Silver, SilverBrush, center - pairWidth / 2 + barWidth + pairGap, barWidth);

            AddLabel(RecentChartCanvas, point.Gold is { } g ? Fmt.Number(g) : "—",
                center - pairWidth / 2 - 10,
                valueAreaHeight + (plotHeight - BarHeight(point.Gold)) - 20,
                barWidth + 32, 17, valueColor, 11.5, TextAlignment.Center);

            AddLabel(RecentChartCanvas, point.Silver is { } sv ? Fmt.Number(sv) : "—",
                center - pairWidth / 2 + barWidth + pairGap - 10,
                valueAreaHeight + (plotHeight - BarHeight(point.Silver)) - 20,
                barWidth + 32, 17, valueColor, 11.5, TextAlignment.Center);

            AddLabel(RecentChartCanvas, point.Date,
                center - 30, valueAreaHeight + plotHeight + 9, 60, 17,
                isCurrent ? BrandBlue : TextTertiary, 11.5, TextAlignment.Center);
        }

        double BarHeight(long? value) =>
            value is { } v ? Math.Max(4, plotHeight * ((double)v / maxValue)) : 0;

        void DrawBar(long? value, Brush fill, double left, double barWidth)
        {
            if (value is null) return;   // 未知字段不画柱
            var h = BarHeight(value);
            var bar = new Rectangle { Width = barWidth, Height = h, Fill = fill, RadiusX = 3, RadiusY = 3 };
            Canvas.SetLeft(bar, left);
            Canvas.SetTop(bar, valueAreaHeight + (plotHeight - h));
            RecentChartCanvas.Children.Add(bar);
        }
    }

    private static void AddLabel(Canvas host, string text, double left, double top,
        double width, double height, Brush foreground, double fontSize, TextAlignment alignment)
    {
        var block = new TextBlock
        {
            Text = text,
            FontSize = fontSize,
            Foreground = foreground,
            Width = width,
            Height = height,
            TextAlignment = alignment,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Canvas.SetLeft(block, left);
        Canvas.SetTop(block, top);
        host.Children.Add(block);
    }
}
