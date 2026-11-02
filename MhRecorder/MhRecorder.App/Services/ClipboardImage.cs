using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Streams;

namespace MhRecorder.App.Services;

/// <summary>
/// Read images off the Windows clipboard.
///
/// The 0.9.3 "粘贴截图" button only logged and replayed demo data, so against a
/// live backend it looked like nothing happened. This does the real thing.
///
/// Three clipboard flavours are handled, because users produce all three:
///  * <c>Bitmap</c>  — Win+Shift+S / Snipping Tool / a browser image;
///  * <c>StorageItems</c> — one or more PNG/JPG files copied in Explorer, which is
///    how people usually move screenshots around on disk;
///  * nothing usable — return a message the page can show instead of failing silent.
///
/// Bitmap bytes come back in whatever format the producer used: PNG (the common
/// case), sometimes JPEG or BMP from a copied file, and occasionally a bare
/// CF_DIB blob with no file header. A DIB is a BMP minus its 14-byte
/// <c>BITMAPFILEHEADER</c>, so it gets wrapped here rather than rejected.
/// </summary>
public static class ClipboardImage
{
    /// <summary>Images found on the clipboard, plus a user-facing message when there are none.</summary>
    public sealed record Result(IReadOnlyList<(string Name, byte[] Bytes)> Items, string Message);

    private static readonly string[] ImageExtensions = { ".png", ".jpg", ".jpeg", ".bmp" };

    public static async Task<Result> ReadAsync()
    {
        DataPackageView content;
        try
        {
            content = Clipboard.GetContent();
        }
        catch (Exception ex)
        {
            App.Log($"ClipboardImage: GetContent failed: {ex.Message}");
            return new Result(Array.Empty<(string, byte[])>(), "读不到剪贴板（可能正被其他程序占用），稍后再试一次");
        }

        if (Has(content, StandardDataFormats.Bitmap))
        {
            var bitmap = await ReadBitmapAsync(content);
            if (bitmap is not null)
            {
                return new Result(new[] { ("剪贴板截图.png", bitmap) }, string.Empty);
            }
            return new Result(Array.Empty<(string, byte[])>(), "读取剪贴板图片失败，重新复制一次试试");
        }

        // Explorer "copy" puts files on the clipboard instead of pixels.
        if (Has(content, StandardDataFormats.StorageItems))
        {
            var files = await ReadFilesAsync(content);
            if (files.Count > 0) return new Result(files, string.Empty);
        }

        return new Result(Array.Empty<(string, byte[])>(), Has(content, StandardDataFormats.Text)
            ? "剪贴板里是文字，不是图片。可以按 Win+Shift+S 截一张后再粘贴，或直接点「截图识别」抓取游戏窗口"
            : "剪贴板里没有图片。可以按 Win+Shift+S 截一张后再粘贴，或直接点「截图识别」抓取游戏窗口");
    }

    private static bool Has(DataPackageView content, string format)
    {
        try
        {
            return content.Contains(format);
        }
        catch (Exception ex)
        {
            App.Log($"ClipboardImage: Contains({format}) failed: {ex.Message}");
            return false;
        }
    }

    private static async Task<byte[]?> ReadBitmapAsync(DataPackageView content)
    {
        byte[] raw;
        try
        {
            var reference = await content.GetBitmapAsync();
            using var stream = await reference.OpenReadAsync();
            var size = (int)stream.Size;
            if (size <= 0) return null;

            raw = new byte[size];
            using var reader = new DataReader(stream.GetInputStreamAt(0));
            await reader.LoadAsync((uint)size);
            reader.ReadBytes(raw);
        }
        catch (Exception ex)
        {
            App.Log($"ClipboardImage: bitmap read failed: {ex.Message}");
            return null;
        }

        return Normalize(raw);
    }

    private static async Task<List<(string Name, byte[] Bytes)>> ReadFilesAsync(DataPackageView content)
    {
        var items = new List<(string, byte[])>();
        try
        {
            var storageItems = await content.GetStorageItemsAsync();
            foreach (var file in storageItems.OfType<StorageFile>())
            {
                if (!ImageExtensions.Contains(file.FileType.ToLowerInvariant())) continue;
                try
                {
                    using var stream = await file.OpenReadAsync();
                    var size = (int)stream.Size;
                    if (size <= 0) continue;
                    var bytes = new byte[size];
                    using var reader = new DataReader(stream.GetInputStreamAt(0));
                    await reader.LoadAsync((uint)size);
                    reader.ReadBytes(bytes);
                    items.Add((file.Name, bytes));
                }
                catch (Exception ex)
                {
                    App.Log($"ClipboardImage: file read failed ({file.Name}): {ex.Message}");
                }
            }
        }
        catch (Exception ex)
        {
            App.Log($"ClipboardImage: GetStorageItemsAsync failed: {ex.Message}");
        }
        return items;
    }

    /// <summary>Return bytes the backend can decode: PNG/JPEG/BMP pass through, a DIB gets a header.</summary>
    private static byte[]? Normalize(byte[] raw)
    {
        if (raw.Length > 8 && raw[0] == 0x89 && raw[1] == 0x50 && raw[2] == 0x4E && raw[3] == 0x47)
            return raw; // PNG
        if (raw.Length > 3 && raw[0] == 0xFF && raw[1] == 0xD8)
            return raw; // JPEG
        if (raw.Length > 2 && raw[0] == 'B' && raw[1] == 'M')
            return raw; // BMP (already has a file header)

        return WrapBareDib(raw);
    }

    /// <summary>
    /// Prepend a `BITMAPFILEHEADER` to a headerless DIB, producing a valid BMP.
    /// Field offsets follow the BITMAPINFOHEADER layout.
    /// </summary>
    private static byte[]? WrapBareDib(byte[] dib)
    {
        if (dib.Length < 40) return null;

        var headerSize = BitConverter.ToInt32(dib, 0);
        if (headerSize is < 40 or > 1024) return null;

        var bitCount = BitConverter.ToInt16(dib, 14);
        var compression = BitConverter.ToInt32(dib, 16);
        // BI_RGB and BI_BITFIELDS are what screenshot producers emit; anything
        // else (RLE, JPEG/PNG-in-DIB) is not worth guessing at.
        if (compression is not (0 or 3)) return null;
        if (bitCount is not (1 or 4 or 8 or 16 or 24 or 32)) return null;

        var colorUsed = BitConverter.ToInt32(dib, 32);
        var paletteBytes = compression == 3
            ? 12 // three colour masks follow the 40-byte header
            : bitCount <= 8
                ? (colorUsed > 0 ? colorUsed : 1 << bitCount) * 4
                : 0;

        var pixelOffset = 14 + headerSize + paletteBytes;
        var fileSize = 14 + dib.Length;
        var bmp = new byte[fileSize];

        bmp[0] = (byte)'B';
        bmp[1] = (byte)'M';
        BitConverter.GetBytes(fileSize).CopyTo(bmp, 2);
        BitConverter.GetBytes(pixelOffset).CopyTo(bmp, 10);
        // `Buffer` is ambiguous here: Windows.Storage.Streams.Buffer vs System.Buffer.
        System.Buffer.BlockCopy(dib, 0, bmp, 14, dib.Length);
        return bmp;
    }
}
