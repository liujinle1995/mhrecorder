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
│   │               PP-OCRv4 det/rec, pHash,   │
│   │               ORB, HSV 直方图            │
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
| 特征识别 | pHash（9×8 差分哈希）+ ORB + HSV 32 桶直方图 |
| 通信 | Windows 命名管道，行分隔 JSON |

## 目录

```
mhrecorder/
├─ src/                    Rust workspace
│  └─ crates/
│     ├─ mhcore/           领域模型 + 管道协议（前后端契约）
│     ├─ mhvision/         图像 / OCR / 特征
│     ├─ mhstore/          SQLite 持久化
│     ├─ mhservice/        命名管道服务端 + 方法分发
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
│  ├─ test_pipe_e2e.ps1    管道端到端测试
│  ├─ verify_vision.ps1    OCR / 特征回归测试
│  ├─ verify_real_captures.ps1  真机截图验收（33 项，数值逐项对照真图）
│  └─ package.ps1          打包 dist/（免安装目录版）
│  └─ package_single.ps1   打包单文件 exe（自解压，推荐分发方式）
│  ├─ version.ps1          版本号与产物命名的唯一来源（读 Protocol.cs）
│  └─ release.ps1          把 dist 产物发布到 GitHub Releases
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

窗口固定为设计稿尺寸 **1328×888 逻辑单位**（比例 1.495，取自 `design/page_01.png`
实测 2766×1850 px @2.0836 px/pt），并按显示器 DPI 换算成物理像素。
**禁止最大化**、**禁止拖拽缩放**（`IsResizable=false` + `IsMaximizable=false`），
因为界面尚未做响应式适配，放大后布局会错乱。

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

## 从源码构建的常见坑（均已在脚本中处理）

1. **`clang.exe` 必须排在 PATH 最前**。`opencv` crate 的 bindgen 生成器会 `which clang`，
   LLVM 装了但 PATH 顺序不对会报 `Can't find clang binary`。
2. **`C:\Windows\System32\onnxruntime.dll` 是 1.17**，会先于我们的 1.31.1 被加载，
   `ort` 随即报 `expected version >= '1.31.x', but got '1.17.1'`。
   `scripts/env.ps1` 与 `dist/run.ps1` 都设置了 `ORT_DYLIB_PATH`；
   另外 `mhservice`/`mhcli` 的 `main` 会**自动**把 `ORT_DYLIB_PATH` 指向与自己同目录的
   `onnxruntime.dll`，所以即使不设环境变量也能正常加载。
3. **必须先启动 `mhservice.exe` 再开界面**。界面通过命名管道连接后端；
   若后端没起，界面会退回内存演示数据（状态栏显示「离线演示数据」），
   识别功能形同虚设。`dist/run.ps1` 已自动拉起后端并等待就绪，
   状态栏显示「已连接本地识别引擎」即表示真的连上了。

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
```

管道协议也可用 `--stdio` 模式直接喂 JSON：

```powershell
'{"id":1,"method":"ping","params":{}}' | .\src\target\release\mhservice.exe --stdio
```

## 数据位置

默认 `%LOCALAPPDATA%\MhRecorder\mhrecorder.db`（可用 `MH_DATA_DIR` 覆盖）。
设置页可导出 JSON / CSV / ZIP，也可清空本地数据。

## 单文件分发

```powershell
.\scripts\package_single.ps1
```

产物：**`dist\MhRecorder-<版本号>.exe`**（当前 `MhRecorder-0.9.9.exe`，86 MB，
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

上传三个文件，命名统一为「项目名 + 版本号」：

| 资产 | 说明 |
|---|---|
| `MhRecorder-0.9.9.exe` | 单文件便携版，双击即用（推荐） |
| `MhRecorder-0.9.9-win-x64.zip` | 免安装目录版（`dist\MhRecorder` 整包） |
| `MhRecorder-0.9.9-SHA256SUMS.txt` | 两个产物的 SHA-256，供下载后核对 |

Release 正文自动从 `Changelog.cs` 里抽取当前版本的条目，不用手写更新日志。

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
.\src\target\release\mhpack.exe --verify .\dist\MhRecorder-0.9.9.exe   # 校验内置载荷
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
并且**不会**凭空创建一个「未命名角色」。

> 反向的规则同样重要：截图里**自带**游戏ID或昵称时（人物属性页），身份以面板为准。
> 那个ID在库里没有对应行，就是**新角色**，不能被"发色相似"改成已有角色的名字。

同一 `gameId` 再次保存只会新增快照，不会重复建档。

### 包裹截图怎么自动对上角色（外观特征 + 视觉指纹）

包裹面板**不显示游戏ID、不显示昵称、不显示等级**——屏幕上没有任何身份字段。
所以第一次遇到某个角色的包裹时，界面会让你从已入库的角色里选一次归属。
保存会把这张面板"记下来"，之后同类截图自动关联。

三个信号，可靠度从高到低：

1. **人物评分**（`人物评分 20214`，包裹面板左下）——每角色唯一、精确，即使物品格换了也还认得出。
   OCR 常把「人物」读成「入物/人份」，所以锚点是「评分」两字，不是整词。
2. **面板缩略图**——裁出的面板转成 32×32 灰度图，做去均值余弦相似度。
   实测：同一张包裹面板再截一次 ≈ 0.98；不同角色的两个人物属性面板 ≈ 0.82；
   包裹 vs 人物属性 ≈ 0.48（所以判定阈值定在 0.94，且要求领先第二名 0.03 以上，
   接近平手时宁可判"不知道"也不猜错人）。
3. **外观特征（发色 + 武器高光色）**——描述的是**角色**而不是这张图，
   所以物品换了一批、面板像素完全变了，甚至换了一种面板（包裹 ↔ 人物属性）都还能对上。
   做法：按面板类型取头部区域，算暗像素均值得到发色；取角色模型区域里青碧色高光
   （H 140–205、V≥165、S≥35）的均值得到武器色。两处都是"均值"而不是"众数"——
   众数会被帽子/阴影这类单一色块带偏（骑乘模型上就选到了帽檐的纯黑）。

   实测同一角色的两块面板：发色 32 / 武器 30（RGB 欧氏距离）；
   另一个角色：发色 45、武器 94。所以门槛是**发色 ≤ 50 且武器 ≤ 70**，
   两个都过才算同一角色；只靠发色会把发色相近的两个角色混为一谈。

> 为什么不用全局 pHash/ORB：包裹和人物属性面板都是"屏幕中间一块浅色羊皮纸"，
> 全局 pHash 给它们 0.91 的高分，完全分不开；ORB 能分开（0.70 vs 0.14）但描述子太大。
> 32×32 面板缩略图的区分度和体积都更合适。

指纹按 `(角色, 面板类型)` 只存最新一条（物品会变），存在 `character_fingerprints` 表；
外观特征每个角色一条，存在 `character_appearance` 表（`hair_name/hair_rgb/weapon_name/weapon_rgb`）。
保存包裹时同时更新该角色的人物评分与外观特征。

**只在候选唯一时采用**：只要库里还有角色没记过外观特征，就完全不用外观这一路——
那个角色可能就是真正的归属，猜错人比让你再选一次更糟。

界面上的「来源」下方会写明依据，例如
`人物评分 20214 与「莫莫莫」一致`、`画面与「莫莫莫」此前录入的包裹截图一致（98%）`、
`发色 深蓝 与「莫莫莫」一致，武器同为青碧`，并且单独一行显示
`外观特征：发色 深蓝 · 武器 青碧`（这是以后自动关联的依据，判错时先看这一行）。
置信度也随之给到 0.76–0.95 而不是"视同识别成功"。

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
身份匹配优先级：游戏ID精确匹配 → 昵称匹配 → 人物评分 → 面板缩略图 → 外观特征
（前两者来自面板文字，后三者来自上面「包裹截图怎么自动对上角色」一节的视觉信号）。
外观特征排在最后：它最不精确，但也是唯一在物品变化、面板类型变化后还成立的一路。

## 设计稿

`design/page_01.png` … `page_04.png` 对应四个页面：
录入识别、角色管理、资产统计、设置。
