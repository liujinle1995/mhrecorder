; ============================================================================
;  mhrecorder.iss — Inno Setup 6 安装包脚本
;
;  由 scripts\package_installer.ps1 调用（它会传 /DAppVersion=<版本号>）：
;      ISCC.exe /DAppVersion=0.15.0 packaging\mhrecorder.iss
;
;  设计取舍（和"绿色版 zip"的区别）：
;    * 按用户安装（PrivilegesRequired=lowest）→ {autopf} 落到
;      %LOCALAPPDATA%\Programs\MhRecorder，不弹 UAC、不写 Program Files、
;      不碰系统目录，普通用户/公司受控机器都能装。
;    * 安装完直接启动 MhRecorder.exe 就行：界面会自己拉起同目录的
;      mhservice.exe。**不要**把 run.ps1 搬进来——装完还要求用户右键
;      "用 PowerShell 运行"等于没安装。
;    * 卸载默认保留数据库（%LOCALAPPDATA%\MhRecorder）：卸载程序顺手删用户
;      记录是最容易招骂的行为。要删得由用户在卸载时明确勾选。
; ============================================================================

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif

#define ProjectName "MhRecorder"
#define AppDisplayName "MhRecorder（梦幻西游截图记录助手）"
#define RepoRoot AddBackslash(SourcePath + "..")
#define DistDir AddBackslash(RepoRoot + "dist\MhRecorder")
#define OutputDir AddBackslash(RepoRoot + "dist")
#define DataDirName "MhRecorder"

#if !DirExists(DistDir)
  #error 找不到 dist\MhRecorder，先运行 scripts\package.ps1
#endif

[Setup]
; AppId 一旦发布就不能改，否则升级会变成"又装了一个"。
AppId={{8F3A5C21-6D74-4E1B-9C8F-2B4A7D5E9C13}
AppName={#AppDisplayName}
AppVersion={#AppVersion}
AppVerName={#ProjectName} {#AppVersion}
AppPublisher={#ProjectName}
VersionInfoVersion={#AppVersion}.0
VersionInfoDescription={#ProjectName} 安装程序
VersionInfoProductName={#ProjectName}
VersionInfoProductVersion={#AppVersion}.0

DefaultDirName={autopf}\{#ProjectName}
DefaultGroupName={#ProjectName}
DisableProgramGroupPage=yes
AllowNoIcons=yes

; 按用户安装：{autopf} = %LOCALAPPDATA%\Programs，不触发 UAC。
; 允许在命令行用 /ALLUSERS 显式改成全机安装（公司统一部署时有需求）。
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763

OutputDir={#OutputDir}
OutputBaseFilename={#ProjectName}-{#AppVersion}-setup
SetupIconFile={#RepoRoot}\packaging\mhrecorder.ico
UninstallDisplayIcon={app}\{#ProjectName}.exe
UninstallDisplayName={#AppDisplayName}

Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupLogging=yes

; 覆盖安装时把正在跑的界面/后端关掉：它们锁着 onnxruntime.dll 和 opencv。
; Restart Manager 只认"占着要替换的文件"的进程，后端是无窗口的，所以
; [Code] 里另外 taskkill 一次（见 StopBackend）。
CloseApplications=yes
RestartApplications=no

[Languages]
; 用仓库里自带的简体中文语言文件，而不是 Inno 安装目录里的：官方安装包根本没
; 带中文（只有 Languages\ 下那 29 种），引用 compiler: 路径会直接编译失败；
; 自带一份也让语言文件的版本随仓库固定。
Name: "chinese"; MessagesFile: "{#RepoRoot}\packaging\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务："; Flags: unchecked

[Files]
; 整个 dist 目录搬进来（含 models\ 与两个原生 DLL），只排除绿色版专用的启动器
; 和它的说明——装完后那些文字反而会误导用户。
Source: "{#DistDir}\*"; DestDir: "{app}"; \
    Excludes: "run.ps1,README.txt"; \
    Flags: ignoreversion recursesubdirs createallsubdirs

; 安装版自己的说明（覆盖绿色版的那份，用 isreadme 让装完自动弹出）
Source: "{#RepoRoot}\packaging\使用说明.txt"; DestDir: "{app}"; DestName: "使用说明.txt"; \
    Flags: ignoreversion isreadme

[Icons]
Name: "{group}\{#ProjectName}"; Filename: "{app}\{#ProjectName}.exe"
Name: "{group}\使用说明"; Filename: "{app}\使用说明.txt"
Name: "{group}\卸载 {#ProjectName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#ProjectName}"; Filename: "{app}\{#ProjectName}.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\{#ProjectName}.exe"; Description: "立即启动 {#ProjectName}"; \
    Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{app}\*.log"

[Code]
var
  RemoveUserData: Boolean;

{ 后端是无窗口进程，Restart Manager 不一定认它，显式收一次。
  失败（进程本来就没跑）不算错误，taskkill 返回非 0 直接忽略。 }
procedure StopBackend();
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM mhservice.exe',
       '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
    StopBackend();
end;

function InitializeUninstall(): Boolean;
var
  DataDir: string;
begin
  Result := True;
  RemoveUserData := False;
  DataDir := ExpandConstant('{localappdata}\{#DataDirName}');
  if not DirExists(DataDir) then
    exit;

  { 静默卸载（/VERYSILENT，公司批量部署会用）里弹 MsgBox 会一直挂着等人点，
    等于卸载卡死。这种情况一律按"保留数据"处理。 }
  if UninstallSilent then
    exit;

  if MsgBox('是否同时删除本地数据？' + #13#10 + #13#10
            + '数据库位置：' + DataDir + #13#10
            + '里面是全部角色、快照、装备、背包记录。' + #13#10 + #13#10
            + '选择"否"只卸载程序，数据保留（重新安装后还能看到）。',
            mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    RemoveUserData := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    StopBackend();

  if (CurUninstallStep = usPostUninstall) and RemoveUserData then
    DelTree(ExpandConstant('{localappdata}\{#DataDirName}'), True, True, True);
end;
