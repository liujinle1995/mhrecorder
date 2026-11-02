using System.Collections.Specialized;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media.Imaging;
using MhRecorder.App.Services;
using MhRecorder.Core.Models;
using MhRecorder.Core.ViewModels;
using Windows.Storage.Streams;
using Windows.System;

namespace MhRecorder.App.Views;

/// <summary>
/// Page 1 — 录入识别 + 审核工作区。
///
/// 这里只做三件"必须由视图做"的事：把 ViewModel 的状态画到控件上、把用户
/// 手势翻译成命令、以及解码图片。业务规则（能不能保存、改过的值怎么入库、
/// 身份不明要不要拦）全在 <see cref="ImportViewModel"/> 里。
/// </summary>
public sealed partial class ImportPage : Page
{
    public ImportViewModel ViewModel { get; } = AppHost.Import;

    private string? _lastHighlight;
    private byte[]? _loadedBytes;
    private bool _syncing;
    private bool _pendingSync;

    public ImportPage()
    {
        InitializeComponent();

        // DataContext so the row template's classic two-way Binding resolves.
        DataContext = ViewModel;

        Loaded += OnLoaded;
        Unloaded += OnUnloaded;

        ViewModel.PropertyChanged += OnViewModelChanged;
        ViewModel.Fields.CollectionChanged += OnFieldsChanged;
        ViewModel.Regions.CollectionChanged += OnRegionsChanged;

        Viewer.RegionClicked += OnRegionClicked;

        // The button says Ctrl+V, so the shortcut has to work too. Scoped to the
        // page: it only fires while something on this page has focus.
        var paste = new KeyboardAccelerator { Key = VirtualKey.V, Modifiers = VirtualKeyModifiers.Control };
        paste.Invoked += (_, args) =>
        {
            args.Handled = true;
            if (ViewModel.PasteCommand.CanExecute(null)) ViewModel.PasteCommand.Execute(null);
        };
        KeyboardAccelerators.Add(paste);
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            ApplyAvatar();
            await ViewModel.LoadDemoAsync();
            await RefreshViewerAsync();
        }
        catch (Exception ex)
        {
            App.Log($"ImportPage.OnLoaded failed: {ex}");
        }
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        ViewModel.PropertyChanged -= OnViewModelChanged;
        ViewModel.Fields.CollectionChanged -= OnFieldsChanged;
        ViewModel.Regions.CollectionChanged -= OnRegionsChanged;
        Viewer.RegionClicked -= OnRegionClicked;
    }

    private void OnViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(ImportViewModel.PreviewBytes):
                _ = RefreshViewerAsync();
                break;
            case nameof(ImportViewModel.AvatarPngBase64):
                ApplyAvatar();
                break;
            case nameof(ImportViewModel.HighlightedKey):
                ApplyHighlight();
                break;
        }
    }

    private void OnFieldsChanged(object? sender, NotifyCollectionChangedEventArgs e) => ApplyHighlight();

    private void OnRegionsChanged(object? sender, NotifyCollectionChangedEventArgs e) => _ = RefreshViewerAsync();

    /// <summary>
    /// Feed the capture and its boxes into the viewer.
    ///
    /// Image and boxes arrive as separate notifications (the VM fills regions
    /// before the bytes are decoded), so both funnel through here and the last
    /// one wins; decoding happens in the view because it produces a bitmap.
    /// </summary>
    private async Task RefreshViewerAsync()
    {
        if (_syncing)
        {
            _pendingSync = true;
            return;
        }

        _syncing = true;
        try
        {
            do
            {
                _pendingSync = false;

                var bytes = ViewModel.PreviewBytes;
                if (!ReferenceEquals(bytes, _loadedBytes))
                {
                    _loadedBytes = bytes;
                    await Viewer.SetImageAsync(bytes);
                }

                Viewer.SetRegions(ViewModel.Regions, ViewModel.HighlightedKey);
                _lastHighlight = ViewModel.HighlightedKey;

                // The selected field is the one worth looking at; zoom to it so
                // the value under the box is actually readable.
                if (ViewModel.HighlightedKey is { } key) Viewer.Highlight(key);
            }
            while (_pendingSync);
        }
        catch (Exception ex)
        {
            App.Log($"ImportPage.RefreshViewer failed: {ex.Message}");
        }
        finally
        {
            _syncing = false;
        }
    }

    private void ApplyHighlight()
    {
        var key = ViewModel.HighlightedKey;
        if (key == _lastHighlight) return;
        _lastHighlight = key;
        Viewer.Highlight(key);
    }

    private void ApplyAvatar()
    {
        var base64 = ViewModel.AvatarPngBase64;
        AvatarImage.Source = null;
        AvatarImage.Visibility = Visibility.Collapsed;
        AvatarInitialText.Text = ViewModel.AvatarInitial;
        AvatarInitialText.Visibility = Visibility.Visible;
        if (string.IsNullOrEmpty(base64)) return;

        try
        {
            // A data: URI is enough for a PNG and avoids an async round-trip here.
            AvatarImage.Source = new BitmapImage(new Uri("data:image/png;base64," + base64));
            AvatarImage.Visibility = Visibility.Visible;
            AvatarInitialText.Visibility = Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            App.Log($"avatar decode failed: {ex.Message}");
        }
    }

    /// <summary>Clicking a box on the capture selects the row that produced it.</summary>
    private void OnRegionClicked(object? sender, FieldRegion region)
    {
        var row = ViewModel.Fields.FirstOrDefault(f => f.Key == region.Key);
        if (row is null)
        {
            ViewModel.HighlightedKey = region.Key;
            return;
        }

        ViewModel.SelectedField = row;
        FieldList.ScrollIntoView(row);
    }

    private void OnImageTabClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: ImageTabViewModel tab }) ViewModel.SelectedImageTab = tab;
    }

    private void OnFitClick(object sender, RoutedEventArgs e) => Viewer.ResetZoom();

    // ---------------- drag & drop ----------------

    private void OnDropZoneDragOver(object sender, DragEventArgs e)
    {
        e.AcceptedOperation = Windows.ApplicationModel.DataTransfer.DataPackageOperation.Copy;
    }

    private void OnDropZoneDragLeave(object sender, DragEventArgs e)
    {
    }

    private async void OnDropZoneDrop(object sender, DragEventArgs e)
    {
        try
        {
            if (!e.DataView.Contains(Windows.ApplicationModel.DataTransfer.StandardDataFormats.StorageItems)) return;
            var items = await e.DataView.GetStorageItemsAsync();
            var paths = items.OfType<Windows.Storage.StorageFile>()
                .Where(f => Core.Services.ImageImportService.SupportedExtensions.Contains(
                    f.FileType, StringComparer.OrdinalIgnoreCase))
                .Select(f => f.Path)
                .ToList();
            if (paths.Count == 0) return;

            // Dropping files is the same pipeline as picking them; the only
            // difference is where the paths came from.
            await ViewModel.ImportPathsAsync(paths);
        }
        catch (Exception ex)
        {
            App.Log($"OnDropZoneDrop failed: {ex}");
        }
    }
}
