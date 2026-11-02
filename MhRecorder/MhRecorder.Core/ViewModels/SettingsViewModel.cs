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

    partial void OnEditableBeforeSaveChanged(bool value) => PersistIfReady();

    partial void OnAutoNavigateAfterSaveChanged(bool value) => PersistIfReady();

    public async Task LoadAsync()
    {
        _loading = true;
        try
        {
            _settings = await _backend.Api.GetSettingsAsync();
            EditableBeforeSave = _settings.EditableBeforeSave;
            AutoNavigateAfterSave = _settings.AutoNavigateAfterSave;
            DataDir = _settings.DataDir;
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

    private async void PersistIfReady()
    {
        if (_loading) return;
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

            StatusText = _backend.IsRemote
                ? $"已是最新版本 {ProtocolConstants.AppVersion}（本地离线版）· 识别插件 {engineId} {updatable}"
                : $"当前为演示模式，版本 {ProtocolConstants.AppVersion} · 识别插件未连接";
        }
        catch (Exception ex)
        {
            AppLog.Write($"SettingsViewModel.CheckUpdate failed: {ex}");
            StatusText = "检查更新失败：" + ex.Message;
        }
    }
}
