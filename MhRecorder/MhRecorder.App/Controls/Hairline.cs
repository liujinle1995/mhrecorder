using Microsoft.UI.Xaml;
using Microsoft.UI;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.UI;

namespace MhRecorder.App.Controls;

/// <summary>
/// A 1px-thin rectangular border drawn with a solid fill. WinUI's stock Border
/// anti-aliases corners inconsistently at fractional DPI, so the shared cards
/// use this wrapper to keep the hairline crisp and uniformly 1px.
/// </summary>
public static class Hairline
{
    public static SolidColorBrush CardBorder => Theme.Resource("CardBorderBrush", new SolidColorBrush(Color.FromArgb(255, 229, 230, 235)));
    public static SolidColorBrush Card => Theme.Resource("CardBackgroundBrush", new SolidColorBrush(Colors.White));
}

/// <summary>Renders the avatar tile as a gradient disc with the character's initial.</summary>
public sealed class AvatarResources
{
    private static readonly Color[] Pairs =
    {
        Color.FromArgb(255, 0x8F, 0xA8, 0xE8),
        Color.FromArgb(255, 0xB6, 0x8F, 0xE8),
        Color.FromArgb(255, 0xE8, 0x9F, 0x8F),
        Color.FromArgb(255, 0x8F, 0xE8, 0xC4),
        Color.FromArgb(255, 0xE8, 0xC4, 0x8F),
    };

    public static Color ColorFor(string? key)
    {
        if (string.IsNullOrEmpty(key)) return Pairs[0];
        var hash = 0;
        foreach (var ch in key) hash = (hash * 31 + ch) & 0x7FFFFFFF;
        return Pairs[hash % Pairs.Length];
    }
}
