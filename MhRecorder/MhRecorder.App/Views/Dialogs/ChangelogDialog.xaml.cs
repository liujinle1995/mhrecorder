using Microsoft.UI.Xaml.Controls;
using MhRecorder.Core;
using MhRecorder.Core.Services;

namespace MhRecorder.App.Views.Dialogs;

/// <summary>
/// In-app release notes.
///
/// Kept as a modal rather than an inline card: four releases of history pushed
/// the rest of the settings page off-screen and made it read like a document
/// instead of a settings list.
/// </summary>
public sealed partial class ChangelogDialog : ContentDialog
{
    public ChangelogDialog()
    {
        InitializeComponent();
        Resources["ContentDialogMaxWidth"] = 560.0;
        Resources["ContentDialogMinWidth"] = 520.0;
        Resources["ContentDialogMaxHeight"] = 640.0;
    }

    public ChangelogEntry[] Entries => Changelog.Entries;

    public string CurrentBuildLine =>
        $"当前版本 {ProtocolConstants.AppVersion}（本地离线版）· 已是最新";
}
