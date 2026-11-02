// 组合根：唯一一处 new 出所有服务与 ViewModel 的地方。
//
// 之前是 AppServices 一个静态类，页面直接 `AppServices.Instance` 拿后端；现在
// 依赖从这里往下传，页面只说"给我 ImportViewModel"。这样 ViewModel 的构造
// 依赖是显式的，也能被单独 new 出来测试。

using MhRecorder.App.Platform;
using MhRecorder.Core.Services;
using MhRecorder.Core.ViewModels;

namespace MhRecorder.App.Services;

public static class AppHost
{
    private static bool _initialized;

    public static BackendService Backend { get; private set; } = null!;
    public static MainViewModel Shell { get; private set; } = null!;
    public static ImportViewModel Import { get; private set; } = null!;
    public static CharactersViewModel Characters { get; private set; } = null!;
    public static AssetsViewModel Assets { get; private set; } = null!;
    public static SettingsViewModel Settings { get; private set; } = null!;

    public static bool IsRemote => Backend.IsRemote;

    public static void Initialize()
    {
        if (_initialized) return;

        AppLog.Attach(App.Log);

        // One dialogs object: it is both the question-asker and the window
        // picker, because both are "show a ContentDialog and report the answer".
        var dialogs = new AppDialogs();

        Backend = new BackendService();
        Shell = new MainViewModel(Backend);

        var navigation = new ShellNavigationService(Shell);
        var images = new ImageImportService(new WindowsImageSource());
        var recognition = new RecognitionService(Backend);
        var characters = new CharacterService(Backend);
        var exports = new ExportService(Backend, new WindowsFileTarget());

        Import = new ImportViewModel(Backend, images, recognition, dialogs, navigation, dialogs);
        Characters = new CharactersViewModel(characters, dialogs, navigation);
        Assets = new AssetsViewModel(Backend);
        Settings = new SettingsViewModel(Backend, exports, dialogs);

        _initialized = true;
        App.Log($"AppHost: ready (remote={IsRemote})");
    }
}
