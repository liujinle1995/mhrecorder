using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace MhRecorder.App.Converters;

/// <summary>true → Visible, false → Collapsed. Set Invert for the opposite.</summary>
public sealed class BoolToVisibilityConverter : IValueConverter
{
    public bool Invert { get; set; }

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var flag = value is bool b && b;
        if (Invert) flag = !flag;
        return flag ? Visibility.Visible : Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}

/// <summary>Non-null/non-empty → Visible.</summary>
public sealed class NonEmptyToVisibilityConverter : IValueConverter
{
    public bool Invert { get; set; }

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var visible = value switch
        {
            null => false,
            string s => !string.IsNullOrWhiteSpace(s),
            int i => i > 0,
            long l => l > 0,
            double d => d > 0,
            System.Collections.IEnumerable e => HasAny(e),
            _ => true,
        };
        if (Invert) visible = !visible;
        return visible ? Visibility.Visible : Visibility.Collapsed;
    }

    private static bool HasAny(System.Collections.IEnumerable items)
    {
        var enumerator = items.GetEnumerator();
        try
        {
            return enumerator.MoveNext();
        }
        finally
        {
            if (enumerator is IDisposable disposable) disposable.Dispose();
        }
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}

/// <summary>
/// ViewModel 里的严重度是字符串（"Success" / "Warning" / "Error" / "Informational"），
/// 因为 Core 不能引用 WinUI 的 InfoBarSeverity。这里做最后一次翻译。
/// </summary>
public sealed class SeverityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        (value as string) switch
        {
            "Success" => Microsoft.UI.Xaml.Controls.InfoBarSeverity.Success,
            "Warning" => Microsoft.UI.Xaml.Controls.InfoBarSeverity.Warning,
            "Error" => Microsoft.UI.Xaml.Controls.InfoBarSeverity.Error,
            _ => Microsoft.UI.Xaml.Controls.InfoBarSeverity.Informational,
        };

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}

/// <summary>0..1 的比例 → 像素宽度（置信度细条的宽度）。</summary>
public sealed class RatioToWidthConverter : IValueConverter
{
    /// <summary>Full-scale width in DIPs.</summary>
    public double FullWidth { get; set; } = 56;

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var ratio = value is double d ? Math.Clamp(d, 0, 1) : 0;
        return Math.Max(2, ratio * FullWidth);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}

/// <summary>置信度 → 颜色：低于 85% 标黄，提醒"这一项值得核对"。</summary>
public sealed class ConfidenceToBrushConverter : IValueConverter
{
    private static readonly Microsoft.UI.Xaml.Media.SolidColorBrush Weak =
        new(Windows.UI.Color.FromArgb(255, 0xE0, 0xA1, 0x2B));

    private static readonly Microsoft.UI.Xaml.Media.SolidColorBrush Strong =
        new(Windows.UI.Color.FromArgb(255, 0x1F, 0xB4, 0x61));

    public double Threshold { get; set; } = 0.85;

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var confidence = value is double d ? d : 0;
        return confidence > 0 && confidence < Threshold ? Weak : Strong;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}
