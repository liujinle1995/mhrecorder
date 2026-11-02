// Core 里定义平台能力接口，这里给出 WinUI 实现。
//
// 页面代码从此不需要知道 FileOpenPicker 需要 HWND、剪贴板要处理三种格式、
// 截图要先抢焦点再还回来——这些都属于"在这台 Windows 上怎么做到"，不属于
// "这次识别要做什么"。

using MhRecorder.App.Services;
using MhRecorder.Core.Services;
using Windows.Storage.Pickers;

namespace MhRecorder.App.Platform;

/// <summary>File / clipboard / window-capture access for a WinUI desktop app.</summary>
public sealed class WindowsImageSource : IImageSource
{
    public async Task<IReadOnlyList<ImageFile>> PickFilesAsync(CancellationToken ct = default)
    {
        var picker = new FileOpenPicker
        {
            ViewMode = PickerViewMode.Thumbnail,
            SuggestedStartLocation = PickerLocationId.PicturesLibrary,
        };
        foreach (var extension in ImageImportService.SupportedExtensions)
            picker.FileTypeFilter.Add(extension);

        Initialize(picker);

        var files = await picker.PickMultipleFilesAsync();
        if (files is null || files.Count == 0) return Array.Empty<ImageFile>();

        return await ImageImportService.ReadFilesAsync(files.Select(f => f.Path), ct);
    }

    public async Task<ClipboardRead> ReadClipboardAsync(CancellationToken ct = default)
    {
        var clip = await ClipboardImage.ReadAsync();
        var items = clip.Items
            .Select(i => new ImageFile(i.Name, i.Bytes, "剪贴板截图"))
            .ToList();
        return new ClipboardRead(items, clip.Message);
    }

    public Task<IReadOnlyList<WindowTarget>> ListWindowsAsync(CancellationToken ct = default)
    {
        var windows = WindowCapture.List()
            .Select(w => new WindowTarget(w.Handle, w.Title, w.ProcessName, w.Width, w.Height))
            .ToList();
        return Task.FromResult<IReadOnlyList<WindowTarget>>(windows);
    }

    public async Task<byte[]?> CaptureWindowAsync(WindowTarget target, CancellationToken ct = default)
    {
        var legacy = new CapturedWindow(target.Handle, target.Title, target.ProcessName, target.Width, target.Height);
        try
        {
            return await WindowCapture.CaptureAsync(legacy);
        }
        finally
        {
            // Capturing a GPU window raises it above ours; come back so the
            // recognition result is actually on screen.
            if (App.MainWindowInstance is { } window)
                WindowCapture.BringToFront(WinRT.Interop.WindowNative.GetWindowHandle(window));
        }
    }

    private static void Initialize(object picker)
    {
        if (App.MainWindowInstance is not { } window) return;
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);
    }
}

/// <summary>Save-file picker and Explorer reveal.</summary>
public sealed class WindowsFileTarget : IFileSaveTarget
{
    public async Task<string?> PickSavePathAsync(string suggestedFileName, string extension, string typeLabel)
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
            SuggestedFileName = Path.GetFileNameWithoutExtension(suggestedFileName),
        };
        picker.FileTypeChoices.Add(typeLabel, new List<string> { extension });

        if (App.MainWindowInstance is not { } window) return null;
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

        var file = await picker.PickSaveFileAsync();
        return file?.Path;
    }

    public void RevealInExplorer(string path)
    {
        try
        {
            if (string.IsNullOrEmpty(path) || !File.Exists(path)) return;
            System.Diagnostics.Process.Start("explorer.exe", $"/select,\"{path}\"");
        }
        catch (Exception ex)
        {
            App.Log($"RevealInExplorer failed: {ex.Message}");
        }
    }
}
