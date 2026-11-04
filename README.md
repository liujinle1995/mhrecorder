# 梦幻手游记录助手 (mhrecorder)

Windows 原生桌面应用，用于自动识别《梦幻西游手游》截图中的角色信息、属性、装备、背包物品与货币，
并本地归档、统计资产变化。

**全部离线运行**：不联网、不上传、不使用 GPU、不使用视觉语言模型。

---

## 架构

```
┌─────────────────────────────────────────────┐
│  MhRecorder.App   (WinUI 3 / C# / .NET 10)  │
│  录入识别 · 角色管理 · 资产统计 · 设置        │
└──────────────────┬──────────────────────────┘
                   │  Windows Named Pipe  \\.\pipe\mhrecorder.v1
                   │  行分隔 JSON-RPC（camelCase）
┌──────────────────▼──────────────────────────┐
│  mhservice.exe    (Rust)                     │
│   ├── mhvision    OpenCV + ONNX Runtime      │
│   │               OCR / 分类 / 特征 / 关联    │
│   └── mhstore     SQLite (rusqlite bundled)  │
└─────────────────────────────────────────────┘
```

| 组件 | 技术 |
|---|---|
| 前端 | WinUI 3 + C# + .NET 10，非打包 / 自包含 |
| 后端 | Rust 2021 |
| 数据库 | SQLite（WAL 模式，内嵌编译） |
| 图像处理 | OpenCV 4.12（`opencv_world4120`，vendor） |
| OCR | RapidOCR / PP-OCRv4 ONNX，ONNX Runtime 1.31 CPU 推理 |
| 特征识别 | dHash（9×8 差分哈希）+ ORB（双向匹配 + RANSAC）+ HSV（色相/饱和/亮度）|
| 通信 | Windows 命名管道，行分隔 JSON |

### Rust 后端的模块划分

`mhvision` 以前是一个 4000 行的 `lib.rs`，换个阈值要在 OpenCV 调用和 OCR
会话管理之间来回翻。现在按"改动频率"拆开：

| 模块 | 职责 |
|---|---|
| `types` | 错误类型、任务控制口、OCR 行 |
| `imgops` | 解码、BGR Mat 转换 |
| `features` | dHash / ORB（双向 + RANSAC）/ HSV 直方图 |
| `classify` | 界面分类、面板定位（Otsu + 投影） |
| `thumbnail` | 32×32 面板缩略图相似度 |
| `identity` | 多证据加权、冲突保护、昵称歧义 |
| `appearance` | 发色 / 武器特征 |
| `ocr` | PP-OCRv4 检测 + 识别 |
| `extract` | 从 OCR 行里抠字段（属性、货币、背包……） |
| `pipeline` | 串成一次调用，并在图间检查取消 / 超时 |

`mhstore` 同样按仓储拆：`characters` / `snapshots` / `identity` / `items` /
`settings` / `cache` / `export` / `maintenance` / `migrations`。

## 目录

```
mhrecorder/
├─ src/                    Rust workspace
│  └─ crates/
│     ├─ mhcore/           领域模型 + 管道协议（前后端契约）
│     ├─ mhvision/         图像流水线，按职责分 10 个模块（见下）
│     ├─ mhstore/          SQLite 持久化：仓储式拆分 + migrations/*.sql
│     ├─ mhservice/        管道服务端：分发 / 边界 / 任务生命周期
│     └─ mhcli/            独立测试 CLI（无需 UI）
├─ MhRecorder/             C# 解决方案
│  ├─ MhRecorder.Core/     模型 / 管道客户端 / ViewModel
│  └─ MhRecorder.App/      WinUI 3 界面
├─ native/                 随包原生依赖（不提交改动）
│  ├─ opencv/              include + lib + dll
│  ├─ onnxruntime/         headers + lib + dll
│  └─ models/              PP-OCRv4 det/rec onnx + 字典
├─ scripts/
│  ├─ env.ps1              构建环境（cargo 前必须 source）
│  ├─ make_samples.py      生成合成测试截图
│  ├─ make_icon.ps1        生成安装包图标（packaging/mhrecorder.ico）
│  ├─ test_pipe_e2e.ps1    管道端到端测试
│  ├─ test_async_tasks.ps1 协议级验证识别可取消（start / get / cancel / shutdown）
│  ├─ test_backend_lifetime.ps1 后端实例锁、空闲自退、多数据目录并存
│  ├─ test_two_windows.ps1 两个窗口共用一个后端，互不影响
│  ├─ test_ui_cancel.ps1   界面级验证「取消」按钮真的会停
│  ├─ test_ocr_profile.ps1 设置档位 → 引擎就地重建 → 重启后保持（21 项）
│  ├─ verify_vision.ps1    OCR / 特征回归测试
│  ├─ verify_real_captures.ps1  真机截图验收（37 项，数值逐项对照真图）
│  ├─ package.ps1          打包 dist/（免安装目录版）
│  ├─ package_single.ps1   打包单文件 exe（自解压）
│  ├─ package_installer.ps1 用 Inno Setup 打标准安装包（setup.exe）
│  ├─ version.ps1          版本号与产物命名的唯一来源（读 Protocol.cs）
│  └─ release.ps1          把 dist 产物发布到 GitHub Releases
├─ packaging/
│  ├─ mhrecorder.iss       Inno Setup 安装脚本（向导 / 快捷方式 / 卸载）
│  ├─ ChineseSimplified.isl 安装向导的简体中文语言文件
│  ├─ mhrecorder.ico       安装包与快捷方式图标
│  └─ 使用说明.txt          安装版随包的说明（装完自动打开）
├─ samples/                合成测试截图
├─ docs/ARCHITECTURE.md    详细架构与协议说明
└─ design/                 设计稿导出的页面图
```

## 构建

前置：.NET SDK 10、Rust stable、LLVM（提供 clang）、Windows 10 1809+。

```powershell
# 1) 环境变量（关键，见下节“两个必踩的坑”）
. .\scripts\env.ps1

# 2) 后端
cd src
cargo build --release
cargo test -p mhstore -p mhvision

# 3) 前端
cd ..\MhRecorder
dotnet build -c Release

# 4) 打包成免安装目录
cd ..
.\scripts\package.ps1
```

## 界面行为（与设计稿对齐）

窗口默认按设计稿尺寸 **1328×888 逻辑单位**打开（比例 1.495，取自
`design/page_01.png` 实测 2766×1850 px @2.0836 px/pt），并按显示器 DPI 换算成物理像素。
允许拖拽缩放与最大化：宽度跨过断点时侧边栏在 254 / 76 两档之间切换
（窄屏收成图标条），内容区随之重排；小屏与高 DPI 下按工作区可用尺寸收敛，
不会把窗口开到屏幕外。

角色管理卡片的排布规则：

| 角色数 | 行为 |
|---|---|
| 1–2 | 平铺、水平居中，不显示轮播箭头 |
| ≥3 | 轮播；**第二张卡片（居中卡片右侧那张）置于 Z 轴上层** |

头像：从人物属性截图自动裁切（以游戏ID文本为锚点反推方形框），
以 base64 PNG 存入数据库的 `avatar_path` 列，卡片与识别结果都显示真实头像。

## 识别引擎 = 插件

识别器被建模为可插拔单元，设置页显示实际使用的插件信息：

```
识别引擎（插件）  PP-OCRv4 (ONNX Runtime) · 已加载
插件明细          ppocrv4-onnx
                  运行时：onnxruntime-cpu
                  模型：ch_PP-OCRv4_det_infer.onnx、ch_PP-OCRv4_rec_infer.onnx
识别引擎版本      4.x
```

`version` 协议返回 `engine` 对象（`id` / `name` / `version` / `runtime` / `models` /
`active` / `detail` / `updatable`）。当前构建 `updatable=false`：本项目坚持离线优先，
**不做在线插件下载**；「检查更新」会如实报告这一点，而不是假装有更新。

设置页还内置「版本更新记录」，列出每个版本对识别与界面的行为变更。

## 识别性能与机器负载

识别是 CPU 上的 ONNX 推理。默认参数偏"低资源"，因为这是交互路径：点一下等
一两秒，把 12 个逻辑核占满只换来几百毫秒，代价是整机（尤其同时开着模拟器、
编译、会议）变卡。

| 档位 | 线程 | 批大小 | 适用 |
|---|---|---|---|
| 低资源 | 2 | 2 | 同时跑别的重活，或笔记本省电 |
| 平衡（默认） | 4 | 4 | 一般使用 |
| 高性能 | 物理核数估算（逻辑核数 / 2，4–8） | 4 | 只跑本程序 |

设置页把**实际生效**的参数直接显示在选项下面（例如「平衡 · 4 线程 · 批 4 ·
DB 轮廓后处理 · 12 逻辑核」）。切换后引擎就地重建，不用重启。

为什么不默认"用满核心"：在本机（i5-10600，6 核 12 线程）实测同一张 1265×711
截图，4 线程 1504 ms，运行时默认的 12 线程 1858 ms，16 线程 3201 ms——多出来
的线程只是在抢内存带宽。

文本框后处理有两套实现（旧的四邻域矩形 / 标准 DB 轮廓），默认用后者，因为
真实截图上它更快、置信度更高、碎片行更少：

```powershell
. .\scripts\env.ps1
.\src\target\release\mhcli.exe ocr-bench --det both --batch 4 --threads 4 `
    samples\real_character_attributes.png samples\real_inventory.png
```

排障时可临时覆盖参数：`MH_OCR_DET`、`MH_OCR_BATCH`、`MH_OCR_THREADS`、
`MH_OCR_MAX_SIDE`、`MH_OCR_PROFILE`（详见 `docs/ARCHITECTURE.md`）。

## 从源码构建的常见坑（均已在脚本中处理）

1. **`clang.exe` 必须排在 PATH 最前**。`opencv` crate 的 bindgen 生成器会 `which clang`，
   LLVM 装了但 PATH 顺序不对会报 `Can't find clang binary`。
2. **`C:\Windows\System32\onnxruntime.dll` 是 1.17**，会先于我们的 1.31.1 被加载，
   `ort` 随即报 `expected version >= '1.31.x', but got '1.17.1'`。
   `scripts/env.ps1` 与 `dist/run.ps1` 都设置了 `ORT_DYLIB_PATH`；
   另外 `mhservice`/`mhcli` 的 `main` 会**自动**把 `ORT_DYLIB_PATH` 指向与自己同目录的
   `onnxruntime.dll`，所以即使不设环境变量也能正常加载。
3. **后端不用手工启动**。界面自己负责：连不上管道就拉起同目录的 `mhservice.exe`
   并等它就绪（最多 8 秒）。启动器（单文件 exe / `dist\run.ps1`）只是提前把后端
   拉好，两种入口行为一致。

   生命周期也归后端：同一数据目录只允许一个实例（独占锁），关窗口时前端只发
   `system.shutdown`，后端自己判断还有没有别的客户端；最后一个客户端走了、空闲
   8 秒自行退出。所以两个窗口可以共用一个后端，先关哪个都不会打断另一个，
   前端被强杀也不会留下常驻进程。

   连不上时界面会在标题栏下方常驻一条红色横幅说明原因（找不到 mhservice.exe /
   启动超时 / 管道被另一个数据目录的后端占着），识别与保存按钮点了只会报错——
   **不会退回演示数据**。

   `version` 返回的 `pid` / `dataDir` 是归属校验依据：连到的后端若不是本程序
   该用的那个数据目录，界面会明确报错，而不是往错误的库里写。

## 直接双击 `mhservice.exe` 报 “找不到 opencv_world4120.dll”

**这是曾经的 bug，现已修复。** 症状是弹出系统对话框：

> mhservice.exe - 系统错误
> 由于找不到 opencv_world4120.dll，无法继续执行代码。

原因：`opencv_world4120.dll` 是**加载期**依赖，Windows 加载器只在可执行文件所在目录、
`PATH`、以及系统目录里找它。之前 `cargo build` 的产物目录里没有这些 DLL，
只有在 `scripts\env.ps1` 预先改好 `PATH` 的终端里跑才成功 —— 直接双击必崩
（退出码 `-1073741515` / `0xC0000135`，即 `STATUS_DLL_NOT_FOUND`）。

修复方式（两个层面，都不再依赖外部环境变量）：

* `src/crates/mhservice/build.rs` 与 `mhcli/build.rs` 在构建后自动把
  `opencv_world4120.dll`、`onnxruntime.dll` 以及 `models/` 下的 OCR 模型
  复制到可执行文件旁边（`target\release\` 与 `deps\`）。
* `mhservice`/`mhcli` 的 `main` 启动时若未显式设置 `ORT_DYLIB_PATH`，
  就自动指向同目录的 `onnxruntime.dll`，避开 System32 里的旧版 1.17。

因此现在 `target\release\mhservice.exe` 和 `dist\MhRecorder\mhservice.exe`
**双击即可运行**，无需任何环境准备。回归脚本 `verify_real_captures.ps1`
第 5 段会用“清空 PATH 与所有 MH_/ORT_ 变量”的方式复现双击场景并断言通过。

`ort` 2.0.0-rc.15 的用法：`ort::init().with_name(..).build()` 每进程只允许一次，
返回进程级 `Environment`，再 `Session::builder(&env)?.commit_from_file(path)`。

## 录入识别的四种入口

「录入识别」页的四条路都汇到同一条识别流程（同一套遮罩、忙碌保护与错误提示）：

| 入口 | 做法 |
|---|---|
| 拖拽 | 把 PNG / JPG / BMP 拖到虚线区 |
| 选择文件 | 「或点击选择文件」，可多选，人物属性 + 背包一起识别 |
| 粘贴截图 | 点按钮或 `Ctrl+V`；支持剪贴板位图（`Win+Shift+S`）、资源管理器复制的图片文件，以及无文件头的 CF_DIB |
| 截图识别 | 列出可见应用窗口（预选游戏窗口），选中后切到前台抓取客户端区域 |

「截图识别」的上机细节：

* 先 `PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT)`，不抢焦点；
* 结果整片纯色（GPU 渲染的游戏窗口常见）时，改为 `SetForegroundWindow` + `BitBlt`
  屏幕区域，抓完再把应用窗口拉回前台，否则结果看不见；
* 像素直接落在自建的 32bpp top-down DIB section 里，用 WinRT `BitmapEncoder`
  编 PNG，不引入 GDI+ / System.Drawing；
* 抓到的画面若一个数值都没读到（比如截的是打怪画面而不是人物属性面板），不会展示
  全 0 的结果面板，而是在页面底部提示先打开「人物属性 / 背包」再截。

**窗口截图会先裁出游戏面板再识别。** 窗口截图里除了面板，还有游戏世界、HUD 时钟、
聊天框和任务追踪；所有字段提取逻辑都假设"整张图就是面板"，直接喂进去会把 `01:56`
读成金币、把任务追踪读成物品、把整张图判成"未识别"。所以 `mhvision::locate_panel()`
先用 Otsu 阈值 + 3×3 开运算找到画面里那块最大的浅色矩形（面板），再往下跑分类与
提取；找不到（截图本身就是面板，或画面里没有明显的面板）就原样使用整帧，不会因为
定位失败而裁错。排查某个具体截图时：

```powershell
.\src\target\release\mhcli.exe panel  samples\real_window_capture.png   # 会裁到哪
.\src\target\release\mhcli.exe ocr    samples\real_window_capture.png   # 逐行 OCR
$env:MH_DUMP_CAPTURE = 'D:\tmp\caps'   # 界面里每次截图都落盘一份原始 PNG
```

四条入口拿到的图都会先显示在左侧卡片的预览里（来源 + 原始分辨率，点图或「放大查看」
可全屏核对），预览下方可「重新截图 / 换一张」；识别期间只叠一层半透明遮罩，预览仍可见。

## 独立测试后端（不启动 UI）

```powershell
. .\scripts\env.ps1
.\src\target\release\mhcli.exe ocr      samples\character_haitaotao.png
.\src\target\release\mhcli.exe recognize samples\character_haitaotao.png
.\src\target\release\mhcli.exe features samples\character_haitaotao.png
.\src\target\release\mhcli.exe compare  samples\character_haitaotao.png samples\character_haitaotao.png
.\src\target\release\mhcli.exe db-list
.\src\target\release\mhcli.exe db-stats
.\src\target\release\mhcli.exe db-tasks 20        # 任务历史（耗时 / 成功与否）
.\src\target\release\mhcli.exe backup out.zip     # 完整备份（JSON + 库 + 清单）
.\src\target\release\mhcli.exe eval samples\dataset --json report.json
```

`eval` 把数据集跑一遍并打分：目录下每个子目录是一例（`expect.json` + 若干
截图），逐字段比较昵称 / 游戏ID / 金币 / 银币 / 面板类型；任一例失败退出码
就是 1，可以直接挂进 CI。`expect.json` 只写关心的字段，没写的字段不参与比较。

```json
{ "nickname": "莫莫莫", "gameId": "429277361", "gold": 2625812, "screen": "bag" }
```

管道协议也可用 `--stdio` 模式直接喂 JSON：

```powershell
'{"id":1,"method":"ping","params":{}}' | .\src\target\release\mhservice.exe --stdio
```

## 数据位置与备份

默认 `%LOCALAPPDATA%\MhRecorder\mhrecorder.db`（可用 `MH_DATA_DIR` 覆盖）。
设置页可导出 JSON / CSV / ZIP，也可清空本地数据。

**两种导出的口径不同，别混用：**

| 形式 | 内容 | 用途 |
|---|---|---|
| JSON / CSV（`export.json` / `export.csv`） | 角色、属性、货币、装备、背包 + 指纹 / 外观 / 物品字典 | 看、分析、喂给别的工具 |
| 完整备份（`export.backup`） | 上面的 JSON + **整个 SQLite 文件** + 清单 | 换电脑、重装后原样恢复 |

完整备份里必须带上 `fingerprints` / `appearances`：它们才是"包裹截图认得人"的
依据，丢了就等于把积累的识别能力清零，而这部分最难重建。数据库快照用
`VACUUM INTO` 生成，而不是直接拷文件——WAL 模式下直接复制主文件会漏掉还没
checkpoint 的事务。

## 单文件分发

```powershell
.\scripts\package_single.ps1
```

产物：**`dist\MhRecorder-<版本号>.exe`**（当前 `MhRecorder-0.13.0.exe`，86 MB，
双击即用，可以单独发给别人）。版本号取自 `MhRecorder.Core/Protocol.cs` 的
`AppVersion`，打包脚本自动拼进文件名，不需要手改。

## 发布到 GitHub Releases

`dist/` 是构建产物，**不进仓库**（见 `.gitignore`），一律走 Releases。
一条命令完成打包 → 打 tag → 建 Release → 上传：

```powershell
.\scripts\release.ps1                    # 用当前版本号发布
.\scripts\release.ps1 -SkipBuild         # 制品已在 dist，直接上传
.\scripts\release.ps1 -Draft             # 先存草稿
.\scripts\release.ps1 -Prerelease        # 标记预发布
```

上传四个文件，命名统一为「项目名 + 版本号」：

| 资产 | 说明 |
|---|---|
| `MhRecorder-<ver>-setup.exe` | **标准安装包**（Inno Setup）：安装向导 + 开始菜单 + 可选桌面快捷方式 + 卸载入口 |
| `MhRecorder-<ver>.exe` | 单文件便携版，双击即用（不写注册表） |
| `MhRecorder-<ver>-win-x64.zip` | 免安装目录版（`dist\MhRecorder` 整包） |
| `MhRecorder-<ver>-SHA256SUMS.txt` | 三个产物的 SHA-256，供下载后核对 |

Release 正文自动从 `Changelog.cs` 里抽取当前版本的条目，不用手写更新日志。

### 安装包（Inno Setup）

```powershell
.\scripts\package_installer.ps1        # 编译 dist\MhRecorder-<ver>-setup.exe
```

| 项 | 值 |
|---|---|
| 安装位置 | `%LOCALAPPDATA%\Programs\MhRecorder`（按用户，**不弹 UAC**） |
| 需要管理员 | 否（命令行给 `/ALLUSERS` 才转全机安装） |
| 快捷方式 | 开始菜单必建；桌面为可选任务，默认不勾 |
| 卸载 | 设置 → 应用，或开始菜单「卸载 MhRecorder」 |
| 卸载时数据 | 默认保留 `%LOCALAPPDATA%\MhRecorder`；勾选后才删 |
| 覆盖安装 | 自动结束正在运行的界面与后端（Restart Manager + taskkill） |
| 语言 | 简体中文向导（`packaging/ChineseSimplified.isl`） |
| 代码签名 | **无**，首次运行会触发 SmartScreen「更多信息 → 仍要运行」 |

编译器需要 Inno Setup 6（`winget install --id JRSoftware.InnoSetup -e`），
脚本按 `INNO_SETUP_HOME` → `build\tools\InnoSetup6` → Program Files 顺序查找 ISCC.exe。

### 为什么是"自解压单文件"而不是静态单文件

WinUI 3 / Windows App SDK 无法静态打包成一个文件，原因有三：

1. `Microsoft.ui.xaml.dll`、`Microsoft.Windows.SDK.NET.dll` 等必须在可执行文件
   **同目录**才能被 Windows 加载器找到；
2. 本应用是**两个进程**（界面 `MhRecorder.exe` + 后端 `mhservice.exe`）；
3. 还需要 OpenCV、ONNX Runtime 与 OCR 模型。

所以"一个文件"在 Windows 上的正确做法是**自解压启动器**：

```
[ mhlaunch 启动器映像（GUI 子系统，无控制台）][ ZIP 载荷（整个应用）][ 尾部索引 56 字节 ]
                                                                     ├ payload offset (u64)
                                                                     ├ payload length (u64)
                                                                     ├ payload SHA-256 (32B)
                                                                     └ magic "MHPACK01" (8B)
```

首次双击时：

1. 读尾部索引定位载荷，**先校验 SHA-256**（防止下载不完整就解压出坏程序）；
2. 解压到 `%LOCALAPPDATA%\MhRecorder\app\<载荷哈希前16位>\`（实测 1.9 秒，约 211 MB）；
3. 用 Win32 `CreateFileW` 探测 `\\.\pipe\mhrecorder.v1`，先拉起后端再启动界面；
4. 启动器退出，应用留驻。

再次启动直接命中已解压目录，**实测 1.1 秒**。数据仍在
`%LOCALAPPDATA%\MhRecorder\mhrecorder.db`，升级新版本不会覆盖用户数据。

### 为什么分成 mhpack 与 mhlaunch 两个 exe

同一份源码产出两个可执行文件，子系统（PE subsystem）不同：

| 产物 | 子系统 | 角色 |
|---|---|---|
| `mhlaunch.exe` | GUI（2） | **随包分发的外壳**。双击时 Windows 不为其分配控制台，所以既没有闪窗也没有常驻黑窗 |
| `mhpack.exe` | Console（3） | 仅打包机使用：`pack` / `verify` 需要 stdout 与退出码，脚本要能读到 |

0.9.2 曾用**一个** console 子系统的 exe 同时干这两件事：双击时 Windows 给它分配控制台，
`mhservice` 又通过 `AttachConsole(ATTACH_PARENT_PROCESS)` 挂上去打日志，于是黑窗在整个使用
期间一直存在。反过来，把唯一的 exe 改成 GUI 子系统又会让 PowerShell 不等它结束、读不到输出，
打包脚本会静默产出旧文件。拆成两个 exe 才能同时满足这两点。

`mhservice` 本身也不再无条件挂接父进程控制台：只有 `--stdio` 或 `MH_LOG_CONSOLE=1` 时才写
控制台，否则日志一律进 `%LOCALAPPDATA%\MhRecorder\mhservice.log`。

校验 / 排查用打包机上的 console 工具，而不是分发出去的 GUI 外壳：

```powershell
.\src\target\release\mhpack.exe --verify .\dist\MhRecorder-0.13.0.exe   # 校验内置载荷
.\src\target\release\mhpack.exe --extract-to D:\tmp\app                # 解压到指定目录（排查用）
```

### 打包项目体积

| 项 | 大小 |
|---|---|
| 解压后（含全部运行时） | 211 MB |
| 单文件压缩后 | 86 MB |
| 其中 OCR 模型 | 14.9 MB |
| 其中 OpenCV DLL | 61 MB |
| 其中 ONNX Runtime DLL | 16.5 MB |

打包脚本会裁掉 81 个用不到的语言资源目录（只保留 zh-CN / zh-TW / en-us / en-GB）
和 pdb 调试文件。

## 识别流程（重要业务规则）

真机截图分两类，顺序不能颠倒：

1. **人物属性**（`real_character_attributes.png`）—— 头部含 `昵称 89级` 与 `ID <游戏ID>`，
   右侧是气血/魔法/物伤/法伤/物防/法防/速度/治疗强度。
   这是**身份来源**：只有它能给出游戏ID。
2. **包裹**（`real_inventory.png`）—— 货币只有图标、没有文字标签：
   `[金币图标] 2,625,812` `[银币图标] 9,828,458`。

因此：**先识别人物属性拿到游戏ID，再识别包裹**是最省事的顺序，两者会合并到同一角色。
单独上传包裹时，若系统还认不出它属于谁，录入页会让用户**从已入库的角色里选一个**
（下拉列表每行是 `昵称 Lv.89 · 429277361`，同名的两个角色靠游戏ID区分），
选完本次记录归到它名下，并把这些截图的外观特征记下来。
后端在完全没有身份来源时仍会返回警告
「请先识别并录入「人物属性」截图获取游戏ID，才能把包裹中的金币/银币归到该角色」，
并且**不会**凭空创建一个「未命名角色」——真的什么身份都没有时，这次结果会进
「待归属」表，等你在设置里指定角色（见下文）。

> 反向的规则同样重要：截图里**自带**游戏ID或昵称时（人物属性页），身份以面板为准。
> 那个ID在库里没有对应行，就是**新角色**，不能被"发色相似"改成已有角色的名字。

同一 `gameId` 再次保存只会新增快照，不会重复建档。

### 包裹截图怎么自动对上角色（多证据加权 + 冲突保护）

包裹面板**不显示游戏ID、不显示昵称、不显示等级**——屏幕上没有任何身份字段。
所以第一次遇到某个角色的包裹时，界面会让你从已入库的角色里选一次归属；
保存时把这张面板与角色的外观一并记下来，之后同类截图自动关联。

识别不是一个阈值说了算，而是把几路弱证据加权求和（`mhvision::MatchPolicy`）：

| 证据 | 权重 | 单独能定案吗 | 说明 |
|---|---|---|---|
| 面板缩略图 | 0.55 | 强度 ≥ 0.90（≈相似度 0.98）可以 | 32×32 灰度、去均值余弦 |
| 人物评分 | 0.40 | **不能** | 随装备/等级变化，只作辅助 |
| 发色 / 武器 | 0.35 | 强度 ≥ 0.55 可以 | 描述角色本身，物品换了也还在 |
| 头像 | 0.25 | 强度 ≥ 0.95 可以 | 16×16 灰度，人脸本身 |
| 时间上下文 | 0.10 | 不能 | 24h 内采集过加分，30 天后归零 |

判定规则：**要么某个信号本身足够决定性，要么两个以上信号互相印证并越过总分线**；
并且必须领先第二名 0.08 以上——接近平手时宁可交给你选，也不猜。

> 权重之和刻意大于 1：互相印证能越过判定线，孤立信号停在线上。这些数字是在约 40 张
> 真图上拟合出来的**启发式**，集中在 `MatchPolicy` 里便于重标定，不散落在各处 `if`。

**为什么不再用单一阈值**：旧实现用 `sim >= 0.94` 一条线，换个发色、换套时装、改个染色、
换坐骑、分辨率变化，任何一项都能让它掉到线下；而 `人物评分` 被当成"精确身份键"给了 0.93，
可它本身随装备和属性浮动，不是永久 ID。

**冲突保护**：面板上写着游戏ID时，视觉证据**永远不得覆盖**。两者不一致时以游戏ID 为准，
并返回一条 `conflicts`，界面会显示「视觉匹配指向 A，但游戏ID 明确是 B，已以游戏ID 为准」——
视觉模型退化时能一眼看见，而不是悄悄错下去（`decide_identity()`）。

**每项证据都留档**：候选结构 `IdentityCandidate` 里带 `evidence[]`，逐条记录
`kind / strength / contribution / detail`，界面按 `莫莫莫（72%）包裹面板相似度 0.98；人物评分 20214 一致`
显示。证据不足时会给出排序后的候选列表，而不是硬猜一个。

**只在候选唯一时采用外观**：只要库里还有角色没记过外观特征，就不做"仅凭外观"的定案——
那个角色可能就是真正的归属，猜错人比让你再选一次更糟。

指纹按 `(角色, 面板类型)` 只存最新一条（物品会变），存在 `character_fingerprints` 表；
外观特征每个角色一条，存在 `character_appearance` 表；
头像缩略图存在 `characters.avatar_thumb`（schema v3，老库自动 `ALTER TABLE` 补列）。

> 为什么不用全局 pHash/ORB：包裹和人物属性面板都是"屏幕中间一块浅色羊皮纸"，
> 全局 pHash 给它们 0.91 的高分，完全分不开；ORB 能分开（0.70 vs 0.14）但描述子太大。

### 一次导入多张图：先逐图读完，再决定能不能合并

旧实现把所有截图的 OCR 行拼成一个池子，最后统一提取身份：

```
A → OCR ─┐
B → OCR ─┼→ 一个 all_lines 池子 → 取游戏ID/昵称/等级 → 合并属性/货币/物品 → 保存
C → OCR ─┘
```

这会把「甲的人物属性 + 乙的包裹」变成「甲的金币」——池子里同时存在两个角色的
身份字段，谁先被扫到就用谁。现在每张图只认自己的面板：

```
A → OCR → 身份/属性/货币/物品/审核框 → ScreenResult(A)
B → OCR → 身份/属性/货币/物品/审核框 → ScreenResult(B)
        ↓
   plan_batch() 批次身份校验
        ↓
  一致 → 合并（只合并同组的图）
  冲突 → 只合并主组，其余图排除并说明原因
```

`RecognitionResult` 因此多了两个字段：

* `screenResults[]`：每张图自己读到的东西（身份、属性、货币、装备、物品、审核框、
  `included` 与 `excludedReason`）。**合并后的值只是视图，来源永远能追溯**。
* `batchIdentity`：`consistent` / `conflict` / `unidentified`。

判定规则（`mhvision::identity::plan_batch`，纯函数，可单测）：

| 情况 | 行为 |
|---|---|
| 两张图都写着游戏ID 且不同 | 冲突，只合并主组，另一张进 `screenResults` 并被标记排除 |
| 一张有游戏ID、一张没有 | 允许合并；有库参照时再做一次视觉复核，指向别人就排除 |
| 两张都有昵称且不同 | 冲突，同上 |
| 整批没有任何身份字段 | `unidentified`：仍按同一角色合并，但保存前必须由人指定归属 |

审核框只画进合并了的那些图：被排除的图上的数字不属于这条记录，显示出来只会误导。

### 存储层是身份安全的最后一道闸门

`recognize.save` 是后端接口，界面上的"请选择归属"不是保护——任何客户端都能直接调。
所以 `mhstore::save_recognition` 自己按下面的顺序判断（`resolve_target`）：

| 情况 | 行为 |
|---|---|
| 明确 `character_id` 且存在 | 写进该角色 |
| 游戏ID 唯一匹配 | 写进该角色 |
| 游戏ID 匹配多个 | **拒绝**（`identityAmbiguous`），提示先清理重复记录 |
| 昵称唯一匹配 | 写进该角色（置信度按昵称算，仍是未确认） |
| 昵称匹配多个 | **拒绝**，要求手动指定 |
| 完全没有身份信息 | 进 `pending_recognitions` 待归属表，**不建角色** |

以前那里是 `WHERE nickname = ? ORDER BY updated_at DESC LIMIT 1`——"同名就挑最近更新的
那一行"。两个区的同名角色会被写串，而且没有任何报错。现在宁可拒绝。

待归属记录在「设置 → 数据 → 待归属的识别结果」里处理：选中一条、选一个角色、
点「归属」，它会补成正式快照（指纹、外观、物品字典都补齐），然后从列表消失。
丢弃需要点两次，因为删除不可撤销。

### latest_stats / latest_currencies 按字段合并

`{}` 不覆盖 `{hp,mp}` 只是第一层。第二层是"非空不等于完整"：只读到气血的截图，
不能把上次的魔法和速度抹掉。所以保存时先把库里的旧 JSON 读出来，逐字段合并
（新值为 `Some` 才覆盖），再写回：

```
旧 {hp:5459, mp:3200, speed:800}
新 {hp:5600}
→ {hp:5600, mp:3200, speed:800}
```

`snapshots` 存的始终是**这一次读到的原样**，不做合并——历史曲线要的是每次观察到的值，
合并只属于 `characters.latest_*` 这个"当前状态"视图。

### 昵称为什么不能当身份键

游戏里**不同服务器可以有同名角色**。旧实现 `known.iter().find(|c| c.nickname == nick)`
取第一个命中，两个区的同名角色会被悄悄合并成一个。

现在的规则（`resolve_identity()`）：

1. 游戏ID 命中 → 直接认（0.98），未命中说明是新角色，不拿别人的行顶上；
2. 昵称**只在唯一命中**时使用；服务器字段只用来在同名里缩小范围，不反向否决唯一候选；
3. 同名多于一个 → 返回候选列表、标记 `Ambiguous`，让用户选；
4. 一个游戏ID 对应多行（库数据重复）→ 同样返回候选并提示。

数据库层面给 `game_id` 加了**部分唯一索引**（`WHERE game_id IS NOT NULL AND game_id <> ''`），
但作用域（全区唯一 vs 服务器内唯一）还没确认，所以做成"能建就建"：老库里若已有重复，
不因此启动失败，冲突交给上面的候选分支提示。

已用真图验证（`scripts/verify_real_captures.ps1`，33 项全部通过）：

| 字段 | 真图读数 | 识别结果 |
|---|---|---|
| 昵称 | 莫莫莫 | 莫莫莫 |
| 游戏ID | 429277361 | 429277361 |
| 等级 | 89级 | 89 |
| 气血 | 5604/5604 | 5604 |
| 魔法 | 2267/2267 | 2267 |
| 物理伤害 | 709 | 709 |
| 法术伤害 | 441 | 441 |
| 物理防御 | 1609 | 1609 |
| 法术防御 | 1081 | 1081 |
| 速度 | 1256 | 1256 |
| 治疗强度 | 133 | 133 |
| 金币（包裹） | 2,625,812 | 2,625,812 |
| 银币（包裹） | 9,828,458 | 9,828,458 |

### 真图上踩到并已修复的 OCR 问题

* **标签被拆成单字**：游戏内标签字间距很宽（气 与 血 相隔 20px），检测器按字形出框。
  已加入 `merge_adjacent_fragments` 把同一行的单字碎片合回词。
* **字形误识**：`血` 被识别成 `面`。除了词表匹配，还按首字兜底（气→hp、魔→mp、速→speed）。
* **标签与数值同框**：真图是 `物理伤害709` 这种无分隔连写，不能用空格切分，
  改为按已知标签前缀剥离。
* **ID 与数字分框**：`ID` 与 `429277361` 是两个独立框，改为按行内横向位置配对。
* **货币无文字标签**：包裹只有图标 + 纯数字，改为在“像包裹”的图上按横向位置取
  最下方一行的前两个纯数字；并加 `looks_like_bag()` 守卫，
  避免把人物属性页的游戏ID/经验当成货币。
* **昵称误取公告**：原本取到顶部滚动公告，改为优先从 `等级` 标记左侧取名，
  且仅在存在身份信息时才填昵称。

## 识别置信度与人工纠正

角色身份关联带置信度，并在界面上显示（如「识别置信度 96%」）。
「保存前可修正」开启时，保存前可改名字、游戏ID、等级；
人工纠正会把该角色标记为「已确认身份」，置信度置为 1.0。

身份解析分两层，**不再是一条优先级链**：

1. **面板文字层**（`resolve_identity()`）：游戏ID 唯一权威 → 昵称（仅唯一命中）→ 都不成立则
   交给视觉层；同名多行时返回候选并标记 `Ambiguous`。
2. **证据层**（`associate_by_evidence()`）：把面板缩略图、人物评分、发色/武器、头像、
   时间上下文加权求和，输出带逐项证据的候选列表；与面板文字冲突时**以面板为准**。

置信度现在是"证据总分"而不是伪概率：面板+评分 ≈ 0.95，只有发色武器 ≈ 0.35，
只有评分 ≈ 0.40（且不足以定案）。看到低分就该点开证据看看是哪一路在说话。

## 资产统计：未知不是 0

`AssetPoint.gold / silver` 是 `Option<i64>`，`null` 表示**当天没有任何一张截图带这个字段**。

旧实现取"每角色当天最新的一条快照"，再从里面读金币银币；可人物属性截图根本不渲染货币
（`{}`），于是 11 点拍了张人物属性，10 点包裹里那 260 万金币就被覆盖成 0，界面显示
「金币归零」。

现在按字段分别取**当天最后一次有效记录**，再跨角色求和：

```
角色 A
├── 金币：当天最后一次有效记录（10:00 的包裹）
├── 银币：当天最后一次有效记录
└── 从未采集到的字段 → null，界面显示「—」，不是 0
```

环比同样只在两天都有该字段时才计算，否则显示「—」。

## 后端工程化（0.11）

### 数据库迁移：版本化，不再"每次启动重跑建表"

`mhstore/migrations/` 下每个文件是一步迁移，编号即版本，编译期嵌入二进制。
版本号存在 `PRAGMA user_version` 里，每一步单独一个事务：

1. 读当前版本；
2. 库比程序**新**就拒绝打开（旧程序不许写新结构）；
3. 逐条执行 `version > current` 的迁移，任一步失败整步回滚，版本号不前进；
4. 迁移后自检：代码要用的表和列必须真的存在。

```
001_initial.sql              字符/快照/设置/特征缓存
002_character_fingerprints   面板视觉指纹
003_character_appearance     发色 / 武器
004_item_database            物品字典 + 谁见过它
005_recognition_tasks        识别任务历史
```

`001` 刻意全部用 `IF NOT EXISTS`：0.10.0 及更早的库从没写过 `user_version`，
第一次被新引擎打开时版本是 0，会从头重放一遍，必须幂等。

### IPC 边界：一行最多多大，说清楚

| 上限 | 默认 | 环境变量 |
|---|---|---|
| 单行请求 | 32 MB | `MH_MAX_MESSAGE_MB` |
| 一次几张图 | 8 | `MH_MAX_IMAGES` |
| 单图像素 | 40 MP | `MH_MAX_IMAGE_MP` |
| 单图字节 | 48 MB | `MH_MAX_IMAGE_MB` |
| 并发连接 | 8 | `MH_MAX_CONNECTIONS` |
| 单次识别时长 | 180 s | `MH_REQUEST_TIMEOUT_SEC` |

读行改成有界实现：超过上限就把这一行读到换行处丢弃，回一条
`tooLarge`，连接照常可用——`read_line` 会把缓冲区一直撑到换行出现，客户端
一个 bug 就能让服务端吃光内存。

图片检查从便宜到贵逐级做：张数 → base64 估算字节（不解码）→ 真实字节 →
**图片头里的像素数**。最后一项防解压炸弹：几百 KB 的 PNG 能解出两亿像素。

协议版本不兼容时明确拒绝（`protocolMismatch`），而不是拿半懂的格式继续跑。

### 任务生命周期

每次 `recognize` 在服务端登记一个任务：开始时间、耗时、结果。运行中可以通过
`tasks.list` 看到，通过 `tasks.cancel` 请求取消——识别在**每张图之间**检查
取消标志与截止时间，所以延迟上限是一张图（这也是用户能感知的粒度）。
结束后写一行到 `recognition_tasks`，崩过一次之后能回答"上次跑到哪一步、
花了多久"，而不是只剩一行 stderr。

### 特征算法：先把名字改对

原来的 `compute_phash` 其实是 **dHash**（缩到 9×8，比相邻像素），里面没有
任何余弦变换。名字错了，围绕"调 pHash 阈值"的讨论就全在讨论一个不是 pHash
的数。现在叫 `compute_dhash`。

ORB 的匹配也补了两件事：

* **双向**：i 认 j 最近，j 也必须认 i 最近，单向的偶然匹配丢掉；
* **几何验证**：用 RANSAC 拟合相似变换（缩放 + 旋转 + 平移），只统计落在
  模型上的点。相似纹理（同一套 UI 边框、同一个图标网格）以前能刷出虚高的
  分数，现在过不了这一步。分数同时要求"比例高"和"内点够多"（≥15）。

HSV 直方图从"只看色相"改成三段：色相（按饱和度×亮度加权）+ 饱和度 + 亮度。
深蓝的袍子和亮蓝的旗子以前落进同一个桶，现在靠后两段分得开。

### 图像分析缓存

同一张截图被导入两次（重试、批量里重复、隔几天再导一次）时，第二次不必再跑 OCR。
键是**图片字节的 SHA-256**，命中即跳过 OCR，直接复用上次的文字行：

```
图片字节 → SHA-256(v3:<hash>) → 查 image_features
  命中 → 用缓存里的 OCR 行（再用 dHash 复核这条缓存确实属于这张图）
  未命中 → OCR + 特征 → 写回缓存 → 本次结果带 cacheHits
```

* 缓存内容：OCR 原始行 + dHash + HSV 直方图。ORB 描述子不进缓存（每次比对都要重算，
  存下来没人用）。合并相邻片段、字段提取、身份关联、审核框在每次请求里**重跑**，
  所以改提取规则不需要让缓存失效。
* 算法版本（`mhcore::ANALYSIS_VERSION`，当前 `3`）进键前缀，同时写进每条 payload
  和导出清单的 `algorithmVersion`。改了特征算法或换了 OCR 模型就 +1，老条目自然失效，
  不会被当成"合法的旧结果"读出来。
* 命中时用 dHash 复核：缓存表是人可读可改的 SQLite 文件，而"用了别人的 OCR 行"
  比"没命中"糟得多。
* 缓存上限 2000 条（约几 MB），超出按写入时间删最旧。
* 前端会说明"这次复用了上次的结果"，避免"秒出"看起来像没干活。

实测（1265×711 的 人物属性 截图，同一进程第二次请求）：583 ms → 128 ms，
`cacheHits` 长度 1，字段值与 12 个审核框坐标完全一致。

## 设计稿

`design/page_01.png` … `page_04.png` 对应四个页面：
录入识别、角色管理、资产统计、设置。
