// 对话框与平台能力的抽象。
//
// ViewModel 里"删角色要确认一下"这类需求，以前是页面自己 new ContentDialog、
// 自己 await ShowAsync——于是业务判断和 UI 事件循环绑在一起，页面无法测试。
// 现在只有一句 await _dialogs.ConfirmAsync(...)，具体怎么弹由 App 决定。

namespace MhRecorder.Core.Infrastructure;

using MhRecorder.Core.Services;

/// <summary>Modal questions and alerts, implemented on top of <c>ContentDialog</c>.</summary>
public interface IDialogService
{
    /// <summary>Two-button confirm. Returns true only when the user chose the primary action.</summary>
    Task<bool> ConfirmAsync(string title, string message, string primaryText, string closeText = "取消");

    /// <summary>Single-button notice.</summary>
    Task AlertAsync(string title, string message);
}

/// <summary>Pick which application window to screenshot. Null when the user cancels.</summary>
public interface IWindowPicker
{
    Task<WindowTarget?> PickAsync(IReadOnlyList<WindowTarget> windows);
}

/// <summary>Answers "yes" to everything and prints nothing — for unit tests.</summary>
public sealed class AutoConfirmDialogService : IDialogService
{
    public Task<bool> ConfirmAsync(string title, string message, string primaryText, string closeText = "取消") =>
        Task.FromResult(true);

    public Task AlertAsync(string title, string message) => Task.CompletedTask;
}
