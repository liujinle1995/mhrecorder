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
   ├─ MhRecorder.Core\  模型 + 管道客户端
   └─ MhRecorder.App\   WinUI 3 界面
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
| `ping` | `{}` | `{"status":"ready","version":"0.9.2","protocol":1}` |
| `version` | `{}` | `{"backend":"0.9.2","protocol":1,"models":"ppocrv4"}` |
| `recognize` | `{"images":[{"name":"a.png","data":"<base64>"}]}` | `RecognitionResult` |
| `recognize.save` | `{"result":RecognitionResult,"screens":["a.png"]}` | `{"characterId":"..."}` |
| `characters.list` | `{}` | `{"characters":[CharacterDetail...]}` |
| `characters.get` | `{"id":"..."}` | `CharacterDetail` |
| `characters.update` | `{"id":"...","nickname":"?","gameId":"?","level":?}` | `{"ok":true}` |
| `characters.confirm` | `{"id":"...","confidence":0.96}` | `{"ok":true}` |
| `characters.delete` | `{"id":"..."}` | `{"ok":true}` |
| `statistics.get` | `{"recentLimit":5}` | `{"summary":AssetSummary,"characters":[CharacterAssetBar...],"recent":[AssetPoint...]}` |
| `settings.get` | `{}` | `Settings` |
| `settings.set` | `Settings` | `{"ok":true}` |
| `export.json` | `{"path":"C:\\..."}` | `{"path":"...","bytes":123}` |
| `export.csv` | `{"path":"..."}` | `{"path":"...","bytes":123}` |
| `export.zip` | `{"path":"..."}` | `{"path":"...","bytes":123}` |
| `data.clear` | `{}` | `{"ok":true}` |

`CharacterDetail` 形如 `{"character":Character,"snapshotCount":3,"lastRecordedAt":123456}`。
所有 JSON 字段均为 camelCase，与 `mhcore` 的 serde 配置一致。

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
