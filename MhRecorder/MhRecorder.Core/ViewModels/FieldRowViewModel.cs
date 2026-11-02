// 审核表格里的一行：一个识别字段 + 它的证据 + 人工修改。
//
// 这一行是"人工修正"真正落地的地方：改文本框就是改数据，改完立刻标成"已修改"，
// 保存时用的是改后的值。以前这一步根本不存在——页面只把数字显示成 TextBlock，
// 想改只能重拍一张。

using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using MhRecorder.Core.Models;

namespace MhRecorder.Core.ViewModels;

public sealed partial class FieldRowViewModel : ObservableObject
{
    private readonly Action<FieldRowViewModel, long?> _apply;

    public FieldRowViewModel(
        string key,
        string label,
        AttributeValue? value,
        FieldRegion? region,
        Action<FieldRowViewModel, long?> apply)
    {
        Key = key;
        Label = label;
        Original = value?.Value;
        RawText = value?.RawText ?? string.Empty;
        Confidence = value?.Confidence ?? 0.0;
        Region = region;
        _apply = apply;
        _valueText = value is null ? "—" : Fmt.Number(value.Value);
    }

    public string Key { get; }
    public string Label { get; }

    /// <summary>Value as first recognised; 还原 goes back to this.</summary>
    public long? Original { get; }

    /// <summary>Raw OCR text that produced the value — the "为什么会读错" clue.</summary>
    public string RawText { get; }

    public double Confidence { get; }

    /// <summary>The OCR box this value came from, when the backend located one.</summary>
    public FieldRegion? Region { get; }

    public string ConfidenceText => Confidence <= 0 ? "—" : $"{Confidence:P0}";

    public bool IsLowConfidence => Confidence > 0 && Confidence < 0.85;

    public bool IsMissing => Original is null;

    /// <summary>0..1 width for the little confidence meter.</summary>
    public double ConfidenceRatio => Math.Clamp(Confidence, 0, 1);

    public string StatusText => IsEdited ? "已修改" : IsMissing ? "未读到" : IsLowConfidence ? "待复核" : "正常";

    /// <summary>Editable text. Parse failures keep the old value instead of zeroing it.</summary>
    [ObservableProperty]
    private string _valueText;

    [ObservableProperty]
    private bool _isEdited;

    /// <summary>Raised when the user edits, so the page can flash the matching box.</summary>
    public event EventHandler? Edited;

    partial void OnValueTextChanged(string value)
    {
        var parsed = ParseNumber(value);
        if (parsed is null)
        {
            // "—" or garbage: nothing to write, and not an edit either.
            IsEdited = false;
            return;
        }

        IsEdited = parsed != Original;
        _apply(this, parsed);
        Edited?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>Drop the edit and go back to what the recogniser said.</summary>
    [RelayCommand]
    public void Revert()
    {
        ValueText = Original is { } v ? Fmt.Number(v) : "—";
        IsEdited = false;
        _apply(this, Original);
    }

    /// <summary>
    /// "7,721 / 7,721" → 7721, "1，714" → 1714, "—" → null.
    ///
    /// Kept deliberately forgiving: the panel prints full-width commas and the
    /// 气血/魔法 rows read as "current / max", and a user fixing a misread digit
    /// should not have to reproduce either.
    /// </summary>
    public static long? ParseNumber(string? text)
    {
        if (string.IsNullOrWhiteSpace(text)) return null;
        var head = text.Split('/', '／')[0];
        var digits = new string(head.Where(char.IsAsciiDigit).ToArray());
        if (digits.Length == 0) return null;
        return long.TryParse(digits, out var value) ? value : null;
    }
}
