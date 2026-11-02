using Microsoft.UI;
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
/// 这张卡片原来是把 200 行"new TextBlock / new Border"堆在页面的 BuildCard 里，
/// 和轮播动画、搜索、删除混在同一个文件。卡片本身只是"把 CharacterCardViewModel
/// 画出来"——它不认识轮播，也不认识后端，所以单独一个控件。
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

    /// <summary>Raised when the user clicks 删除 on this card.</summary>
    public event EventHandler<CharacterCardViewModel>? DeleteRequested;

    public CharacterCardView(CharacterCardViewModel vm, bool isCenter, double width)
    {
        Width = width;

        // WinUI's Border is sealed, so the card is a UserControl wrapping one.
        // Everything visual (corner radius, hairline, shadow) stays on the Border.
        var frame = new Border
        {
            Background = Theme.Resource("CardBackgroundBrush", new SolidColorBrush(Colors.White)),
            BorderBrush = Theme.Resource("CardBorderBrush", new SolidColorBrush(Color.FromArgb(255, 0xE5, 0xE6, 0xEB))),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(14),
            // Generous vertical rhythm: the mock's card is ~424px tall at 316px wide.
            Padding = new Thickness(18, isCenter ? 18 : 16, 18, isCenter ? 16 : 14),
            Child = BuildContent(vm, isCenter, width),
        };

        if (isCenter)
        {
            try
            {
                frame.Translation = new System.Numerics.Vector3(0, 0, 32);
                frame.Shadow = new ThemeShadow();
            }
            catch (Exception ex)
            {
                App.Log($"ThemeShadow unavailable: {ex.Message}");
            }
        }

        Content = frame;
    }

    private UIElement BuildContent(CharacterCardViewModel vm, bool isCenter, double width)
    {
        var root = new StackPanel { Spacing = 0 };

        // ---- identity row ----
        var identity = new Grid();
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var avatarTile = new Border
        {
            Width = isCenter ? 60 : 46,
            Height = isCenter ? 60 : 46,
            CornerRadius = new CornerRadius(isCenter ? 30 : 23),
            Background = AvatarTileBackground,
        };

        avatarTile.Child = BuildInitial(vm.AvatarInitial, isCenter ? 24 : 18);
        if (!string.IsNullOrEmpty(vm.AvatarPngBase64)) LoadAvatar(avatarTile, vm, isCenter);

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
            FontSize = isCenter ? 17 : 16,
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
        root.Children.Add(MakeBar(vm.HpText, true, isCenter ? 24 : 20));
        root.Children.Add(SectionLabel("魔法", 14));
        root.Children.Add(MakeBar(vm.MpText, false, isCenter ? 24 : 20));

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

        _ = width;
        return root;
    }

    /// <summary>
    /// Feed a base64 PNG into a <see cref="BitmapImage"/> off the UI thread.
    ///
    /// BitmapImage exposes no synchronous byte loader, and blocking on
    /// SetSourceAsync inside the UI thread deadlocks. Decoding into a small
    /// in-memory stream and awaiting SetSourceAsync lets ImageOpened fire.
    /// </summary>
    private static void LoadAvatar(Border tile, CharacterCardViewModel vm, bool isCenter)
    {
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
            _ = LoadAvatarAsync(bitmap, vm.AvatarPngBase64!, isCenter ? 24 : 18);
        }
        catch (Exception ex)
        {
            App.Log($"card avatar failed: {ex.Message}");
        }
    }

    private static async Task LoadAvatarAsync(BitmapImage target, string base64Png, double fontSize)
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
            App.Log($"LoadAvatarAsync failed: {ex.Message} (font {fontSize})");
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
