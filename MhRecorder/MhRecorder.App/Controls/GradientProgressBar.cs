using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.UI;

namespace MhRecorder.App.Controls;

/// <summary>
/// Segmented progress bar used for the 气血 / 魔法 rows: rounded track plus a
/// gradient-filled fill and a centred value label. XAML can't express a
/// gradient-on-a-rounded-clip for a dynamic value, so this builds it in code.
/// </summary>
public sealed class GradientProgressBar : UserControl
{
    public static readonly DependencyProperty ValueProperty = DependencyProperty.Register(
        nameof(Value), typeof(double), typeof(GradientProgressBar),
        new PropertyMetadata(1.0, OnVisualChanged));

    public static readonly DependencyProperty RatioProperty = DependencyProperty.Register(
        nameof(Ratio), typeof(double), typeof(GradientProgressBar),
        new PropertyMetadata(1.0, OnVisualChanged));

    public static readonly DependencyProperty LabelProperty = DependencyProperty.Register(
        nameof(Label), typeof(string), typeof(GradientProgressBar),
        new PropertyMetadata(string.Empty, OnVisualChanged));

    public static readonly DependencyProperty BarHeightProperty = DependencyProperty.Register(
        nameof(BarHeight), typeof(double), typeof(GradientProgressBar),
        new PropertyMetadata(18.0, OnVisualChanged));

    public static readonly DependencyProperty UseHpGradientProperty = DependencyProperty.Register(
        nameof(UseHpGradient), typeof(bool), typeof(GradientProgressBar),
        new PropertyMetadata(true, OnVisualChanged));

    private readonly Grid _root = new();
    private readonly Border _track = new();
    private readonly Border _fill = new();
    private readonly TextBlock _label = new();

    public GradientProgressBar()
    {
        IsTabStop = false;

        _track.CornerRadius = new CornerRadius(9);
        _fill.CornerRadius = new CornerRadius(9);
        _fill.HorizontalAlignment = HorizontalAlignment.Left;

        _label.HorizontalAlignment = HorizontalAlignment.Center;
        _label.VerticalAlignment = VerticalAlignment.Center;
        _label.Foreground = new SolidColorBrush(Colors.White);
        _label.FontSize = 11.5;
        _label.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        _label.FontFamily = Theme.Resource("UiFontFamily", new FontFamily("Segoe UI Variable Text"));
        _label.IsHitTestVisible = false;

        _root.Children.Add(_track);
        _root.Children.Add(_fill);
        _root.Children.Add(_label);

        Content = _root;
        Loaded += (_, _) => Rebuild();
        SizeChanged += (_, _) => Rebuild();
    }

    public double Value
    {
        get => (double)GetValue(ValueProperty);
        set => SetValue(ValueProperty, value);
    }

    /// <summary>0..1 share of the track that should be filled.</summary>
    public double Ratio
    {
        get => (double)GetValue(RatioProperty);
        set => SetValue(RatioProperty, value);
    }

    public string Label
    {
        get => (string)GetValue(LabelProperty);
        set => SetValue(LabelProperty, value);
    }

    public double BarHeight
    {
        get => (double)GetValue(BarHeightProperty);
        set => SetValue(BarHeightProperty, value);
    }

    public bool UseHpGradient
    {
        get => (bool)GetValue(UseHpGradientProperty);
        set => SetValue(UseHpGradientProperty, value);
    }

    private static void OnVisualChanged(DependencyObject d, DependencyPropertyChangedEventArgs e) =>
        ((GradientProgressBar)d).Rebuild();

    private void Rebuild()
    {
        var height = Math.Max(6, BarHeight);
        var ratio = Math.Clamp(Ratio, 0, 1);

        _root.Height = height;
        _track.Height = height;
        _fill.Height = height;

        _track.Background = new SolidColorBrush(UseHpGradient
            ? Color.FromArgb(0x1F, 0xE8, 0x44, 0x3A)
            : Color.FromArgb(0x1F, 0x1E, 0x86, 0xFB));
        _track.Width = double.NaN;
        _track.HorizontalAlignment = HorizontalAlignment.Stretch;

        var gradient = new LinearGradientBrush
        {
            StartPoint = new Windows.Foundation.Point(0, 0.5),
            EndPoint = new Windows.Foundation.Point(1, 0.5),
        };
        if (UseHpGradient)
        {
            gradient.GradientStops.Add(new GradientStop { Color = Color.FromArgb(255, 0xE8, 0x44, 0x3A), Offset = 0 });
            gradient.GradientStops.Add(new GradientStop { Color = Color.FromArgb(255, 0xFF, 0x8A, 0x4C), Offset = 1 });
        }
        else
        {
            gradient.GradientStops.Add(new GradientStop { Color = Color.FromArgb(255, 0x1E, 0x86, 0xFB), Offset = 0 });
            gradient.GradientStops.Add(new GradientStop { Color = Color.FromArgb(255, 0x63, 0xD2, 0xF0), Offset = 1 });
        }
        _fill.Background = gradient;

        var available = ActualWidth > 0 ? ActualWidth : (Parent as FrameworkElement)?.ActualWidth ?? 0;
        if (available > 0)
            _fill.Width = Math.Max(height, available * ratio);

        _label.Text = Label ?? string.Empty;
        _label.FontSize = Math.Max(10.5, height * 0.64);
    }
}
