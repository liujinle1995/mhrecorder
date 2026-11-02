// 图片从哪里来：文件、剪贴板、另一个窗口的截图。
//
// 三种入口以前各自散在 ImportPage.xaml.cs 里，每个都自带一份 busy 判断、错误
// 提示和字节转换。抽出来以后页面只调一个命令，来源差异被关在这一个文件里。

namespace MhRecorder.Core.Services;

/// <summary>One image about to be recognised.</summary>
public sealed record ImageFile(string Name, byte[] Bytes, string Source)
{
    /// <summary>Base64 payload exactly as the pipe protocol expects it.</summary>
    public RecognizeImage ToWire() => new() { Name = Name, Data = Convert.ToBase64String(Bytes) };
}

/// <summary>Result of reading the clipboard: images plus a message when there were none.</summary>
public sealed record ClipboardRead(IReadOnlyList<ImageFile> Items, string Message);

/// <summary>An application window the user can screenshot.</summary>
public sealed record WindowTarget(nint Handle, string Title, string ProcessName, int Width, int Height)
{
    public string Detail => string.IsNullOrWhiteSpace(ProcessName)
        ? $"{Width}×{Height}"
        : $"{ProcessName} · {Width}×{Height}";

    /// <summary>The game is what this feature exists for, so preselect it.</summary>
    public bool LooksLikeGame =>
        Title.Contains("梦幻") || ProcessName.Contains("MyGame", StringComparison.OrdinalIgnoreCase);
}

/// <summary>Platform capabilities Core cannot implement (WinUI pickers, clipboard, screen capture).</summary>
public interface IImageSource
{
    /// <summary>Open-file picker. Returns an empty list when the user cancels.</summary>
    Task<IReadOnlyList<ImageFile>> PickFilesAsync(CancellationToken ct = default);

    Task<ClipboardRead> ReadClipboardAsync(CancellationToken ct = default);

    Task<IReadOnlyList<WindowTarget>> ListWindowsAsync(CancellationToken ct = default);

    /// <summary>Grab one window's client area as PNG. Null when the capture failed.</summary>
    Task<byte[]?> CaptureWindowAsync(WindowTarget target, CancellationToken ct = default);
}

/// <summary>Shared file-name and encoding rules for every import path.</summary>
public sealed class ImageImportService
{
    /// <summary>Extensions the recogniser accepts.</summary>
    public static readonly string[] SupportedExtensions = { ".png", ".jpg", ".jpeg", ".bmp" };

    private readonly IImageSource _source;

    public ImageImportService(IImageSource source) => _source = source;

    public Task<IReadOnlyList<ImageFile>> PickFilesAsync(CancellationToken ct = default) =>
        _source.PickFilesAsync(ct);

    public Task<ClipboardRead> ReadClipboardAsync(CancellationToken ct = default) =>
        _source.ReadClipboardAsync(ct);

    /// <summary>Window list, game first then by title — the order the picker shows.</summary>
    public async Task<IReadOnlyList<WindowTarget>> ListWindowsAsync(CancellationToken ct = default)
    {
        var windows = await _source.ListWindowsAsync(ct).ConfigureAwait(false);
        return windows
            .OrderByDescending(w => w.LooksLikeGame)
            .ThenBy(w => w.Title, StringComparer.CurrentCulture)
            .ToList();
    }

    /// <summary>Capture a window and wrap it as a one-image batch.</summary>
    public async Task<ImageFile?> CaptureWindowAsync(WindowTarget target, CancellationToken ct = default)
    {
        var png = await _source.CaptureWindowAsync(target, ct).ConfigureAwait(false);
        if (png is null || png.Length == 0) return null;
        var stem = string.IsNullOrWhiteSpace(target.ProcessName) ? "窗口截图" : target.ProcessName;
        return new ImageFile($"{stem}-{DateTime.Now:HHmmss}.png", png, target.Title);
    }

    /// <summary>Read files from disk, skipping (and logging) the ones that fail.</summary>
    public static async Task<IReadOnlyList<ImageFile>> ReadFilesAsync(
        IEnumerable<string> paths, CancellationToken ct = default)
    {
        var items = new List<ImageFile>();
        foreach (var path in paths)
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                var bytes = await File.ReadAllBytesAsync(path, ct).ConfigureAwait(false);
                items.Add(new ImageFile(Path.GetFileName(path), bytes, "文件"));
            }
            catch (Exception ex)
            {
                AppLog.Write($"read image failed ({path}): {ex.Message}");
            }
        }
        return items;
    }
}
