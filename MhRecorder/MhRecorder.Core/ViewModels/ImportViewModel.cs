// 录入与审核页的 ViewModel。
//
// 这一页原来是 936 行 code-behind：拖放、剪贴板、窗口截图、识别、预览、保存、
// 身份审核全在里面。拆完之后页面只剩下"把控件和属性连起来"，规则都在这里：
//   · 一次识别怎么算成功（至少读到一个数值）
//   · 未识别身份的截图必须先指定归属才能保存
//   · 人工改过的值优先于 OCR 值
//   · 保存后是否自动跳到角色管理

using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using MhRecorder.Core.Infrastructure;
using MhRecorder.Core.Models;
using MhRecorder.Core.Services;

namespace MhRecorder.Core.ViewModels;

/// <summary>One imported capture, shown as a tab above the image viewer.</summary>
public sealed class ImageTabViewModel
{
    public ImageTabViewModel(int index, string name, string source, ScreenKind screen)
    {
        Index = index;
        Name = name;
        Source = source;
        Screen = screen;
    }

    public int Index { get; }
    public string Name { get; }
    public string Source { get; }
    public ScreenKind Screen { get; }

    public string Label => $"{ScreenLabel(Screen)} · {Name}";

    public static string ScreenLabel(ScreenKind kind) => kind switch
    {
        ScreenKind.CharacterAttributes => "人物属性",
        ScreenKind.Inventory => "背包",
        _ => "未识别",
    };
}

/// <summary>A character the capture can be attributed to.</summary>
public sealed record OwnerOption(string Id, string Label)
{
    public override string ToString() => Label;
}

/// <summary>One row of the "身份关联" strip.</summary>
public sealed partial class IdentityCandidateRowViewModel : ObservableObject
{
    public IdentityCandidateRowViewModel(
        IdentityCandidate candidate,
        bool isBest,
        Action<IdentityCandidateRowViewModel> confirm,
        Action<IdentityCandidateRowViewModel> reject)
    {
        Candidate = candidate;
        IsBest = isBest;
        ConfirmCommand = new RelayCommand(() => confirm(this));
        RejectCommand = new RelayCommand(() => reject(this));
    }

    public IdentityCandidate Candidate { get; }
    public bool IsBest { get; }

    /// <summary>The row owns its two actions, so the template needs no ancestor binding.</summary>
    public IRelayCommand ConfirmCommand { get; }

    public IRelayCommand RejectCommand { get; }

    public string CharacterId => Candidate.CharacterId;
    public string Nickname => string.IsNullOrWhiteSpace(Candidate.Nickname) ? Candidate.CharacterId : Candidate.Nickname;
    public string ConfidenceText => $"{Candidate.Confidence:P0}";

    public string Detail
    {
        get
        {
            var parts = new List<string>();
            if (!string.IsNullOrWhiteSpace(Candidate.GameId)) parts.Add($"ID {Candidate.GameId}");
            if (Candidate.Level is { } lvl) parts.Add($"Lv.{lvl}");
            if (!string.IsNullOrWhiteSpace(Candidate.Server)) parts.Add(Candidate.Server);
            return string.Join(" · ", parts);
        }
    }

    /// <summary>Per-signal evidence, strongest first — "为什么怀疑是它".</summary>
    public string EvidenceText => Candidate.Evidence.Count == 0
        ? "无匹配依据"
        : string.Join("；", Candidate.Evidence.Take(3).Select(e => e.Detail));

    public bool IsStrong => Candidate.Confidence >= 0.75;
}

public sealed partial class ImportViewModel : ObservableObject
{
    private readonly BackendService _backend;
    private readonly ImageImportService _images;
    private readonly RecognitionService _recognition;
    private readonly IDialogService _dialogs;
    private readonly INavigationService _navigation;
    private readonly IWindowPicker _windowPicker;

    private IReadOnlyList<ImageFile> _batch = Array.Empty<ImageFile>();
    private string? _savedIdentityFrom;

    public ImportViewModel(
        BackendService backend,
        ImageImportService images,
        RecognitionService recognition,
        IDialogService dialogs,
        INavigationService navigation,
        IWindowPicker windowPicker)
    {
        _backend = backend;
        _images = images;
        _recognition = recognition;
        _dialogs = dialogs;
        _navigation = navigation;
        _windowPicker = windowPicker;
    }

    // ------------------------------------------------------------------
    // State
    // ------------------------------------------------------------------

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasResult))]
    [NotifyPropertyChangedFor(nameof(ShowEmptyResult))]
    [NotifyPropertyChangedFor(nameof(ShowDropHint))]
    [NotifyPropertyChangedFor(nameof(ShowNoImageHint))]
    private RecognitionResult? _result;

    [ObservableProperty]
    private bool _isBusy;

    [ObservableProperty]
    private string _busyText = "正在识别截图…";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasPreview))]
    [NotifyPropertyChangedFor(nameof(ShowDropHint))]
    [NotifyPropertyChangedFor(nameof(ShowNoImageHint))]
    private byte[]? _previewBytes;

    [ObservableProperty]
    private string _previewCaption = string.Empty;

    [ObservableProperty]
    private ObservableCollection<ImageTabViewModel> _imageTabs = new();

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(Regions))]
    [NotifyPropertyChangedFor(nameof(SelectedScreenLabel))]
    private ImageTabViewModel? _selectedImageTab;

    /// <summary>Switching tab swaps the picture, its boxes and the caption together.</summary>
    partial void OnSelectedImageTabChanged(ImageTabViewModel? value)
    {
        if (value is null)
        {
            PreviewBytes = null;
            PreviewCaption = string.Empty;
            Regions.Clear();
            return;
        }

        var shot = _batch.FirstOrDefault(i => i.Name == value.Name);
        PreviewBytes = shot is { Bytes.Length: > 0 } ? shot.Bytes : null;
        PreviewCaption = PreviewBytes is null
            ? $"{value.Label} · 演示数据（没有原始截图）"
            : $"{value.Label} · {value.Source}";
        RefreshRegions();
    }

    partial void OnSelectedFieldChanged(FieldRowViewModel? value) =>
        HighlightedKey = value?.Key;

    [ObservableProperty]
    private ObservableCollection<FieldRegion> _regions = new();

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasFields))]
    private ObservableCollection<FieldRowViewModel> _fields = new();

    [ObservableProperty]
    private FieldRowViewModel? _selectedField;

    [ObservableProperty]
    private ObservableCollection<IdentityCandidateRowViewModel> _candidates = new();

    [ObservableProperty]
    private ObservableCollection<OwnerOption> _owners = new();

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CanSave))]
    private OwnerOption? _selectedOwner;

    [ObservableProperty]
    private bool _showOwnerPicker;

    [ObservableProperty]
    private string _ownerHint = string.Empty;

    [ObservableProperty]
    private string _nickname = "未识别角色";

    [ObservableProperty]
    private string _levelText = "Lv.?";

    [ObservableProperty]
    private string _identityLine = string.Empty;

    [ObservableProperty]
    private string _sourceLine = string.Empty;

    [ObservableProperty]
    private string _evidenceText = string.Empty;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasEvidence))]
    private string _appearanceText = string.Empty;

    [ObservableProperty]
    private string _confidenceText = "—";

    [ObservableProperty]
    private double _confidenceRatio;

    [ObservableProperty]
    private bool _isConfidenceLow;

    [ObservableProperty]
    private string _identityStateText = string.Empty;

    [ObservableProperty]
    private string _identityStateDetail = string.Empty;

    [ObservableProperty]
    private bool _identityIsResolved;

    [ObservableProperty]
    private string? _avatarPngBase64;

    [ObservableProperty]
    private string _avatarInitial = "角";

    /// <summary>Highlight box for one field, set when a row is selected/edited.</summary>
    [ObservableProperty]
    private string? _highlightedKey;

    [ObservableProperty]
    private string? _hintMessage;

    [ObservableProperty]
    private string _hintSeverity = "Informational";

    /// <summary>Drives InfoBar.IsOpen; the user closing the bar must stick.</summary>
    [ObservableProperty]
    private bool _isHintOpen;

    public bool HasResult => Result is not null;
    public bool ShowEmptyResult => Result is null && !IsBusy;
    public bool HasPreview => PreviewBytes is not null;

    /// <summary>Nothing imported yet: show the "drop a capture here" hint.</summary>
    public bool ShowDropHint => PreviewBytes is null && Result is null && !IsBusy;

    /// <summary>A result with no capture behind it (demo data, or a read failure).</summary>
    public bool ShowNoImageHint => PreviewBytes is null && Result is not null;
    public bool HasFields => Fields.Count > 0;
    public bool HasEvidence => !string.IsNullOrWhiteSpace(EvidenceText);
    public bool HasAppearance => !string.IsNullOrWhiteSpace(AppearanceText);
    public bool HasCandidates => Candidates.Count > 0;

    /// <summary>Inverse of <see cref="HasCandidates"/>: show the state line instead of the list.</summary>
    public bool IdentityNeedsAttention => !HasCandidates;
    public bool CanSave => Result is not null && !IsBusy;

    public string SelectedScreenLabel => SelectedImageTab?.Label ?? string.Empty;

    /// <summary>
    /// "这次有几张图没重新 OCR"。识别结果里本来就带 cacheHits，把条数翻成人话：
    /// 用户看到"秒出"时会怀疑是不是跳过了什么，这里正面回答。
    /// </summary>
    [ObservableProperty]
    private string _cacheNote = string.Empty;

    /// <summary>One sentence explaining that nothing can be recorded right now.</summary>
    private string BackendUnavailableText =>
        _backend.UnavailableReason
        ?? "未连接本地识别引擎，识别与保存都不可用。请退出后重新启动程序。";

    // ------------------------------------------------------------------
    // Commands — import
    // ------------------------------------------------------------------

    [RelayCommand]
    private async Task SelectFilesAsync()
    {
        if (IsBusy) return;
        try
        {
            var files = await _images.PickFilesAsync();
            if (files.Count == 0) return;
            await RunRecognitionAsync(files, "文件");
        }
        catch (Exception ex)
        {
            AppLog.Write($"ImportViewModel.SelectFiles failed: {ex}");
            ShowHint("打开文件失败：" + ex.Message, "Error");
        }
    }

    /// <summary>Drag-and-drop entry point: same pipeline as the file picker.</summary>
    public async Task ImportPathsAsync(IReadOnlyList<string> paths)
    {
        if (paths.Count == 0 || IsBusy) return;

        var files = await ImageImportService.ReadFilesAsync(paths);
        if (files.Count == 0)
        {
            ShowHint("这些文件读不出来，换 png / jpg 再试一次", "Warning");
            return;
        }

        await RunRecognitionAsync(files, "拖入文件");
    }

    [RelayCommand]
    private async Task PasteAsync()
    {
        if (IsBusy) return;
        try
        {
            var clip = await _images.ReadClipboardAsync();
            if (clip.Items.Count == 0)
            {
                ShowHint(clip.Message, "Warning");
                return;
            }
            await RunRecognitionAsync(clip.Items, "剪贴板截图");
        }
        catch (Exception ex)
        {
            AppLog.Write($"ImportViewModel.Paste failed: {ex}");
            ShowHint("粘贴失败：" + ex.Message, "Error");
        }
    }

    [RelayCommand]
    private async Task CaptureAsync()
    {
        if (IsBusy) return;
        try
        {
            var windows = await _images.ListWindowsAsync();
            if (windows.Count == 0)
            {
                ShowHint("没有找到可截取的应用窗口。请先把游戏窗口从最小化恢复，再点一次", "Warning");
                return;
            }

            var target = await _windowPicker.PickAsync(windows);
            if (target is null) return;

            BusyText = "正在截取窗口…";
            IsBusy = true;
            ImageFile? shot;
            try
            {
                shot = await _images.CaptureWindowAsync(target);
            }
            finally
            {
                IsBusy = false;
            }

            if (shot is null)
            {
                ShowHint(
                    $"没能截取「{target.Title}」。若是独占全屏的游戏窗口，请先切成窗口/无边框模式再试",
                    "Warning");
                return;
            }

            var outcome = await RunRecognitionAsync(new[] { shot }, target.Title);
            if (outcome is null) return;

            // A captured game window is usually *not* the character panel (the
            // player has to open 人物属性 / 背包 first), so say what to do next
            // instead of claiming success on an empty read.
            ShowHint(
                "已截取窗口画面并完成识别",
                "Success");
        }
        catch (Exception ex)
        {
            AppLog.Write($"ImportViewModel.Capture failed: {ex}");
            ShowHint("截图失败：" + ex.Message, "Error");
        }
    }

    [RelayCommand]
    private void ClearPreview()
    {
        PreviewBytes = null;
        PreviewCaption = string.Empty;
    }

    [RelayCommand]
    private async Task UndoAsync()
    {
        // Corrections are hand-typed work: throwing them away on a stray click
        // is the one destructive thing this page can do.
        if (Fields.Any(f => f.IsEdited)
            && !await _dialogs.ConfirmAsync("撤销本次录入", "已经手工修改过的字段会一起丢弃，确定撤销吗？", "撤销"))
        {
            return;
        }

        Result = null;
        _batch = Array.Empty<ImageFile>();
        _savedIdentityFrom = null;
        ImageTabs.Clear();
        SelectedImageTab = null;
        Fields.Clear();
        Candidates.Clear();
        Regions.Clear();
        Owners.Clear();
        ShowOwnerPicker = false;
        PreviewBytes = null;
        PreviewCaption = string.Empty;
        CacheNote = string.Empty;
        ConfidenceText = "—";
        ConfidenceRatio = 0;
        IdentityStateText = string.Empty;
        IdentityStateDetail = string.Empty;
        IdentityIsResolved = false;
        EvidenceText = string.Empty;
        AppearanceText = string.Empty;
        AvatarPngBase64 = null;
        ShowHint("已撤销本次录入", "Informational");
    }

    // ------------------------------------------------------------------
    // Commands — review
    // ------------------------------------------------------------------

    [RelayCommand]
    private void RevertField(FieldRowViewModel? row) => row?.Revert();

    /// <summary>Accept a candidate: the capture now belongs to that character.</summary>
    [RelayCommand]
    private async Task ConfirmCandidateAsync(IdentityCandidateRowViewModel? row)
    {
        if (row is null || Result is null) return;
        try
        {
            await _recognition.AttributeToAsync(Result, row.CharacterId);
            _savedIdentityFrom = row.Nickname;
            Candidates.Clear();
            ShowOwnerPicker = false;
            RefreshFromResult(keepFields: false);
            ShowHint($"已把这次识别归到「{row.Nickname}」名下", "Success");
        }
        catch (Exception ex)
        {
            AppLog.Write($"ImportViewModel.ConfirmCandidate failed: {ex}");
            ShowHint("确认身份失败：" + ex.Message, "Error");
        }
    }

    /// <summary>Reject a candidate: drop the guess and ask the user to pick an owner.</summary>
    [RelayCommand]
    private async Task RejectCandidateAsync(IdentityCandidateRowViewModel? row)
    {
        if (row is null) return;
        Candidates.Remove(row);
        if (Candidates.Count == 0 && Result is not null)
        {
            IdentityStateText = "已排除全部候选，请手动指定归属";
            IdentityStateDetail = "这张截图自身没有身份信息";
            IdentityIsResolved = false;
            ShowOwnerPicker = true;
            await LoadOwnersAsync(Result);
        }
    }

    // ------------------------------------------------------------------
    // Commands — save
    // ------------------------------------------------------------------

    [RelayCommand]
    private async Task SaveAsync()
    {
        if (Result is null || IsBusy) return;

        if (!_backend.IsAvailable)
        {
            ShowHint(BackendUnavailableText, "Error");
            return;
        }

        // An unidentified capture (a 包裹 screenshot has no 游戏ID) is saved
        // against whichever character the user picked. Without a pick there is
        // nothing to attach to: saving would invent a "未命名角色" row that no
        // 人物属性 capture will ever match again.
        if (RecognitionService.NeedsOwner(Result))
        {
            if (SelectedOwner is null)
            {
                ShowHint(
                    Owners.Count == 0
                        ? "还没有角色可选：先在「角色管理」里新建，或先录入一张「人物属性」截图"
                        : "请先选择这次识别归属的角色",
                    "Warning");
                return;
            }
        }

        IsBusy = true;
        try
        {
            if (SelectedOwner is { } owner && string.IsNullOrEmpty(Result.CharacterId))
            {
                await _recognition.AttributeToAsync(Result, owner.Id);
                // Remember how this identity was decided, so the strip can say
                // "由你手动指定为「莫莫莫」" instead of a vague "已归属".
                _savedIdentityFrom = owner.Label.Split(' ')[0];
            }

            // 只把真正并进结果的截图记成来源：冲突时被排除的那张属于别人，
            // 写进 source_screens 会让快照看起来覆盖了一堆它没有的图。
            var screens = _batch.Count > 0
                ? RecognitionService.MergedScreenNames(Result, _batch.Select(i => i.Name).ToList()).ToList()
                : new List<string> { "character_attributes.png", "inventory.png" };

            var outcome = await _recognition.SaveAsync(Result, screens);
            if (outcome.IsPending)
            {
                // 不是失败：数据在，只是还没有主人。说清去哪儿认领。
                RefreshFromResult(keepFields: true);
                ShowHint(
                    $"这次的截图没有任何身份信息，已存入「待归属」（{outcome.Message}）。"
                    + "到「设置 → 数据」里指定角色即可，数据不会丢。",
                    "Warning");
                return;
            }
            if (!outcome.IsSaved)
            {
                ShowHint("保存失败：" + (outcome.Message ?? "后端没有返回角色 ID"), "Error");
                return;
            }

            RefreshFromResult(keepFields: false);
            ShowHint("已保存本次识别", "Success");

            var settings = await _backend.Api.GetSettingsAsync();
            if (settings.AutoNavigateAfterSave) _navigation.NavigateTo(AppPage.Characters);
        }
        catch (Exception ex)
        {
            AppLog.Write($"ImportViewModel.Save failed: {ex}");
            ShowHint("保存失败：" + ex.Message, "Error");
        }
        finally
        {
            IsBusy = false;
        }
    }

    // ------------------------------------------------------------------
    // Load
    // ------------------------------------------------------------------

    /// <summary>
    /// Demo-only: with no backend there are no canned images to send, and an
    /// empty payload is correctly rejected by the recogniser — so only the
    /// in-memory demo service can synthesise a result from nothing.
    ///
    /// <c>MH_DEMO_IMAGE</c> points at a real screenshot to show underneath the
    /// demo result, which is how the review workspace gets designed and
    /// screenshotted without a running recogniser.
    /// </summary>
    public async Task LoadDemoAsync()
    {
        // 只有调试构建的演示后端才合成结果；正式构建没有后端时是"不可用"，
        // 合成一张看起来正常的识别结果恰恰是最坏的行为。
        if (Result is not null || !_backend.IsDemo) return;

        var bytes = Array.Empty<byte>();
        var name = "demo-character.png";
        var path = Environment.GetEnvironmentVariable("MH_DEMO_IMAGE");
        if (!string.IsNullOrWhiteSpace(path) && File.Exists(path))
        {
            try
            {
                bytes = await File.ReadAllBytesAsync(path);
                name = Path.GetFileName(path);
            }
            catch (Exception ex)
            {
                AppLog.Write($"demo image load failed: {ex.Message}");
            }
        }

        var batch = new[] { new ImageFile(name, bytes, "演示") };
        var outcome = await _recognition.RecognizeAsync(batch);
        if (outcome.Result is { } result)
        {
            _batch = batch;
            ApplyResult(result, batch);
        }

        // The tab is already selected, so its change handler will not fire again.
        if (bytes.Length > 0)
        {
            PreviewBytes = bytes;
            PreviewCaption = $"{name} · 演示数据";
        }
    }

    private async Task<RecognitionResult?> RunRecognitionAsync(IReadOnlyList<ImageFile> batch, string source)
    {
        if (batch.Count == 0 || IsBusy) return null;

        if (!_backend.IsAvailable)
        {
            ShowHint(BackendUnavailableText, "Error");
            return null;
        }

        BusyText = "正在识别截图…";
        IsBusy = true;
        HintMessage = null;
        IsHintOpen = false;
        try
        {
            // Show what is about to be read *before* the OCR round: a screenshot
            // nobody can see is a screenshot nobody can trust.
            _batch = batch;
            ShowPreview(batch, source);

            var outcome = await _recognition.RecognizeAsync(batch);
            if (outcome.Result is null)
            {
                Result = null;
                Fields.Clear();
                Candidates.Clear();
                ShowHint(outcome.Message ?? "识别失败", "Warning");
                return null;
            }

            ApplyResult(outcome.Result, batch);
            return outcome.Result;
        }
        catch (Exception ex)
        {
            AppLog.Write($"ImportViewModel.RunRecognition failed: {ex}");
            Result = null;
            ShowHint("识别失败：" + ex.Message, "Error");
            return null;
        }
        finally
        {
            IsBusy = false;
        }
    }

    private void ShowPreview(IReadOnlyList<ImageFile> batch, string source)
    {
        ImageTabs.Clear();
        for (var i = 0; i < batch.Count; i++)
        {
            var kind = Result is not null && i < Result.Screens.Count ? Result.Screens[i] : ScreenKind.Unknown;
            ImageTabs.Add(new ImageTabViewModel(i, batch[i].Name, source, kind));
        }
        SelectedImageTab = ImageTabs.Count > 0 ? ImageTabs[0] : null;
        PreviewBytes = batch.Count > 0 && batch[0].Bytes.Length > 0 ? batch[0].Bytes : null;
        PreviewCaption = batch.Count switch
        {
            0 => string.Empty,
            1 => $"{source} · {batch[0].Name}",
            _ => $"{source} · 共 {batch.Count} 张",
        };
    }

    // ------------------------------------------------------------------
    // Result projection
    // ------------------------------------------------------------------

    private void ApplyResult(RecognitionResult result, IReadOnlyList<ImageFile> batch)
    {
        Result = result;
        if (batch.Count > 0) _batch = batch;

        // Screen tabs carry the classifier's verdict, which is only known now.
        if (_batch.Count > 0)
        {
            var keepIndex = SelectedImageTab?.Index ?? 0;
            ImageTabs.Clear();
            for (var i = 0; i < _batch.Count; i++)
            {
                var kind = i < result.Screens.Count ? result.Screens[i] : ScreenKind.Unknown;
                ImageTabs.Add(new ImageTabViewModel(i, _batch[i].Name, _batch[i].Source, kind));
            }
            SelectedImageTab = ImageTabs.FirstOrDefault(t => t.Index == keepIndex) ?? ImageTabs.FirstOrDefault();
        }
        else if (ImageTabs.Count > 0)
        {
            var rebuilt = ImageTabs
                .Select((tab, i) => new ImageTabViewModel(
                    tab.Index,
                    tab.Name,
                    tab.Source,
                    i < result.Screens.Count ? result.Screens[i] : ScreenKind.Unknown))
                .ToList();

            var keep = SelectedImageTab?.Index ?? 0;
            ImageTabs.Clear();
            foreach (var tab in rebuilt) ImageTabs.Add(tab);
            SelectedImageTab = ImageTabs.FirstOrDefault(t => t.Index == keep) ?? ImageTabs.FirstOrDefault();
        }
        else if (result.Screens.Count > 0)
        {
            for (var i = 0; i < result.Screens.Count; i++)
                ImageTabs.Add(new ImageTabViewModel(i, $"截图 {i + 1}", "识别", result.Screens[i]));
            SelectedImageTab = ImageTabs[0];
        }

        CacheNote = result.CacheHits.Count switch
        {
            0 => string.Empty,
            var n when n >= result.ScreenshotCount && result.ScreenshotCount > 0 =>
                $"这张截图上次已经分析过，本次直接复用了 OCR 结果（{n} 张命中缓存）",
            var n => $"{n}/{Math.Max(result.ScreenshotCount, n)} 张截图复用了上次的 OCR 结果",
        };

        RefreshFromResult(keepFields: false);
        _ = RefreshOwnersAsync(result);
    }

    /// <summary>Re-project the result onto the binding surface.</summary>
    private void RefreshFromResult(bool keepFields)
    {
        if (Result is not { } result)
        {
            return;
        }

        // "未识别角色" when nothing was resolved at all: calling it 未命名角色
        // implies we know it is someone, which for a bare 包裹 we do not.
        Nickname = result.Character.Nickname
                   ?? (string.IsNullOrEmpty(result.CharacterId) ? "未识别角色" : "未命名角色");
        AvatarInitial = Fmt.AvatarInitial(result.Character.Nickname);
        AvatarPngBase64 = result.AvatarPngBase64;
        LevelText = result.Character.Level is { } lvl ? $"Lv.{lvl}" : "Lv.?";

        var screens = result.Screens.Count > 0
            ? string.Join(" + ", result.Screens.Select(ImageTabViewModel.ScreenLabel))
            : "人物属性";
        var count = result.ScreenshotCount > 0 ? result.ScreenshotCount : _batch.Count;
        var idLine = Fmt.IdLine(result.Character.Level, result.Character.GameId);
        IdentityLine = string.IsNullOrEmpty(idLine) ? $"游戏 ID 待确认 · {screens}" : $"{idLine} · {screens}";
        SourceLine = $"来源：{screens} · {count} 张截图";

        ConfidenceRatio = Math.Clamp(result.Confidence, 0, 1);
        ConfidenceText = $"{result.Confidence * 100:0}%";
        IsConfidenceLow = result.Confidence > 0 && result.Confidence < 0.85;

        // 证据：为什么认定是这个角色 / 哪里互相矛盾 / 有哪些候选。
        var evidence = new List<string>();
        if (!string.IsNullOrWhiteSpace(result.IdentityEvidence)) evidence.Add(result.IdentityEvidence!);
        foreach (var conflict in result.Conflicts) evidence.Add("⚠ " + conflict);

        // 一批图里混了别人的截图时说清"这次只记了哪几张"，否则"2 张截图"
        // 会让人以为另一个人那张的数据也写进了这条记录。
        if (result.ExcludedScreens.Count > 0)
        {
            var kept = result.ScreenResults.Count(s => s.Included);
            evidence.Add(
                $"本次只合并了 {kept}/{result.ScreenResults.Count} 张截图；被排除的截图请单独导入");
        }
        else if (result.BatchIdentity == BatchIdentity.Unidentified)
        {
            evidence.Add("这批截图没有任何身份字段，保存前请确认归属");
        }
        if (result.Candidates.Count > 0 && string.IsNullOrEmpty(result.CharacterId))
            evidence.Add($"证据不足，有 {result.Candidates.Count} 个候选（见下方身份关联）");
        EvidenceText = string.Join(Environment.NewLine, evidence);

        AppearanceText = result.Appearance is { } look ? $"外观特征：{look.Describe()}" : string.Empty;

        if (!keepFields) RebuildFields(result);

        RebuildCandidates(result);
        UpdateIdentityState(result);
        RefreshRegions();

        OnPropertyChanged(nameof(HasResult));
        OnPropertyChanged(nameof(ShowEmptyResult));
        OnPropertyChanged(nameof(CanSave));
        OnPropertyChanged(nameof(HasFields));
        OnPropertyChanged(nameof(HasEvidence));
        OnPropertyChanged(nameof(HasAppearance));
        OnPropertyChanged(nameof(HasCandidates));
    }

    private void RebuildFields(RecognitionResult result)
    {
        var rows = new List<FieldRowViewModel>();
        var regions = result.Regions
            .GroupBy(r => r.Key)
            .ToDictionary(g => g.Key, g => g.First(), StringComparer.Ordinal);

        FieldRegion? RegionFor(string key) => regions.TryGetValue(key, out var r) ? r : null;

        void Add(string key, string label, AttributeValue? value) =>
            rows.Add(new FieldRowViewModel(key, label, value, RegionFor(key), ApplyFieldValue));

        var s = result.Stats;
        Add("hp", "气血", s.Hp);
        Add("mp", "魔法", s.Mp);
        Add("physical_damage", "物伤", s.PhysicalDamage);
        Add("magic_damage", "法伤", s.MagicDamage);
        Add("physical_defense", "物防", s.PhysicalDefense);
        Add("magic_defense", "法防", s.MagicDefense);
        Add("speed", "速度", s.Speed);
        Add("healing_power", "治疗强度", s.HealingPower);
        Add("gold", "金币", result.Currencies.Gold);
        Add("silver", "银币", result.Currencies.Silver);

        Fields.Clear();
        foreach (var row in rows) Fields.Add(row);

        // Land on the field most likely to be wrong: the whole point of the
        // review step is that the weakest link gets looked at first, instead of
        // the user re-checking eight numbers that were already fine.
        SelectedField = Fields
            .Where(f => !f.IsMissing)
            .OrderBy(f => f.Confidence)
            .FirstOrDefault();
    }

    /// <summary>Write an edited value back into the result the save will send.</summary>
    private void ApplyFieldValue(FieldRowViewModel row, long? value)
    {
        if (Result is not { } result) return;

        // A cleared field means "not visible", which is different from zero;
        // the whole model keeps that distinction, so honour it here too.
        AttributeValue? Build(AttributeValue? original) => value is null
            ? null
            : new AttributeValue
            {
                Value = value.Value,
                Confidence = original?.Confidence ?? 1.0,
                RawText = original?.RawText ?? string.Empty,
            };

        var s = result.Stats;
        switch (row.Key)
        {
            case "hp": s.Hp = Build(s.Hp); break;
            case "mp": s.Mp = Build(s.Mp); break;
            case "physical_damage": s.PhysicalDamage = Build(s.PhysicalDamage); break;
            case "magic_damage": s.MagicDamage = Build(s.MagicDamage); break;
            case "physical_defense": s.PhysicalDefense = Build(s.PhysicalDefense); break;
            case "magic_defense": s.MagicDefense = Build(s.MagicDefense); break;
            case "speed": s.Speed = Build(s.Speed); break;
            case "healing_power": s.HealingPower = Build(s.HealingPower); break;
            case "gold": result.Currencies.Gold = Build(result.Currencies.Gold); break;
            case "silver": result.Currencies.Silver = Build(result.Currencies.Silver); break;
        }
    }

    private void RebuildCandidates(RecognitionResult result)
    {
        Candidates.Clear();
        if (string.IsNullOrEmpty(result.CharacterId))
        {
            for (var i = 0; i < result.Candidates.Count; i++)
            {
                Candidates.Add(new IdentityCandidateRowViewModel(
                    result.Candidates[i],
                    i == 0,
                    row => _ = ConfirmCandidateAsync(row),
                    row => _ = RejectCandidateAsync(row)));
            }
        }
        OnPropertyChanged(nameof(HasCandidates));
        OnPropertyChanged(nameof(IdentityNeedsAttention));
    }

    private void UpdateIdentityState(RecognitionResult result)
    {
        ShowOwnerPicker = false;

        if (!string.IsNullOrEmpty(result.CharacterId))
        {
            IdentityIsResolved = true;
            IdentityStateText = $"已归属「{Nickname}」";
            IdentityStateDetail = result.IdentitySource switch
            {
                IdentitySource.IdField => $"按游戏ID {result.Character.GameId} 直接确定",
                IdentitySource.Manual => _savedIdentityFrom is { } name
                    ? $"由你手动指定为「{name}」"
                    : "由你手动指定",
                IdentitySource.Nickname => "昵称唯一匹配到库里的角色",
                IdentitySource.VisualMatch => result.IdentityEvidence ?? "按画面与面板证据匹配",
                IdentitySource.Ambiguous => "多个角色同样吻合，需要你确认",
                _ => string.Empty,
            };
            return;
        }

        if (Candidates.Count > 0)
        {
            IdentityIsResolved = false;
            IdentityStateText = $"身份待确认 · {Candidates.Count} 个候选";
            IdentityStateDetail = "证据不足以定论，请确认一个或排除后手动指定";
            return;
        }

        IdentityIsResolved = false;
        IdentityStateText = "这张截图没有身份信息";
        IdentityStateDetail = "包裹面板不显示游戏ID，选一个已入库角色作为归属";
        ShowOwnerPicker = true;
    }

    private async Task RefreshOwnersAsync(RecognitionResult result)
    {
        if (string.IsNullOrEmpty(result.CharacterId) && Candidates.Count == 0)
        {
            // The picker is only for captures that carry no identity of their own.
            var selfIdentified = !string.IsNullOrEmpty(result.Character.GameId)
                                 || !string.IsNullOrEmpty(result.Character.Nickname);
            if (!selfIdentified) await LoadOwnersAsync(result);
        }
        else if (RecognitionService.NeedsOwner(result))
        {
            await LoadOwnersAsync(result);
        }
    }

    private async Task LoadOwnersAsync(RecognitionResult result)
    {
        try
        {
            var characters = await _backend.Api.ListCharactersAsync();
            Owners.Clear();
            foreach (var c in characters)
            {
                var level = c.Character.Level is { } lvl ? $" Lv.{lvl}" : string.Empty;
                var gameId = string.IsNullOrWhiteSpace(c.Character.GameId) ? string.Empty : $" · {c.Character.GameId}";
                Owners.Add(new OwnerOption(c.Character.Id, $"{c.Character.Nickname}{level}{gameId}"));
            }

            // Unambiguous when there is exactly one character to choose.
            SelectedOwner = Owners.Count == 1 ? Owners[0] : null;
            ShowOwnerPicker = true;
            OwnerHint = Owners.Count == 0
                ? "库里还没有角色：先在「角色管理」里新建，或先录入一张「人物属性」截图"
                : result.Appearance is { } look
                    ? $"会记住外观特征：{look.Describe()}（发色与武器的画面特征用于以后自动关联）"
                    : "这张截图里没有读到角色模型，无法记录外观特征；本次仍会记住面板画面，物品变化后可能认不出。";
        }
        catch (Exception ex)
        {
            AppLog.Write($"ImportViewModel.LoadOwners failed: {ex.Message}");
        }
    }

    private void RefreshRegions()
    {
        Regions.Clear();
        if (Result is not { } result) return;
        var index = SelectedImageTab?.Index ?? 0;
        foreach (var region in result.Regions.Where(r => r.ScreenIndex == index))
            Regions.Add(region);
    }

    private void ShowHint(string message, string severity)
    {
        HintSeverity = severity;
        HintMessage = message;
        IsHintOpen = !string.IsNullOrWhiteSpace(message);
    }
}
