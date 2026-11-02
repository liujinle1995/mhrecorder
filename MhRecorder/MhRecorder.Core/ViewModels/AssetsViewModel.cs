// 资产统计页的 ViewModel。
//
// 这一页的"难"不在业务，而在画图：两张柱状图的坐标计算有三百行。但取数、算
// 环比、把 null 当成"未知"而不是 0，这些是规则，属于 ViewModel；画柱子的像素
// 计算属于视图，留在页面里。

using CommunityToolkit.Mvvm.ComponentModel;
using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.Core.ViewModels;

public sealed partial class AssetsViewModel : ObservableObject
{
    private readonly BackendService _backend;

    public AssetsViewModel(BackendService backend) => _backend = backend;

    [ObservableProperty]
    private StatisticsPayload _payload = new();

    [ObservableProperty]
    private string _goldTotalText = "0";

    [ObservableProperty]
    private string _silverTotalText = "0";

    [ObservableProperty]
    private string _characterCountText = "0";

    [ObservableProperty]
    private string _goldChangeText = "—";

    [ObservableProperty]
    private string _silverChangeText = "—";

    [ObservableProperty]
    private string _characterChangeText = "—";

    [ObservableProperty]
    private bool _isEmpty = true;

    public async Task LoadAsync(int recentLimit = 5)
    {
        try
        {
            Payload = await _backend.Api.GetStatisticsAsync(recentLimit);
        }
        catch (Exception ex)
        {
            AppLog.Write($"AssetsViewModel.Load failed: {ex}");
            Payload = new StatisticsPayload();
        }

        var summary = Payload.Summary;
        GoldTotalText = Fmt.Number(summary.GoldTotal);
        SilverTotalText = Fmt.Number(summary.SilverTotal);
        CharacterCountText = Fmt.Number(summary.CharacterCount);

        GoldChangeText = summary.GoldChangePercent is { } g ? Fmt.Percent(g) : "—";
        SilverChangeText = summary.SilverChangePercent is { } s ? Fmt.Percent(s) : "—";
        CharacterChangeText = summary.CharacterChange is { } c && c != 0 ? $"{c:+0;-0} 个" : "—";

        IsEmpty = Payload.Characters.Count == 0 && Payload.Recent.Count == 0;
    }
}
