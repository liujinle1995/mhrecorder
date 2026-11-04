// 设置页的 ViewModel。
//
// 设置页原来的负担不在"逻辑复杂"，而在把三件事混在一起：读写设置、把后端
// 插件信息翻译成三行文案、以及一堆 ContentDialog 的构造。后者搬去 Views/Dialogs，
// 前两件在这里。页面回到"开关绑属性"。

using System.Text.Json;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using MhRecorder.Core.Infrastructure;
using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.Core.ViewModels;

public sealed partial class SettingsViewModel : ObservableObject
{
    private readonly BackendService _backend;
    private readonly ExportService _exports;
    private readonly IDialogService _dialogs;

    private Settings _settings = new();

    /// <summary>True while the settings are being pushed onto the controls.</summary>
    private bool _loading;

    public SettingsViewModel(BackendService backend, ExportService exports, IDialogService dialogs)
    {
        _backend = backend;
        _exports = exports;
        _dialogs = dialogs;
    }

    [ObservableProperty]
    private bool _editableBeforeSave = true;

    [ObservableProperty]
    private bool _autoNavigateAfterSave;

    [ObservableProperty]
    private string _dataDir = "—";

    [ObservableProperty]
    private string _versionText = $"{ProtocolConstants.AppVersion}（本地版）";

    [ObservableProperty]
    private string _engineText = "—";

    [ObservableProperty]
    private string _engineVersionText = "—";

    [ObservableProperty]
    private string _pluginDetailText = "加载中…";

    [ObservableProperty]
    private string _changelogSummary = "—";

    [ObservableProperty]
    private string _statusText = string.Empty;

    /// <summary>
    /// "3 条" / "无" —— 没有身份信息、等着人工指定归属的识别结果。
    ///
    /// 这些记录以前会直接变成「未命名角色」，混在角色列表里永远匹配不上；
    /// 现在停在这里，等一句话就能归位。
    /// </summary>
    [ObservableProperty]
    private string _pendingText = "—";

    [ObservableProperty]
    private bool _hasPending;

    /// <summary>
    /// 识别性能档位的三个选项。顺序必须和 <see cref="ProfileIndex"/> 的映射一致。
    /// </summary>
    public IReadOnlyList<string> ProfileOptions { get; } = new[]
    {
        "低资源 · 2 线程",
        "平衡 · 4 线程",
        "高性能 · 按核心数",
    };

    /// <summary>0 = 低资源, 1 = 平衡, 2 = 高性能。</summary>
    [ObservableProperty]
    private int _profileIndex = 1;

    /// <summary>
    /// 后端实际生效的引擎参数（线程 / 批 / 检测后处理）。
    ///
    /// 和下拉框分开显示，是因为"选了高性能但实际只有 4 线程"这种情况必须
    /// 一眼看得见——档位是意图，这行才是事实。
    /// </summary>
    [ObservableProperty]
    private string _ocrDetailText = "—";

    /// <summary>
    /// False when there is no backend: the page then shows the values as "—" and
    /// every control is disabled, instead of rendering defaults that look like
    /// real settings and then failing to save.
    /// </summary>
    [ObservableProperty]
    private bool _isSettingsEditable = true;

    partial void OnEditableBeforeSaveChanged(bool value) => PersistIfReady();

    partial void OnAutoNavigateAfterSaveChanged(bool value) => PersistIfReady();

    partial void OnProfileIndexChanged(int value)
    {
        if (_loading) return;
        _settings.OcrProfile = value switch
        {
            0 => "low",
            2 => "high",
            _ => "balanced",
        };
        OcrDetailText = "正在切换识别引擎…";
        _ = SaveProfileAsync();
    }

    /// <summary>
    /// 保存档位并等后端重建引擎。
    ///
    /// 后端把 settings.set 做成"保存 + 就地重建"，所以保存返回时新配置已经
    /// 生效，紧接着读 version 拿到的就是实际参数，不需要轮询。
    /// </summary>
    private async Task SaveProfileAsync()
    {
        if (!_backend.IsAvailable)
        {
            StatusText = "后端未连接，设置改动不会保存";
            OcrDetailText = "—";
            return;
        }
        try
        {
            _settings.EditableBeforeSave = EditableBeforeSave;
            _settings.AutoNavigateAfterSave = AutoNavigateAfterSave;
            await _backend.Api.SetSettingsAsync(_settings);
            StatusText = $"识别性能已切换 · {DateTime.Now:HH:mm:ss}";
            await LoadEngineInfoAsync();
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.SaveProfile failed: {ex}");
            StatusText = "保存设置失败：" + ex.Message;
            OcrDetailText = "切换失败";
        }
    }

    public async Task LoadAsync()
    {
        // 没有后端时不要去"读设置"：读到的空对象会让开关看起来是关着的，
        // 用户以为设置丢了，而这个页面本身什么都改不了。
        if (!_backend.IsAvailable)
        {
            _loading = true;
            IsSettingsEditable = false;
            EngineText = "未连接（没有可用的后端）";
            EngineVersionText = "—";
            PluginDetailText = _backend.UnavailableReason ?? "后端未连接";
            StatusText = "后端未连接：设置、导出与清空数据都不可用";
            DataDir = "—";
            ChangelogSummary = ChangelogSummaryOf();
            PendingText = "—";
            HasPending = false;
            OcrDetailText = "—";
            _loading = false;
            return;
        }

        IsSettingsEditable = true;
        _loading = true;
        try
        {
            _settings = await _backend.Api.GetSettingsAsync();
            EditableBeforeSave = _settings.EditableBeforeSave;
            AutoNavigateAfterSave = _settings.AutoNavigateAfterSave;
            DataDir = _settings.DataDir;
            // _loading 期间赋值，OnProfileIndexChanged 只负责回写，不会反过来
            // 触发一次保存。
            ProfileIndex = _settings.OcrProfile switch
            {
                "low" => 0,
                "high" => 2,
                _ => 1,
            };
            VersionText = $"{ProtocolConstants.AppVersion}（本地版）";
            EngineText = _backend.IsRemote ? "离线 OCR · 不做任何上传（已连接引擎）" : "离线 OCR · 不做任何上传";
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.Load failed: {ex}");
            StatusText = "读取设置失败：" + ex.Message;
        }
        finally
        {
            _loading = false;
        }

        ChangelogSummary = ChangelogSummaryOf();
        await LoadEngineInfoAsync();
        await LoadPendingAsync();
    }

    /// <summary>Count the parked results. Cheap enough to call after every save.</summary>
    public async Task LoadPendingAsync()
    {
        if (!_backend.IsAvailable)
        {
            PendingText = "—";
            HasPending = false;
            return;
        }

        try
        {
            var items = await _backend.Api.ListPendingAsync();
            HasPending = items.Count > 0;
            PendingText = items.Count == 0 ? "无" : $"{items.Count} 条待归属";
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.LoadPending failed: {ex}");
            PendingText = "读取失败";
        }
    }

    private static string ChangelogSummaryOf()
    {
        var entries = Changelog.Entries;
        return entries.Length == 0 ? "—" : $"最新 {entries[0].Version} · 共 {entries.Length} 个版本";
    }

    /// <summary>
    /// Ask the backend which recognition plugin is active.
    ///
    /// The recogniser is modelled as a pluggable unit, so the UI reports the
    /// concrete engine id/runtime instead of a vague "离线 OCR" label — that is
    /// what makes a recognition result attributable.
    /// </summary>
    private async Task LoadEngineInfoAsync()
    {
        if (!_backend.IsAvailable) return;

        try
        {
            var info = await _backend.Api.GetEngineInfoAsync();
            if (info is null || info.Value.ValueKind != JsonValueKind.Object)
            {
                EngineText = "未连接（使用内置演示引擎）";
                EngineVersionText = "—";
                PluginDetailText = "后端未连接，无法读取插件信息";
                return;
            }

            // The `version` payload nests the plugin descriptor under "engine".
            var engine = info.Value.TryGetProperty("engine", out var e) && e.ValueKind == JsonValueKind.Object
                ? e
                : info.Value;

            string Str(string key) =>
                engine.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
                    ? v.GetString() ?? ""
                    : "";

            var active = engine.TryGetProperty("active", out var act) && act.ValueKind == JsonValueKind.True;
            var name = Str("name");
            EngineText = active
                ? $"{(name.Length > 0 ? name : "OCR")} · 已加载"
                : "OCR · 未加载";
            EngineVersionText = Str("version") is { Length: > 0 } ver ? ver : "—";
            OcrDetailText = DescribeOcr(info.Value);

            var models = engine.TryGetProperty("models", out var arr) && arr.ValueKind == JsonValueKind.Array
                ? string.Join("、", arr.EnumerateArray().Select(x => x.GetString()))
                : "—";
            PluginDetailText = $"{Str("id")}\n运行时：{Str("runtime")}\n模型：{models}";
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.LoadEngineInfo failed: {ex}");
            PluginDetailText = "读取失败：" + ex.Message;
        }
    }

    /// <summary>
    /// 把 version 里的 ocr 段翻成一句人话：档位 · 线程 · 批 · 检测后处理。
    ///
    /// 只显示后端**实际报上来**的值，不按档位推算——推算出来的"高性能"会
    /// 掩盖重建失败、环境变量覆盖这类真实状态。
    /// </summary>
    private static string DescribeOcr(JsonElement info)
    {
        if (!info.TryGetProperty("ocr", out var ocr) || ocr.ValueKind != JsonValueKind.Object)
        {
            return "—";
        }

        var profile = ocr.TryGetProperty("profile", out var p) && p.ValueKind == JsonValueKind.String
            ? p.GetString() ?? ""
            : "";
        var cores = ocr.TryGetProperty("cores", out var c) && c.ValueKind == JsonValueKind.Number
            ? c.GetInt32()
            : 0;

        string det = "—";
        var threads = 0;
        var batch = 0;
        if (ocr.TryGetProperty("config", out var cfg) && cfg.ValueKind == JsonValueKind.Object)
        {
            det = cfg.TryGetProperty("det", out var d) && d.ValueKind == JsonValueKind.String
                ? d.GetString() ?? "—"
                : "—";
            threads = cfg.TryGetProperty("threads", out var t) && t.ValueKind == JsonValueKind.Number
                ? t.GetInt32()
                : 0;
            batch = cfg.TryGetProperty("batchSize", out var b) && b.ValueKind == JsonValueKind.Number
                ? b.GetInt32()
                : 0;
        }

        if (threads == 0 && batch == 0)
        {
            return "引擎未加载";
        }

        var label = profile switch
        {
            "low" => "低资源",
            "high" => "高性能",
            _ => "平衡",
        };
        var detLabel = det switch
        {
            "db" => "DB 轮廓",
            "legacy" => "四邻域",
            _ => det,
        };
        var coreNote = cores > 0 ? $" · {cores} 逻辑核" : "";
        return $"{label} · {threads} 线程 · 批 {batch} · {detLabel}后处理{coreNote}";
    }

    private async void PersistIfReady()
    {
        if (_loading) return;

        if (!_backend.IsAvailable)
        {
            StatusText = "后端未连接，设置改动不会保存";
            return;
        }
        _settings.EditableBeforeSave = EditableBeforeSave;
        _settings.AutoNavigateAfterSave = AutoNavigateAfterSave;
        try
        {
            await _backend.Api.SetSettingsAsync(_settings);
            StatusText = $"设置已保存 · {DateTime.Now:HH:mm:ss}";
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.Persist failed: {ex}");
            StatusText = "保存设置失败：" + ex.Message;
        }
    }

    [RelayCommand]
    private async Task ExportJsonAsync() => StatusText = await _exports.ExportAsync(ExportFormat.Json);

    [RelayCommand]
    private async Task ExportCsvAsync() => StatusText = await _exports.ExportAsync(ExportFormat.Csv);

    [RelayCommand]
    private async Task BackupAsync() => StatusText = await _exports.BackupAsync();

    [RelayCommand]
    private async Task ClearDataAsync()
    {
        if (!await _dialogs.ConfirmAsync(
                "清空本地数据", "这会删除全部角色与历史记录，且无法恢复。确定继续吗？", "清空"))
        {
            return;
        }

        try
        {
            await _backend.Api.ClearDataAsync();
            StatusText = "本地数据已清空";
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.ClearData failed: {ex}");
            StatusText = "清空失败：" + ex.Message;
        }
    }

    [RelayCommand]
    private async Task CheckUpdateAsync()
    {
        StatusText = "检查中…";
        try
        {
            // This build is offline-first: there is no update server, so the
            // honest answer is "you are on the newest local build". We still
            // surface the engine plugin's own updatability.
            var info = await _backend.Api.GetEngineInfoAsync();
            var updatable = "不可在线更新";
            var engineId = "—";
            if (info is { ValueKind: JsonValueKind.Object } obj)
            {
                var engine = obj.TryGetProperty("engine", out var engineEl) && engineEl.ValueKind == JsonValueKind.Object
                    ? engineEl
                    : obj;
                if (engine.TryGetProperty("updatable", out var up) && up.ValueKind == JsonValueKind.True)
                    updatable = "支持在线更新";
                if (engine.TryGetProperty("id", out var idEl) && idEl.ValueKind == JsonValueKind.String)
                    engineId = idEl.GetString() ?? "—";
            }

            StatusText = _backend.State switch
            {
                BackendState.Ready =>
                    $"已是最新版本 {ProtocolConstants.AppVersion}（本地离线版）· 识别插件 {engineId} {updatable}",
                BackendState.Demo =>
                    $"当前为演示数据（未连接后端），版本 {ProtocolConstants.AppVersion} · 识别插件未连接",
                _ => $"后端未连接，无法检查版本（当前 {ProtocolConstants.AppVersion}）",
            };
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.CheckUpdate failed: {ex}");
            StatusText = "检查更新失败：" + ex.Message;
        }
    }
}
