using Microsoft.UI.Xaml.Controls;
using MhRecorder.Core.Services;

namespace MhRecorder.App.Views.Dialogs;

/// <summary>Pick which application window to screenshot.</summary>
public sealed partial class WindowPickerDialog : ContentDialog
{
    private readonly IReadOnlyList<WindowTarget> _windows;

    public WindowPickerDialog(IReadOnlyList<WindowTarget> windows)
    {
        InitializeComponent();
        _windows = windows;

        WindowList.ItemsSource = windows;
        WindowList.SelectedIndex = Math.Max(0, windows.ToList().FindIndex(w => w.LooksLikeGame));
        IsPrimaryButtonEnabled = WindowList.SelectedItem is not null;

        // Dialog sizing lives in template ThemeResources; overriding them on the
        // instance is the only supported way to widen a ContentDialog.
        Resources["ContentDialogMaxWidth"] = 580.0;
        Resources["ContentDialogMinWidth"] = 540.0;
        Resources["ContentDialogMaxHeight"] = 700.0;
    }

    public WindowTarget? Selected => WindowList.SelectedItem as WindowTarget;

    private void OnSelectionChanged(object sender, SelectionChangedEventArgs e) =>
        IsPrimaryButtonEnabled = WindowList.SelectedItem is not null;
}
