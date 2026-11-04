# mhrecorder — 架构与协作契约

梦幻西游手游截图记录助手。前端 WinUI 3 + C#，后端 Rust，SQLite，OpenCV，RapidOCR(PP-OCRv4 ONNX)，
特征识别 pHash + ORB + HSV，命名管道通信。

## 目录结构

```
D:\app\mhrecorder
├─ design\            设计稿渲染图 (page_01..04.png) + 提取的文本
├─ native\            随包分发的原生依赖（不要提交改动）
│  ├─ opencv\         include\ + lib\opencv_world4120.lib + bin\opencv_world4120.dll
│  ├─ onnxruntime\    lib\ (headers + .lib) + bin\onnxruntime.dll
│  └─ models\         ch_PP-OCRv4_det_infer.onnx / rec / ppocr_keys_v1.txt
├─ scripts\env.ps1    构建环境变量（每次 cargo 前先 `. .\scripts\env.ps1`）
├─ src\               Rust workspace
│  └─ crates\
│     ├─ mhcore\      领域模型 + 管道协议（已定稿，勿改公开类型）
│     ├─ mhvision\    图像/OCR/特征
│     ├─ mhstore\     SQLite
│     ├─ mhservice\   命名管道服务端 + 分发
│     └─ mhcli\       独立测试 CLI（后端可脱离 UI 测试）
└─ MhRecorder\        C# 解决方案
   ├─ MhRecorder.Core\  模型 + 协议 + 服务 + ViewModel（不引用 WinUI）
   └─ MhRecorder.App\   WinUI 3 界面（页面 / 控件 / 平台适配）
```

## 构建

```powershell
# Rust
. .\scripts\env.ps1
cd D:\app\mhrecorder\src
cargo build --release
cargo test --release

# C#
cd D:\app\mhrecorder\MhRecorder
dotnet build -c Release
```

环境已经就绪：LLVM 已装（`C:\Program Files\LLVM\bin`，已在用户 PATH 中），
OpenCV 4.12 world 已 vendor，ONNX Runtime 1.31.1 已 vendor（`ort` 用 `load-dynamic`，无需 cmake）。

### 两个必踩的环境坑（已解决，务必保留）

1. **`clang.exe` 必须排在 PATH 最前**。opencv crate 用 `opencv-binding-generator` 需要
   clang，且它会 `which clang`；LLVM 装了但如果 PATH 顺序不对会报 `Can't find clang binary`。
2. **`C:\Windows\system32\onnxruntime.dll` 是 1.17**，会先于我们的 1.31.1 被 `ort` 加载，
   然后报 `expected version >= '1.31.x', but got '1.17.1'`。
   必须显式设置 `ORT_DYLIB_PATH` 指向 `native\onnxruntime\bin\onnxruntime.dll`。
   发行版启动 `mhservice.exe` 时同样要设这个环境变量（或复制 dll 并保证其优先）。

`ort` 2.0.0-rc.15 的 API：`ort::init().with_name(..).build()` 返回进程级唯一 `Environment`
（每进程只能一个），再 `Session::builder(&env)?.commit_from_file(path)`。

## 通信协议（已定稿）

见 `src/crates/mhcore/src/protocol.rs`。换行分隔的 UTF-8 JSON，一行一请求/响应。

请求：`{"id":1,"method":"characters.list","params":{}}`
响应成功：`{"id":1,"ok":true,"result":{...}}`
响应失败：`{"id":1,"ok":false,"error":{"code":"notFound","message":"..."}}`

方法名见 `protocol::method`，错误码见 `protocol::errcode`。

管道名：`mhrecorder.v1`，服务端为 pipe server，客户端连接。

### 方法清单（实现方需严格遵守）

| method | params | result |
|---|---|---|
| `ping` | `{}` | `{"status":"ready","version":"0.16.0","protocol":2}` |
| `version` | `{}` | `{"backend":"0.16.0","protocol":2,"models":"ppocrv4","pid":1234,"dataDir":"C:\\…\\MhRecorder","connections":1}` |
| `recognize` | `{"images":[{"name":"a.png","data":"<base64>"}]}` | `RecognitionResult`（**同步**，连接被占到底；只给 mhcli / 测试用） |
| `recognize.start` | `{"images":[…]}` | `{"taskId":"t1-…","state":"pending"}`（**立刻返回**，OCR 在后台线程） |
| `tasks.get` | `{"id":"…","consume":true}` | `{"id":"…","state":"running","elapsedMs":820,"terminal":false}`；终态时附 `result` 或 `error`，且只给一次 |
| `recognize.save` | `{"result":RecognitionResult,"screens":["a.png"]}` | `{"characterId":"…","pending":false}` 或 `{"characterId":null,"pending":true,"pendingId":"…","message":"…"}` |
| `characters.list` | `{}` | `{"characters":[CharacterDetail...]}` |
| `characters.get` | `{"id":"..."}` | `CharacterDetail` |
| `characters.update` | `{"id":"...","nickname":"?","gameId":"?","level":?,"server":"?"}` | `{"ok":true}` |
| `characters.confirm` | `{"id":"...","confidence":0.96}` | `{"ok":true}` |
| `characters.delete` | `{"id":"..."}` | `{"ok":true}` |
| `statistics.get` | `{"recentLimit":5}` | `{"summary":AssetSummary,"characters":[CharacterAssetBar...],"recent":[AssetPoint...]}` |
| `settings.get` | `{}` | `Settings` |
| `settings.set` | `Settings` | `{"ok":true}` |
| `export.json` | `{"path":"C:\\..."}` | `{"path":"...","bytes":123}` |
| `export.csv` | `{"path":"..."}` | `{"path":"...","bytes":123}` |
| `export.zip` | `{"path":"..."}` | `{"path":"...","bytes":123}` |
| `data.clear` | `{}` | `{"ok":true}` |
| `pending.list` | `{"limit":50}` | `{"items":[PendingRecognition...],"count":3}` |
| `pending.assign` | `{"id":"…","characterId":"…"}` | `{"characterId":"…","pending":false}` |
| `pending.discard` | `{"id":"…"}` | `{"ok":true}` |
| `tasks.list` | `{"limit":20}` | `{"running":[…],"history":[…]}` |
| `tasks.cancel` | `{"id":"…"}` | `{"cancelled":true,"id":"…","state":"cancelling"}`；未知 id 返回 `taskNotFound` |
| `system.shutdown` | `{"force":false}` | `{"shuttingDown":true,"clients":1}`；还有别的客户端时 `{"shuttingDown":false,"clients":2}` |

### 为什么识别要拆成 start / get / cancel

管道是「一条连接一个线程，请求串行」。同步 `recognize` 一旦发出，服务端就在这条
连接上跑到 OCR 结束，**期间读不到这条连接上的任何后续请求**——包括 `tasks.cancel`。
所以 0.15 及以前的取消在协议层面不可能生效：`taskId` 是随结果一起返回的，用户
点取消时前端连要取消谁都不知道。

v2 改成：

```
recognize.start ──> {taskId}          立刻返回，连接马上空出来
       │
       ├─ tasks.get {id}              每 120–400ms 查一次状态
       └─ tasks.cancel {id}           用户点取消，同一条连接、立刻被处理
                                       状态 running → cancelling → cancelled
```

任务状态机见 `src/crates/mhservice/src/tasks.rs`：`pending / running / cancelling /
cancelled / completed / failed / timeout`。取消是协作式的（OpenCV / ONNX Runtime
都是同步调用），粒度是一张图，所以 `cancelling` 是真实存在、可能停留若干秒的状态。

识别是**独占**的：已有任务在跑时 `recognize.start` 返回 `taskBusy`，不排队。

### 实例身份与生命周期

后端 `version` 会报出 `pid` 与 `dataDir`，前端据此判断"连着的到底是谁"。后端启动
时按数据目录加独占锁（`mhservice.lock`，共享模式 0），同一目录不可能有两个实例；
进程退出即释放，不留僵尸锁。

关停由后端裁决：`system.shutdown` 只在没有其他客户端时生效，其余情况返回当前
连接数。前端不再"谁拉起来谁杀掉"——两个窗口共用一个后端时，先关的那个不会打断
后关的那个。最后一个客户端断开后，后端在 8 秒宽限期（`MH_IDLE_EXIT_SEC` 可调）
后自行退出，所以前端崩溃也不会留下常驻进程。


`CharacterDetail` 形如 `{"character":Character,"snapshotCount":3,"lastRecordedAt":123456}`。
所有 JSON 字段均为 camelCase，与 `mhcore` 的 serde 配置一致。

## 前端分层（MVVM）

页面不再承担业务逻辑。分层是硬边界，不是建议：

```
MhRecorder.Core/                 不引用 WinUI，可脱离界面编译 / 测试
├─ Models.cs                     领域模型镜像（与 mhcore 一一对应）
├─ NamedPipeClient.cs            协议传输
├─ Services/
│  ├─ IAppService.cs             协议门面（Remote / Fake 两个实现）
│  ├─ BackendService.cs          当前用哪个后端 + 连接状态 + 重连
│  ├─ ImageImportService.cs      文件 / 剪贴板 / 窗口截图的归一化
│  ├─ RecognitionService.cs      识别 → 判定可用 → 归属 → 保存 → 回填身份
│  ├─ CharacterService.cs        角色库读写 + 搜索规则
│  ├─ ExportService.cs           导出 / 完整备份 + 选路径
│  └─ Changelog.cs               版本记录（release.ps1 从这里抽正文）
├─ Infrastructure/
│  ├─ NavigationService.cs       INavigationService + AppPage（外壳只认这个）
│  └─ DialogService.cs           IDialogService / IWindowPicker（问用户一句）
└─ ViewModels/                   CommunityToolkit.Mvvm（源生成器）
   ├─ ImportViewModel.cs         录入 + 审核工作区
   ├─ CharactersViewModel.cs     角色列表 / 搜索 / 翻页 / 删除
   ├─ AssetsViewModel.cs         资产汇总 + 环比文案
   ├─ SettingsViewModel.cs       设置 / 引擎信息 / 导出 / 备份
   ├─ MainViewModel.cs           外壳导航与状态栏
   ├─ CharacterCardViewModel.cs  卡片投影
   └─ FieldRowViewModel.cs       审核表格的一行（可改、可还原）

MhRecorder.App/                  只做"画"和"把平台能力接进来"
├─ Views/*.xaml(.cs)             绑定 + 手势 + 图表/轮播等视觉计算
├─ Views/Dialogs/                ContentDialog 子类（更新记录 / 窗口选择）
├─ Controls/ZoomableImageView    可缩放平移的截图 + OCR 框叠层
├─ Controls/CharacterCardView    角色卡片（UserControl，Border 是 sealed）
├─ Platform/                     WinUI 实现：文件选择器、剪贴板、窗口截图
└─ Services/AppHost.cs           组合根：唯一 new 服务与 ViewModel 的地方
```

约束（改代码前先看这几条）：

1. **Core 不得 `using Microsoft.UI.*`**。需要平台能力就加接口（`IImageSource`、
   `IFileSaveTarget`、`IDialogService`、`IWindowPicker`），在 `App/Platform` 实现。
2. **ViewModel 不发 UI 通知**：不 new 控件、不弹窗、不碰 `XamlRoot`。
3. **页面不写业务判断**：能不能保存、改过的值怎么入库、身份不明要不要拦，都在 VM。
4. 视觉计算（轮播三张卡的位置、柱状图坐标、缩放平移）留在视图，那是"怎么画"。

### 后端状态：三种，不许含糊

`BackendService.State` 是 "当前数据从哪来" 的唯一答案：

| 状态 | 触发条件 | 界面表现 |
|---|---|---|
| `Ready` | 命名管道握手成功 | 正常，无横幅 |
| `Demo` | **仅调试构建** + `MH_FORCE_DEMO=1` | 标题栏黄条「演示数据…不会落库」 |
| `Unavailable` | 正式构建连不上后端 | 红色横幅 + 识别/保存/导出拒绝执行 |

正式构建里 `MH_FORCE_DEMO` 会被**忽略并写日志**：一个环境变量能让发布版"看起来
在记录数据"是设计缺陷，不是方便。`Unavailable` 时 `Api` 是 `UnavailableAppService`，
每个方法都抛 `BackendUnavailableException`——返回空数据会被误读成"库是空的"。

启动器（`mhpack::launch`）是另一个关卡：后端进程起不来、或 20 秒内没建立管道，
就弹原生对话框说明原因（含日志路径与杀毒白名单提示）并退出，不再"照常拉起界面"。

调试开关（仅开发用）：

| 环境变量 | 作用 | 生效范围 |
|---|---|---|
| `MH_FORCE_DEMO=1` | 忽略命名管道，强制用内存演示后端 | **仅调试构建** |
| `MH_DEMO_IMAGE=<png>` | 演示模式下把这张截图垫在识别结果下面，便于调审核界面 | 仅调试构建 |

## 逐图结果与批次校验

`RecognitionResult` 的顶层 `stats` / `currencies` / `inventory` 是**合并视图**，
前提是这一批截图确实属于同一个角色。为了不让这个前提变成假设：

```rust
// mhvision::pipeline —— 顺序不能反
for img in images { scans.push(read_one(img)) }        // 每张图独立提取身份与数值
let mut plan = plan_batch(&identities);                // 纯函数，可单测
verify_unsigned_screens(&mut plan, &scans, refs, known); // 无ID的图再走一次视觉复核
let (result, _) = merge_included(&scans, &plan, …);     // 只合并 included
```

`ScreenResult`（`mhcore`）逐图保留：

| 字段 | 含义 |
|---|---|
| `screenIndex` / `screen` | 第几张图、哪类面板 |
| `character` | **只从这张图**读到的昵称/游戏ID/等级/门派/服务器 |
| `stats` / `currencies` / `equipment` / `inventory` | 这张图自己的读数 |
| `regions` | 这张图自己的审核框 |
| `included` / `excludedReason` | 是否并进了批次结果；没并进的原因 |

`batchIdentity` 有 `consistent` / `conflict` / `unidentified` 三种。冲突时前端在
证据区写明"本次只合并了 1/2 张截图"，并把被排除的图上的框从叠加层里去掉——
那些数字不属于这条记录。

## 身份安全：两道闸门

1. **识别层**：`plan_batch` 按面板写着的身份分组；无ID的图允许并入主组，但若库里
   的视觉证据明确指向**另一个**角色（游戏ID 不同），照样排除。
2. **存储层**（`mhstore::characters::resolve_target`）：`recognize.save` 是后端接口，
   不能依赖前端先问过用户。显式 id → 游戏ID 唯一 → 昵称唯一 → 否则拒绝或进待归属。

| 输入 | 结果 |
|---|---|
| 显式 `characterId` 存在 | 写入该行 |
| 显式 `characterId` 不存在 | `notFound`，**不新建** |
| 游戏ID 唯一 | 写入 |
| 游戏ID 多条 | `identityAmbiguous`，拒绝 |
| 昵称唯一 | 写入（`identity_confirmed` 仍为 0） |
| 昵称多条 | `identityAmbiguous`，拒绝 |
| 无任何身份 | 写 `pending_recognitions`，返回 `pending:true` |

`pending_recognitions` 存完整 `RecognitionResult`，`pending.assign` 时设
`character_id` 后**重放**一次 `save_recognition`：快照、指纹、外观、物品字典都走
同一条路径，不会因为"是补归属"而少记东西。归属成功才删行，失败留着可以重试。

## 识别审核工作区

识别结果里每个值都带一个像素框，前端据此把"数字错了"变成"这块像素读错了"。

`RecognitionResult.regions[]`（`mhcore::FieldRegion`）：

| 字段 | 含义 |
|---|---|
| `screenIndex` / `screen` | 属于批次里第几张图、哪类面板 |
| `key` | `hp` / `gold` / `nickname` / `item:金柳丹` / `feature:avatar` |
| `label` | 界面上的中文名（气血 / 金币 …） |
| `x,y,width,height` | 原图像素坐标（面板裁切偏移已加回） |
| `rawText` / `confidence` / `value` | 该框里的 OCR 原文、置信度、解析值 |
| `kind` | `field` / `identity` / `item` / `feature` |

坐标由 `mhvision::regions` 用字段的 `rawText` 反查 OCR 行得到——不是重新猜位置，
所以查不到就不画框，不会画错位置。前端 `ZoomableImageView` 按 1 像素 = 1 DIP
铺开图片与叠加层，缩放/平移只作用于外层变换，因此像素坐标可以直接当布局坐标用。

交互约定：

- 选中字段行 → 查看器缩放到该框（整图 27% 缩放下看不清数字，这是必须的）。
- 点图上的框 → 选中表格对应行并滚到可见。
- 识别完成后自动选中**置信度最低**的字段，先看最可能错的那一项。
- 缩放到 55% 以下隐藏框标签，只留框，避免标签糊成一团。

## 图像分析缓存

```
图片字节 → sha256 → "v{ANALYSIS_VERSION}:{hash}" → image_features.payload
    命中 → 反序列化 → dHash 复核 → 复用 OCR 行（跳过 OCR）
    未命中 → OCR → ImageAnalysis{version,dhash,hsvHist,lines} → 写回 → prune
```

* 版本常量在 `mhcore::ANALYSIS_VERSION`（mhvision 与 mhstore 都要看同一个数字，
  PING 的 `algorithmVersion` 与导出清单也用它）。改特征算法或换 OCR 模型就 +1。
* 缓存边界划在 OCR：`merge_adjacent_fragments`、字段提取、身份关联、审核框每次都重跑。
  所以"改了提取规则"不需要清缓存，"换了模型/特征算法"必须 +1。
* `ImageAnalysis::matches_image` 命中后用 dHash 复核；不一致按未命中处理。
* 写入失败只记日志（缓存是优化，不该让识别结果作废）；`MAX_IMAGE_FEATURES = 2000`。
* `BatchOutcome { result, cached_screens, computed }` 把"哪些命中、哪些新算"交给
  `mhservice`：流水线不碰存储，落库策略属于服务层。

## 窗口尺寸策略

| 场景 | 行为 |
|---|---|
| 常规桌面 | 按设计稿 1328×888 逻辑像素 |
| 小屏 / 工作区不足 | 取工作区 − 40 后的较大者，永不超出工作区 |
| 最小尺寸 | 1024×700（再小内容就挤不下，用 presenter 钳住） |
| 用户缩放 / 最大化 | 允许；内容按星号比例与 MinWidth 自适应 |
| 宽度 < 1180 | 侧边栏收成 76px 图标条，隐藏导航文字 |
| 高 DPI | 所有物理像素换算走 `GetDpiForWindow`，用 `SetWindowPos` 而非 `AppWindow.Resize` |

## 设计令牌（从设计稿采样）

| 令牌 | 值 | 用途 |
|---|---|---|
| `BrandBlue` | `#0074FF` | 主按钮、选中态、进度条、链接 |
| `BrandBlueHover` | `#0062D6` | 主按钮悬停 |
| `WindowBackground` | `#F5F5F7` | 窗口底色 |
| `SidebarBackground` | `#EBEBEE` | 左侧导航栏 |
| `CardBackground` | `#FFFFFF` | 卡片 |
| `CardBorder` | `#E5E6EB` | 卡片 1px 描边 |
| `TextPrimary` | `#1C1C1F` | 主文本 |
| `TextSecondary` | `#6B6F76` | 次要文本/说明 |
| `TextTertiary` | `#9AA0A6` | 占位/次要信息 |
| `HpGradientFrom` | `#E8443A` → `#FF8A4C` | 气血条 |
| `MpGradientFrom` | `#1E86FB` → `#63D2F0` | 魔法条 |
| `GoldAccent` | `#F6B82B` | 金币 |
| `SilverAccent` | `#8A93A5` | 银币 |
| `SuccessGreen` | `#1FB461` | 正向变化、置信度徽章 |
| `DangerRed` | `#E5484D` | 负向变化、清空数据 |
| `ChipBackground` | `#F2F3F5` | 来源徽章底色 |

字体：`Segoe UI Variable Text` / 回退 `Segoe UI`。圆角：卡片 12px，按钮 8px，徽章 6px。
间距基线 4px，卡片内边距 20px，页面边距 32px。

## 纯本地原则

- 不联网，不上传，不做任何云调用。
- 不使用 GPU，ONNX Runtime CPU EP 即可。
- 不使用视觉语言模型做常规识别；只用 PP-OCRv4 det+rec。
- 特征结果必须缓存，重复截图不重算。

## 协作边界

每个 teammate 只写自己 crate/目录下的文件。公共契约（`mhcore`）如需变更，
必须先在本文件登记并通知 Lead，不得各自私改。
