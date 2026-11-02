using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace MhRecorder.App.Services;

/// <summary>One capturable top-level window.</summary>
/// <param name="Handle">Native HWND.</param>
/// <param name="Title">Window caption, shown in the picker.</param>
/// <param name="ProcessName">Owning process (e.g. <c>MyGame_x64r</c>).</param>
/// <param name="Width">Client width in physical pixels.</param>
/// <param name="Height">Client height in physical pixels.</param>
public sealed record CapturedWindow(IntPtr Handle, string Title, string ProcessName, int Width, int Height);

/// <summary>
/// Enumerate visible top-level windows and capture one of them as PNG bytes.
///
/// Why not the WinRT <c>GraphicsCapture</c> APIs: they need a Direct3D device and
/// a frame pool, which drags Win2D/D3D interop into the app. Grabbing a single
/// frame is enough here, so this uses the two classic paths:
///
///  1. <c>PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT)</c> — no focus steal,
///     works for ordinary GDI/XAML windows;
///  2. if that comes back blank (typical for D3D/GPU-composited game windows, which
///     return an all-black surface), bring the window forward and <c>BitBlt</c> the
///     screen region instead. The user asked for this button *because* the game
///     window is the thing being captured, so the fallback is the common path in
///     practice and must not be an afterthought.
///
/// Pixel buffers come from a top-down 32bpp DIB section, so no GDI+ /
/// System.Drawing dependency is needed; PNG encoding goes through WinRT's
/// <see cref="BitmapEncoder"/>.
/// </summary>
public static class WindowCapture
{
    private const uint PwClientOnly = 0x0000_0001;
    private const uint PwRenderFullContent = 0x0000_0002;
    private const uint Srccopy = 0x00CC_0020;
    private const int SwRestore = 9;
    private const int SwShow = 5;
    private const int GwlExstyle = -20;
    private const uint WsExToolwindow = 0x0000_0080;
    private const uint WsExNoactivate = 0x0800_0000;
    /// <summary>Sanity bound: a 8K capture is already absurd for OCR.</summary>
    private const int MaxDimension = 8192;

    /// <summary>Shell/IME/host windows that are never what the user means.</summary>
    private static readonly HashSet<string> ExcludedClasses = new(StringComparer.OrdinalIgnoreCase)
    {
        "Progman",
        "WorkerW",
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
        "tooltips_class32",
        "Windows.UI.Core.CoreWindow",
        "MSCTFIME UI",
        "IME",
        "GDI+ Hook Window Class",
        "SysShadow",
        "ForegroundStaging",
    };

    /// <summary>Visible top-level windows that have a caption and a real client area.</summary>
    public static List<CapturedWindow> List()
    {
        var ownPid = (uint)Environment.ProcessId;
        var found = new List<CapturedWindow>();

        Native.EnumWindows((hwnd, _) =>
        {
            if (!Native.IsWindowVisible(hwnd)) return true;
            if (Native.IsIconic(hwnd)) return true;

            var exStyle = (uint)Native.GetWindowLong(hwnd, GwlExstyle);
            if ((exStyle & (WsExToolwindow | WsExNoactivate)) != 0) return true;

            Native.GetWindowThreadProcessId(hwnd, out var pid);
            if (pid == ownPid) return true;

            if (Native.GetWindowTextLength(hwnd) <= 0) return true;
            var titleBuf = new StringBuilder(512);
            Native.GetWindowText(hwnd, titleBuf, titleBuf.Capacity);
            var title = titleBuf.ToString().Trim();
            if (title.Length == 0) return true;

            var classBuf = new StringBuilder(256);
            Native.GetClassName(hwnd, classBuf, classBuf.Capacity);
            if (ExcludedClasses.Contains(classBuf.ToString())) return true;

            if (!Native.GetClientRect(hwnd, out var rc)) return true;
            var width = rc.Right - rc.Left;
            var height = rc.Bottom - rc.Top;
            if (width < 200 || height < 150) return true;

            var process = "";
            try
            {
                process = Process.GetProcessById((int)pid).ProcessName;
            }
            catch
            {
                // Protected / exiting process: keep the window, drop the name.
            }

            found.Add(new CapturedWindow(hwnd, title, process, width, height));
            return true;
        }, IntPtr.Zero);

        return found;
    }

    /// <summary>
    /// Capture the client area of <paramref name="window"/> as PNG bytes.
    /// Returns <c>null</c> when the window cannot be captured at all.
    /// </summary>
    public static async Task<byte[]?> CaptureAsync(CapturedWindow window)
    {
        if (!Native.GetClientRect(window.Handle, out var client)) return null;
        var width = client.Right - client.Left;
        var height = client.Bottom - client.Top;
        if (width <= 0 || height <= 0) return null;
        if (width > MaxDimension || height > MaxDimension) return null;

        var origin = new POINT();
        if (!Native.ClientToScreen(window.Handle, ref origin)) return null;

        var info = new BITMAPINFO
        {
            bmiHeader = new BITMAPINFOHEADER
            {
                biSize = Marshal.SizeOf<BITMAPINFOHEADER>(),
                biWidth = width,
                // Negative height = top-down rows, so row 0 is the top of the image.
                biHeight = -height,
                biPlanes = 1,
                biBitCount = 32,
                biCompression = 0, // BI_RGB
            },
        };

        var screenDc = Native.GetDC(IntPtr.Zero);
        var memoryDc = Native.CreateCompatibleDC(screenDc);
        // CreateDIBSection gives us both an HDC to draw into and the raw pixels,
        // which is what lets us skip GDI+ entirely.
        var bitmap = Native.CreateDIBSection(screenDc, ref info, 0, out var bits, IntPtr.Zero, 0);
        if (bitmap == IntPtr.Zero || bits == IntPtr.Zero)
        {
            Native.DeleteDC(memoryDc);
            Native.ReleaseDC(IntPtr.Zero, screenDc);
            return null;
        }

        var previous = Native.SelectObject(memoryDc, bitmap);
        try
        {
            var printed = Native.PrintWindow(window.Handle, memoryDc, PwClientOnly | PwRenderFullContent);
            var pixels = ReadPixels(bits, width, height);

            if (!printed || IsBlank(pixels))
            {
                // GPU-composited windows (the game) answer PrintWindow with black.
                // Bring the window to the front and copy the screen instead.
                if (Native.IsIconic(window.Handle)) Native.ShowWindow(window.Handle, SwRestore);
                Native.SetForegroundWindow(window.Handle);
                await Task.Delay(320); // let composition settle before blitting

                Native.BitBlt(memoryDc, 0, 0, width, height, screenDc, origin.X, origin.Y, Srccopy);
                pixels = ReadPixels(bits, width, height);
                if (IsBlank(pixels)) return null;
            }

            var png = await EncodePngAsync(pixels, width, height);
            // MH_DUMP_CAPTURE=<dir> writes the raw frame out. Recognition
            // problems are undebuggable from a thumbnail — being able to hand
            // the exact bytes to `mhcli recognize` is the difference between
            // guessing and fixing.
            DumpIfRequested(png);
            return png;
        }
        finally
        {
            Native.SelectObject(memoryDc, previous);
            Native.DeleteObject(bitmap);
            Native.DeleteDC(memoryDc);
            Native.ReleaseDC(IntPtr.Zero, screenDc);
        }
    }

    /// <summary>
    /// Bring a window back to the foreground. Capturing a GPU-composited window
    /// requires raising it first, which leaves our own window behind the target —
    /// the result would be invisible without this.
    /// </summary>
    public static void BringToFront(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero) return;
        Native.ShowWindow(hwnd, SwShow);
        Native.SetForegroundWindow(hwnd);
    }

    /// <summary>
    /// Persist the last captured frame when <c>MH_DUMP_CAPTURE</c> points at a
    /// directory. Used by <c>scripts\dumphdr.ps1</c>-style debugging and by the
    /// OCR tuning loop: the exact PNG can be replayed with <c>mhcli recognize</c>.
    /// </summary>
    private static void DumpIfRequested(byte[] png)
    {
        var dir = Environment.GetEnvironmentVariable("MH_DUMP_CAPTURE");
        if (string.IsNullOrWhiteSpace(dir)) return;
        try
        {
            Directory.CreateDirectory(dir);
            var path = Path.Combine(dir, $"capture-{DateTime.Now:HHmmss-fff}.png");
            File.WriteAllBytes(path, png);
        }
        catch
        {
            // Debug helper only — never let it break a real capture.
        }
    }

    private static byte[] ReadPixels(IntPtr bits, int width, int height)    {
        var size = width * height * 4;
        var buffer = new byte[size];
        Marshal.Copy(bits, buffer, 0, size);
        return buffer;
    }

    /// <summary>
    /// True when the buffer is effectively one flat colour — the signature of a
    /// capture that produced nothing (black or fully transparent surfaces).
    /// </summary>
    private static bool IsBlank(byte[] bgra)
    {
        if (bgra.Length < 4) return true;

        byte min = 255;
        byte max = 0;
        // Sample ~20k pixels spread across the image rather than scanning 4M.
        var step = Math.Max(4, (bgra.Length / 20_000 / 4) * 4);
        for (var i = 0; i + 3 < bgra.Length; i += step)
        {
            var luma = (byte)((bgra[i] + bgra[i + 1] + bgra[i + 2]) / 3);
            if (luma < min) min = luma;
            if (luma > max) max = luma;
        }
        return max - min < 6;
    }

    /// <summary>Encode a top-down BGRA buffer as PNG via WinRT.</summary>
    private static async Task<byte[]> EncodePngAsync(byte[] bgra, int width, int height)
    {
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
        // Ignore alpha: GDI never writes meaningful alpha into a DIB section, and
        // "premultiplied with alpha 0" would encode a fully transparent image.
        encoder.SetPixelData(
            BitmapPixelFormat.Bgra8,
            BitmapAlphaMode.Ignore,
            (uint)width,
            (uint)height,
            96,
            96,
            bgra);
        await encoder.FlushAsync();

        var bytes = new byte[stream.Size];
        using var reader = new DataReader(stream.GetInputStreamAt(0));
        await reader.LoadAsync((uint)stream.Size);
        reader.ReadBytes(bytes);
        return bytes;
    }

    // -----------------------------------------------------------------------
    // Win32
    // -----------------------------------------------------------------------

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT
    {
        public int Left, Top, Right, Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT
    {
        public int X, Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFOHEADER
    {
        public int biSize;
        public int biWidth;
        public int biHeight;
        public short biPlanes;
        public short biBitCount;
        public int biCompression;
        public int biSizeImage;
        public int biXPelsPerMeter;
        public int biYPelsPerMeter;
        public int biClrUsed;
        public int biClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct RGBQUAD
    {
        public byte Blue, Green, Red, Reserved;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFO
    {
        public BITMAPINFOHEADER bmiHeader;
        public RGBQUAD bmiColors;
    }

    private delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    private static class Native
    {
        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr param);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsWindowVisible(IntPtr hWnd);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsIconic(IntPtr hWnd);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern int GetWindowTextLength(IntPtr hWnd);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int maxCount);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern int GetClassName(IntPtr hWnd, StringBuilder name, int maxCount);

        [DllImport("user32.dll")]
        public static extern int GetWindowLong(IntPtr hWnd, int index);

        [DllImport("user32.dll")]
        public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool SetForegroundWindow(IntPtr hWnd);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool ShowWindow(IntPtr hWnd, int command);

        [DllImport("user32.dll")]
        public static extern IntPtr GetDC(IntPtr hWnd);

        [DllImport("user32.dll")]
        public static extern int ReleaseDC(IntPtr hWnd, IntPtr hdc);

        [DllImport("gdi32.dll")]
        public static extern IntPtr CreateCompatibleDC(IntPtr hdc);

        [DllImport("gdi32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool DeleteDC(IntPtr hdc);

        [DllImport("gdi32.dll")]
        public static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);

        [DllImport("gdi32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool DeleteObject(IntPtr obj);

        [DllImport("gdi32.dll")]
        public static extern IntPtr CreateDIBSection(
            IntPtr hdc,
            ref BITMAPINFO info,
            uint usage,
            out IntPtr bits,
            IntPtr section,
            uint offset);

        [DllImport("gdi32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool BitBlt(
            IntPtr destDc,
            int x,
            int y,
            int width,
            int height,
            IntPtr sourceDc,
            int sourceX,
            int sourceY,
            uint rop);
    }
}
