using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using MhRecorder.Core.Models;
using Windows.Foundation;
using Windows.Storage.Streams;
using Windows.UI;

namespace MhRecorder.App.Controls;

/// <summary>
/// 可缩放 / 平移的截图查看器，并在图上画出 OCR 区域框。
///
/// 这是审核工作区里"看得见错在哪"的那一半。关键决定：图片按 **1 像素 = 1 DIP**
/// 铺开，叠加层画在同一坐标系里，于是后端返回的像素坐标可以直接当布局坐标用，
/// 不需要任何换算——换算就是错位的来源。缩放/平移作用在外层容器的 RenderTransform
/// 上，叠加层跟着一起走。
/// </summary>
public sealed class ZoomableImageView : Grid
{
    private const double MinZoom = 0.15;
    private const double MaxZoom = 8.0;

    private static readonly Color FieldColor = Color.FromArgb(255, 0, 116, 255);
    private static readonly Color IdentityColor = Color.FromArgb(255, 122, 92, 255);
    private static readonly Color ItemColor = Color.FromArgb(255, 226, 152, 22);
    private static readonly Color FeatureColor = Color.FromArgb(255, 31, 180, 97);

    private readonly Image _image = new() { Stretch = Stretch.Fill };
    private readonly Canvas _overlay = new();

    // A Canvas, not a Grid: children position from (0,0) with no alignment
    // games, so image pixels and overlay pixels share one coordinate space.
    private readonly Canvas _viewport = new();
    private readonly ScaleTransform _scale = new() { ScaleX = 1, ScaleY = 1 };
    private readonly TranslateTransform _translate = new();
    private readonly TextBlock _placeholder = new();

    private readonly Dictionary<string, Border> _boxes = new(StringComparer.Ordinal);
    private readonly List<Border> _chips = new();

    private double _pixelWidth;
    private double _pixelHeight;

    private bool _dragging;
    private Point _dragStart;
    private Point _translateStart;

    /// <summary>True once the user zoomed/panned; a window resize then leaves their view alone.</summary>
    private bool _userAdjusted;

    /// <summary>Set when the image arrived before the control had a size.</summary>
    private bool _needsFit;

    /// <summary>Raised when the user clicks a box; the page selects the matching row.</summary>
    public event EventHandler<FieldRegion>? RegionClicked;

    public ZoomableImageView()
    {
        Background = new SolidColorBrush(Color.FromArgb(255, 247, 248, 250));
        Clip = new RectangleGeometry();

        var group = new TransformGroup();
        // Scale first, then translate: the translation is expressed in screen
        // pixels, which is what the pointer maths below assumes.
        group.Children.Add(_scale);
        group.Children.Add(_translate);

        _viewport.RenderTransform = group;
        _viewport.RenderTransformOrigin = new Point(0, 0);
        _viewport.Background = new SolidColorBrush(Colors.Transparent);
        _viewport.HorizontalAlignment = HorizontalAlignment.Left;
        _viewport.VerticalAlignment = VerticalAlignment.Top;
        _viewport.Children.Add(_image);
        _viewport.Children.Add(_overlay);
        Children.Add(_viewport);

        // The page draws its own drop hint over this control, so the viewer only
        // shows text when a capture failed to decode.
        _placeholder.Text = "这张截图解码失败，换一张试试";
        _placeholder.FontSize = 12.5;
        _placeholder.HorizontalAlignment = HorizontalAlignment.Center;
        _placeholder.VerticalAlignment = VerticalAlignment.Center;
        _placeholder.Foreground = new SolidColorBrush(Color.FromArgb(255, 181, 181, 185));
        _placeholder.Visibility = Visibility.Collapsed;
        Children.Add(_placeholder);

        SizeChanged += OnSizeChanged;
        PointerPressed += OnPointerPressed;
        PointerMoved += OnPointerMoved;
        PointerReleased += OnPointerReleased;
        PointerCaptureLost += (_, _) => _dragging = false;
        PointerWheelChanged += OnWheelChanged;
        DoubleTapped += (_, _) => FitToView();
        AutomationProperties.SetAutomationId(this, "RecognitionImageViewer");
    }

    /// <summary>Current zoom factor (1.0 = image pixels shown 1:1).</summary>
    public double Zoom => _scale.ScaleX;

    public bool HasImage => _image.Source is not null;

    /// <summary>Load a capture. Null clears the viewer back to its placeholder.</summary>
    public async Task SetImageAsync(byte[]? bytes)
    {
        if (bytes is null || bytes.Length == 0)
        {
            _image.Source = null;
            _overlay.Children.Clear();
            _boxes.Clear();
            _viewport.Visibility = Visibility.Collapsed;
            _placeholder.Visibility = Visibility.Collapsed;
            return;
        }

        try
        {
            var bitmap = new BitmapImage();
            using (var stream = new InMemoryRandomAccessStream())
            {
                var writer = new DataWriter(stream.GetOutputStreamAt(0));
                writer.WriteBytes(bytes);
                await writer.StoreAsync();
                await writer.FlushAsync();
                writer.DetachStream();
                await bitmap.SetSourceAsync(stream);
            }

            _pixelWidth = bitmap.PixelWidth;
            _pixelHeight = bitmap.PixelHeight;

            // 1 pixel = 1 DIP: keeps overlay coordinates identical to OCR pixels.
            _image.Width = _pixelWidth;
            _image.Height = _pixelHeight;
            _overlay.Width = _pixelWidth;
            _overlay.Height = _pixelHeight;
            _viewport.Width = _pixelWidth;
            _viewport.Height = _pixelHeight;
            Canvas.SetLeft(_image, 0);
            Canvas.SetTop(_image, 0);
            Canvas.SetLeft(_overlay, 0);
            Canvas.SetTop(_overlay, 0);

            _image.Source = bitmap;
            _viewport.Visibility = Visibility.Visible;
            _placeholder.Visibility = Visibility.Collapsed;

            _userAdjusted = false;
            FitToView();
        }
        catch (Exception ex)
        {
            App.Log($"ZoomableImageView.SetImage failed: {ex.Message}");
            _placeholder.Visibility = Visibility.Visible;
        }
    }

    /// <summary>Replace the overlay with the boxes belonging to the current capture.</summary>
    public void SetRegions(IEnumerable<FieldRegion> regions, string? highlightKey = null)
    {
        _overlay.Children.Clear();
        _boxes.Clear();
        _chips.Clear();

        foreach (var region in regions)
        {
            if (region.Width <= 0 || region.Height <= 0) continue;

            var color = ColorFor(region);
            var box = new Border
            {
                Width = region.Width,
                Height = region.Height,
                BorderThickness = new Thickness(1.5),
                CornerRadius = new CornerRadius(2),
                BorderBrush = new SolidColorBrush(color),
                Background = new SolidColorBrush(Color.FromArgb(28, color.R, color.G, color.B)),
                Tag = region,
                IsHitTestVisible = true,
            };
            AutomationProperties.SetName(box, $"{region.Label} {region.RawText}");
            box.Tapped += (_, e) =>
            {
                e.Handled = true;
                RegionClicked?.Invoke(this, region);
            };

            Canvas.SetLeft(box, region.X);
            Canvas.SetTop(box, region.Y);
            _overlay.Children.Add(box);

            // Label chip above the box, flipped below when the box hugs the top
            // edge so it never gets clipped out of view.
            var chip = new Border
            {
                Background = new SolidColorBrush(color),
                CornerRadius = new CornerRadius(3),
                Padding = new Thickness(4, 1, 4, 2),
                IsHitTestVisible = false,
                Child = new TextBlock
                {
                    Text = region.Label,
                    FontSize = 10,
                    Foreground = new SolidColorBrush(Colors.White),
                },
            };
            var chipTop = region.Y - 15 >= 0 ? region.Y - 15 : region.Y + region.Height + 1;
            Canvas.SetLeft(chip, region.X);
            Canvas.SetTop(chip, chipTop);
            _overlay.Children.Add(chip);
            _chips.Add(chip);

            _boxes[region.Key] = box;
        }

        UpdateChipVisibility();
        Highlight(highlightKey, focus: false);
    }

    /// <summary>
    /// Labels are only readable once the capture is zoomed in; at fit-to-window
    /// scale a 10px chip over a 25%-scaled panel is noise, so only the boxes show.
    /// </summary>
    private void UpdateChipVisibility()
    {
        var visible = _scale.ScaleX >= 0.55;
        foreach (var chip in _chips)
            chip.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Emphasise one box (and, when asked, bring it into view).</summary>
    public void Highlight(string? key, bool focus = true)
    {
        foreach (var (k, box) in _boxes)
        {
            var active = key is not null && k == key;
            box.BorderThickness = new Thickness(active ? 3 : 1.5);
            if (box.Tag is FieldRegion region)
            {
                var color = ColorFor(region);
                box.BorderBrush = new SolidColorBrush(color);
                box.Background = new SolidColorBrush(
                    Color.FromArgb(active ? (byte)64 : (byte)28, color.R, color.G, color.B));
            }
        }

        if (!focus || key is null) return;
        if (!_boxes.TryGetValue(key, out var targetBox)) return;
        if (targetBox.Tag is not FieldRegion target) return;

        FocusRegion(target);
    }

    /// <summary>
    /// Bring one field into view at a size where the text is legible.
    ///
    /// Fit-to-window on a 1265x711 capture inside a 350px pane is a 27% view —
    /// good for "where on the panel is this", useless for "which digit is
    /// wrong". Selecting a field therefore zooms to roughly half the viewport.
    /// </summary>
    private void FocusRegion(FieldRegion region)
    {
        if (region.Width <= 0 || region.Height <= 0) return;

        var targetScale = Math.Min(
            ActualWidth * 0.55 / region.Width,
            ActualHeight * 0.45 / region.Height);
        targetScale = Math.Clamp(targetScale, 0.8, 3.0);

        _scale.ScaleX = _scale.ScaleY = targetScale;
        _translate.X = ActualWidth / 2 - (region.X + region.Width / 2.0) * targetScale;
        _translate.Y = ActualHeight / 2 - (region.Y + region.Height / 2.0) * targetScale;
        _userAdjusted = true;
        UpdateChipVisibility();
    }

    /// <summary>Fit the whole capture into the viewport and centre it.</summary>
    public void FitToView()
    {
        if (_pixelWidth <= 0 || _pixelHeight <= 0) return;
        if (ActualWidth <= 0 || ActualHeight <= 0)
        {
            // Not laid out yet; OnSizeChanged fits once it is.
            _needsFit = true;
            return;
        }

        var scale = Math.Min(ActualWidth / _pixelWidth, ActualHeight / _pixelHeight);
        if (double.IsNaN(scale) || double.IsInfinity(scale) || scale <= 0) scale = 1;

        _scale.ScaleX = _scale.ScaleY = Math.Clamp(scale, MinZoom, 1.0);
        _translate.X = (ActualWidth - _pixelWidth * _scale.ScaleX) / 2;
        _translate.Y = (ActualHeight - _pixelHeight * _scale.ScaleY) / 2;
        _needsFit = false;
        UpdateChipVisibility();
    }

    /// <summary>Zoom around the viewport centre — what the +/- buttons mean.</summary>
    public void ZoomBy(double factor)
    {
        ZoomAt(new Point(ActualWidth / 2, ActualHeight / 2), factor);
    }

    public void ResetZoom()
    {
        _userAdjusted = false;
        FitToView();
    }

    private void ZoomAt(Point anchor, double factor)
    {
        if (_pixelWidth <= 0) return;

        var oldScale = _scale.ScaleX;
        var newScale = Math.Clamp(oldScale * factor, MinZoom, MaxZoom);
        if (Math.Abs(newScale - oldScale) < 1e-6) return;

        // Keep the content point under `anchor` pinned while the scale changes.
        var contentX = (anchor.X - _translate.X) / oldScale;
        var contentY = (anchor.Y - _translate.Y) / oldScale;

        _scale.ScaleX = _scale.ScaleY = newScale;
        _translate.X = anchor.X - contentX * newScale;
        _translate.Y = anchor.Y - contentY * newScale;
        _userAdjusted = true;
        UpdateChipVisibility();
    }

    private void EnsureVisible(Rect target)
    {
        var left = target.X * _scale.ScaleX + _translate.X;
        var top = target.Y * _scale.ScaleY + _translate.Y;
        var right = left + target.Width * _scale.ScaleX;
        var bottom = top + target.Height * _scale.ScaleY;

        const double pad = 60;
        double dx = 0, dy = 0;
        if (left < pad) dx = pad - left;
        else if (right > ActualWidth - pad) dx = ActualWidth - pad - right;
        if (top < pad) dy = pad - top;
        else if (bottom > ActualHeight - pad) dy = ActualHeight - pad - bottom;

        _translate.X += dx;
        _translate.Y += dy;
    }

    private static Color ColorFor(FieldRegion region) => region.Kind switch
    {
        FieldRegionKind.Identity => IdentityColor,
        FieldRegionKind.Item => ItemColor,
        FieldRegionKind.Feature => FeatureColor,
        _ => FieldColor,
    };

    private void OnSizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (Clip is RectangleGeometry clip)
            clip.Rect = new Rect(0, 0, e.NewSize.Width, e.NewSize.Height);

        // First layout after an image was set, or a plain window resize: keep the
        // whole capture in view. Once the user has zoomed in, respect that.
        if (_pixelWidth > 0 && (!_userAdjusted || _needsFit)) FitToView();
    }

    private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (_image.Source is null) return;
        var point = e.GetCurrentPoint(this);
        if (!point.Properties.IsLeftButtonPressed) return;

        _dragging = true;
        _dragStart = point.Position;
        _translateStart = new Point(_translate.X, _translate.Y);
        CapturePointer(e.Pointer);
        e.Handled = true;
    }

    private void OnPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging) return;
        var position = e.GetCurrentPoint(this).Position;
        _translate.X = _translateStart.X + (position.X - _dragStart.X);
        _translate.Y = _translateStart.Y + (position.Y - _dragStart.Y);
    }

    private void OnPointerReleased(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging) return;
        _dragging = false;
        ReleasePointerCapture(e.Pointer);
    }

    private void OnWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        if (_image.Source is null) return;
        var delta = e.GetCurrentPoint(this).Properties.MouseWheelDelta;
        if (delta == 0) return;

        var anchor = e.GetCurrentPoint(this).Position;
        ZoomAt(anchor, delta > 0 ? 1.12 : 1 / 1.12);
        e.Handled = true;
    }
}
