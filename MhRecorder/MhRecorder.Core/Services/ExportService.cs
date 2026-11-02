// 导出与备份。
//
// 导出以前是设置页里的一段：new FileSavePicker、拿窗口句柄初始化、调后端、把
// 结果拼成一句话显示。选文件属于平台能力（需要 HWND），导出属于业务，分开以后
// 设置页只剩一个命令绑定。

namespace MhRecorder.Core.Services;

/// <summary>Save-file picker, implemented by the app (pickers need a window handle).</summary>
public interface IFileSaveTarget
{
    /// <summary>Ask for a destination. Null when the user cancels.</summary>
    Task<string?> PickSavePathAsync(string suggestedFileName, string extension, string typeLabel);

    /// <summary>Reveal a file in Explorer (best effort).</summary>
    void RevealInExplorer(string path);
}

public sealed class ExportService
{
    private readonly BackendService _backend;
    private readonly IFileSaveTarget _files;

    public ExportService(BackendService backend, IFileSaveTarget files)
    {
        _backend = backend;
        _files = files;
    }

    /// <summary>
    /// Ask for a path and export. Returns a sentence to show in the status line,
    /// which is the only thing the settings page needs.
    /// </summary>
    public async Task<string> ExportAsync(ExportFormat format, CancellationToken ct = default)
    {
        var (extension, label) = format switch
        {
            ExportFormat.Csv => (".csv", "CSV 表格"),
            ExportFormat.Zip => (".zip", "ZIP 备份"),
            _ => (".json", "JSON 数据"),
        };

        var suggested = $"mhrecorder-{DateTime.Now:yyyyMMdd-HHmm}{extension}";
        var path = await _files.PickSavePathAsync(suggested, extension, label).ConfigureAwait(false);
        if (string.IsNullOrEmpty(path)) return "已取消导出";

        try
        {
            var result = await _backend.Api.ExportAsync(format, path, ct).ConfigureAwait(false);
            if (result is null) return "导出失败：后端没有返回结果";
            _files.RevealInExplorer(result.Path);
            return $"已导出 {result.Bytes:N0} 字节 → {result.Path}";
        }
        catch (Exception ex)
        {
            AppLog.Write($"ExportService.Export failed: {ex}");
            return "导出失败：" + ex.Message;
        }
    }

    /// <summary>Full backup (JSON + SQLite snapshot + manifest) to a user-picked zip.</summary>
    public async Task<string> BackupAsync(CancellationToken ct = default)
    {
        var suggested = $"mhrecorder-backup-{DateTime.Now:yyyyMMdd-HHmm}.zip";
        var path = await _files.PickSavePathAsync(suggested, ".zip", "完整备份").ConfigureAwait(false);
        if (string.IsNullOrEmpty(path)) return "已取消备份";

        try
        {
            var result = await _backend.Api.ExportAsync(ExportFormat.Backup, path, ct).ConfigureAwait(false);
            if (result is null) return "备份失败：后端没有返回结果";
            _files.RevealInExplorer(result.Path);
            return $"已备份 {result.Bytes:N0} 字节 → {result.Path}";
        }
        catch (Exception ex)
        {
            AppLog.Write($"ExportService.Backup failed: {ex}");
            return "备份失败：" + ex.Message;
        }
    }
}
