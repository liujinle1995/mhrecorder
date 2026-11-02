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
