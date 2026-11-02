using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.UI.Xaml.Shapes;
using Windows.UI;

namespace MhRecorder.App.Controls;

/// <summary>Gold / silver coin glyph drawn as vector shapes (crisp at any DPI).</summary>
public sealed class CoinIcon : UserControl
{
    public static readonly DependencyProperty KindProperty = DependencyProperty.Register(
        nameof(Kind), typeof(CoinKind), typeof(CoinIcon),
        new PropertyMetadata(CoinKind.Gold, OnKindChanged));

    private readonly Grid _root = new();
    private readonly Ellipse _outer = new();
    private readonly Ellipse _inner = new();
    private readonly Border _square = new();

    public CoinIcon()
    {
        _outer.StrokeThickness = 1.6;
        _inner.StrokeThickness = 1.6;
        _square.Width = 5;
        _square.Height = 5;
        _square.CornerRadius = new CornerRadius(1);
        _square.HorizontalAlignment = HorizontalAlignment.Center;
        _square.VerticalAlignment = VerticalAlignment.Center;

        _root.Children.Add(_outer);
        _root.Children.Add(_inner);
        _root.Children.Add(_square);

        Content = _root;
        SizeChanged += (_, _) => Rebuild();
        Loaded += (_, _) => Rebuild();
    }

    public CoinKind Kind
    {
        get => (CoinKind)GetValue(KindProperty);
        set => SetValue(KindProperty, value);
    }

    private static void OnKindChanged(DependencyObject d, DependencyPropertyChangedEventArgs e) =>
        ((CoinIcon)d).Rebuild();

    private void Rebuild()
    {
        var size = Math.Min(ActualWidth, ActualHeight);
        if (size <= 0) size = 22;

        var gold = Kind == CoinKind.Gold;
        var main = gold ? Color.FromArgb(255, 0xF6, 0xB8, 0x2B) : Color.FromArgb(255, 0x8A, 0x93, 0xA5);
        var dark = gold ? Color.FromArgb(255, 0xD1, 0x94, 0x1C) : Color.FromArgb(255, 0x6E, 0x7A, 0x8C);

        _root.Width = size;
        _root.Height = size;

        _outer.Width = size;
        _outer.Height = size;
        _outer.Fill = new SolidColorBrush(dark);
        _outer.Stroke = new SolidColorBrush(dark);

        var innerSize = size * 0.74;
        _inner.Width = innerSize;
        _inner.Height = innerSize;
        _inner.Fill = new SolidColorBrush(main);
        _inner.Stroke = new SolidColorBrush(main);

        _square.Background = new SolidColorBrush(gold ? Colors.White : Color.FromArgb(255, 0xE7, 0xEE, 0xF4));
        _square.Width = Math.Max(4, size * 0.22);
        _square.Height = Math.Max(4, size * 0.22);
    }
}

public enum CoinKind
{
    Gold,
    Silver,
}

