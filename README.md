# Droidlog

**Android 日志采集与分析桌面应用** —— 一根数据线，把 logcat、内核日志、崩溃现场、启动日志、Recovery 日志收进同一个可过滤、可标记、可追溯、可导出的窗口。


> **Tauri 2 + Rust + React + TypeScript**（不使用 Electron）。安装包**内置 adb**，装完即用，不需要先配 Android SDK。
> 支持 **Windows / macOS / Linux**，三个平台各一条打包命令（见[快速开始](#快速开始)）。
> 界面遵循 Material Design 3：**零渐变**、低饱和配色，四套配色 × 浅色 / 深色 / 跟随系统。

---

## 目录

- [它解决什么问题](#它解决什么问题)
- [特性](#特性)
- [快速开始](#快速开始)
- [使用指南](#使用指南)
- [常见问题](#常见问题)
- [技术栈](#技术栈)
- [项目结构](#项目结构)
- [开发与测试](#开发与测试)
- [已知限制](#已知限制)
- [许可与致谢](#许可与致谢)

---

## 它解决什么问题

`adb logcat` 很原始，而真正的现场往往不在 logcat 里，或者等你反应过来已经错过了：

| 你想查的事 | 过去要怎么做 | Droidlog 里怎么做 |
| --- | --- | --- |
| 应用一打开就闪退，来不及抓 | 手动点采集时崩溃已经发生 | **监听应用启动**：填包名点一下，应用一启动立刻自动开始采集 |
| 只想看某个 App 的日志 | 手抄 PID，App 一重启 PID 就失效 | 输入**应用名 / 包名 / UID** 任一，或从列表按**中文真名**直接点选 |
| 应用为什么闪退 | `logcat -b crash` 翻半天，还要手动对齐时间 | 崩溃行自动打标，用「下一个崩溃」逐条跳 |
| 上次为什么自动重启 | 得记住不同内核版本该读 `/proc/last_kmsg` 还是 `/sys/fs/pstore` | 「启动日志」先 `uname -r` 判断内核世代，再决定读哪条路径 |
| 是内存不足还是看门狗 | 人肉在 dmesg 里搜关键词 | 崩溃分类器自动分成 8 个族 |
| 采了一轮却什么都没有 | 不知道是设备没崩、还是路径要 root | 采集报告逐条说明：不存在 / 为空 / 无权限 / 内核受限，并给出下一步 |
| 日志要给同事 | 控制台里拖选复制，格式乱 | 停止采集后点「导出」，写出带时间戳的 `.log` 并自动定位 |

---

## 特性

### 采集：6 个采集源

| 采集源 | 设备侧动作 | 需要 root | 备注 |
| --- | --- | --- | --- |
| **logcat** | `logcat -v threadtime -b main,system,crash --uid=<uid>` | 否 | 缓冲区按设备实际拥有的自动取舍 |
| **dmesg** | `dmesg` | 是 | 内核环形缓冲；`dmesg_restrict=0` 时无需 root |
| **kmsg** | `cat /dev/kmsg` | 是 | 结构化内核日志，带级别数字，比 dmesg 更完整 |
| **崩溃日志** | `logcat -b crash` + 事件缓冲 + 内核缓冲，再列目录读回 tombstone / dropbox / ANR | 否（读 `/data/...` 通常要 root） | 一次性探针采集，跑完即出报告 |
| **开机日志** | 内核快照 + 按间隔轮询直到启动完成 | 否 | 自动选 `logcat -b kernel`（免 root）或 `dmesg`；默认窗口 120 秒 |
| **Recovery 日志** | `/tmp/recovery.log`、`/cache/recovery/last_log`、`dmesg`、pstore | 否 | 仅设备处于 Recovery / Sideload 模式时可用 |

- **ADB / Root 双模式**：Root 走 `su -c`，启动时自动探测并显示徽章；探测对本地化输出免疫（中文 ROM 的 `用户id=0(root)` 也能识别）。
- **多设备 · 多采集源并行**：每 2 秒轮询热插拔，可同时跑多个会话并各自停止。
- **解析失败的行永不丢弃**：不匹配任何已知格式的行按原文保留并标记 —— 很多厂商私有格式正是靠这个才看得到。

### 应用真名解析（决定列表能不能用）

「应用名称」在 adb 里**拿不到**：`dumpsys package`、`pm dump`、`cmd package dump`、`dumpsys activity recents` 都只给 `labelRes`（一个资源 id），文字本体在 APK 的 `resources.arsc` 里 —— 在电脑上解析它意味着每个应用要拉几 MB。

Droidlog 换个问法：**让设备自己回答**。仓库里带一个 4 KB 的 dex（源码 `src-tauri/device-helper/LabelProbe.java`，产物 `labelprobe.dex`，`include_bytes!` 内嵌进二进制），运行时推送到 `/data/local/tmp` 并用 `app_process` 执行，它调用系统 `PackageManager.getApplicationLabel()`，一次打印全部应用：

```
包名 \t 真名 \t uid \t 是否系统应用
com.coolapk.market	酷安	10377	0
tv.danmaku.bili	哔哩哔哩	10369	0
com.tencent.mm	微信	10368	0
```

| 设计点 | 说明 |
| --- | --- |
| 一次调用拿全量 | 实测小米 MIX 4（Android 17）**438 个应用 / 1.9 秒**，含推送耗时 |
| 真名跟随系统语言 | 名字由设备解析，中文 ROM 就是「设置 / 酷安 / 微信」 |
| 免 root、免安装 | `app_process` 以 shell（uid 2000）身份运行即可 |
| 与架构无关 | 一份 dex 通吃 arm64 / arm32 / x86（对比：塞一个预编译 ELF 只能单架构） |
| 编译期零 Android 依赖 | Java 侧**纯反射**写，不需要 `android.jar`，dex 也不绑定 API 版本 |
| 内嵌进二进制 | 不需要额外资源文件，三平台一致 |
| 磁盘缓存 | 按设备序列号缓存 5 分钟；切「仅用户 / 含系统」不再读设备 |
| 失败有降级 | ROM 限制 `app_process` 时回落到包名推导（列表仍可用），原因写到控制台 |
| 并发安全 | 探针串行执行、临时文件按进程命名，不会互相踩；失败结果**不写缓存** |

版本 / 目标 SDK / 安装时间 / 启用状态来自**一次 `dumpsys package packages` 全量 dump**（对比：逐应用 `dumpsys` 要起几百个进程）。安装时间是设备本地时间原文，原样显示，不做时区换算。

### 从已安装应用中选择

目标应用卡片里点「从已安装应用中选择」展开：

- **真名 + 包名**两行显示，悬停可看 `UID · 版本 · 安装时间`；
- **搜索框**按应用名或包名即时过滤；
- **仅用户 / 含系统**一键切换（本地过滤，切换是瞬时的）；
- 点任意一行即把该应用设为目标（等效于「解析」）。

### 监听应用启动 → 自动采集

专治"一打开就崩"：填入应用名 / 包名 / UID，点「监听启动」后**每秒检查一次**，进程一出现立刻开始采集，并提示「检测到 X 已启动，正在开始采集…」。

- 采集目标已收窄到该应用（设备侧 `--uid=`），不会淹没在系统日志里；
- 应用消失后**重新武装**，下次启动会再抓一次；一次启动只采一次，不会重复开会话；
- 崩溃缓冲是常驻的，所以即使进程启动后立刻死亡，采集开始后依然能把那几条崩溃记录读出来。

### 运行中的应用

- **应用名（真名）+ 灰色包名**两行；悬停显示 `PID · UID`；
- 「刷新运行列表」手动刷新；「收起运行列表 / 展开运行列表」可折叠 —— 这是卡片里唯一会随进程数无限增长的部分（实测 78 行）；
- 说明：`ps` 里有些条目是**进程名而非包名**（`android.ext.services`、`android.process.media`），系统里没有对应应用，因此只能显示推导名。

### 查看与过滤

- **10 万行环形缓冲**：满了丢最旧并计数上报，不会被一次日志风暴吃光内存。
- **虚拟化表格 + 平滑跟随**：只渲染视口内约 40 行；跨批次滑行动画（`prefers-reduced-motion` 下退化为直接跳），贴底自动跟随、用户一滚即停。
- **实时过滤**：级别多选、TAG 包含/排除、消息关键字（可切正则）、时间窗口（最近 N 秒），以及可叠加的「高级规则」（字段 + 比较方式 + 值，全部 AND）。
- 过滤发生在**采集侧**：被过滤掉的日志不会进入缓冲。

### 崩溃识别与采集报告

- **8 个失败族**：内核 Panic、Oops、内核 BUG、看门狗、低内存杀进程、ANR、系统服务崩溃、Native 崩溃；
- **行级标记**：崩溃行带暖色徽标与左边线；「仅看崩溃」把 10 万行收成几条；「下一个崩溃」直接跳，也能从报告里定位；
- **探针式采集报告**：逐条列出读了什么、结果与原因，并给出可执行建议（例如「读取被拒绝：请切换到 Root 模式」）；
- **内核世代自适应**：内核 **≤3.4** 读 `/proc/last_kmsg`；**≥3.5**（pstore/ramoops）读 `/sys/fs/pstore` 下的 `console-ramoops` / `console-ramoops-0` / `pmsg-ramoops-0`。判断依据会写进报告，不会在 5.10 内核上去找早已被移除的 `/proc/last_kmsg`。

| 报告状态 | 含义 | 该做什么 |
| --- | --- | --- |
| 已找到 | 读到了内容 | —— |
| 为空 | 位置存在但没有内容 | 设备确实没产生这类日志（例如至今没崩过） |
| 不存在 | 该机型没有这个路径 | 换其它采集源，或确认机型与内核世代 |
| 无权限 | 读取被拒绝 | 切到 **Root** 模式重试 |
| 内核受限 | `dmesg_restrict=1` | 用 Root 模式，或 `echo 0 > /proc/sys/kernel/dmesg_restrict` |
| 失败 | 命令没执行成功 | 检查设备是否仍在线、adb 版本是否匹配 |

### 导出

- **按钮只在采集完成后出现**：它占的就是「全部停止」那一个槽位 —— 采集中显示 `全部停止`，采集完成变成 `导出`，未开始采集时两者都不显示（与「开始采集」同尺寸：32px 高、8px 圆角）。这个位置永远不会出现按了没用的按钮。
- **导出为文本 `.log`**：logcat threadtime 形状（`时间 PID TID 级别 标签: 消息`）；解析失败的行按原文写出，不重排。
- **落点**：`<下载目录>/Droidlog/droidlog-<时间戳>.log`，导出后在文件管理器中选中该文件（Windows）。
- **导出的是当前缓冲区**：上限 10 万行（更多时最旧的已在采集阶段被丢弃）。实测 10 万行 / 12.5 MB 约 1 秒。
- CSV / JSON 序列化器已实现并有单测（CSV 带 UTF-8 BOM、按 RFC 4180 转义），目前尚未接入界面。

### 界面

- Material Design 3（`material-expressive-react` + `@material/web`），四套配色（灰蓝 / 青瓷 / 靛紫 / 陶土）× 浅色 / 深色 / 跟随系统；
- **零渐变**（`background-image: none !important` 全局封死）、低饱和、无霓虹无玻璃拟态；层级用色阶 + 1px 描边表达；
- 三栏面板 + 圆角卡片契约；四套配色在深色模式下共用同一条亮度梯子；
- **只有日志可以选中复制**：界面文字不可选，避免整屏拖选把标题、芯片、统计数字一起带走；
- 中文字体随包内置（Roboto + Noto Sans SC），完全离线可用。

### 多平台与分发

| 平台 | 产物 | 打包脚本 |
| --- | --- | --- |
| Windows 10/11 x64 | `.msi`、NSIS `.exe`、便携目录 | `scripts\build-windows.ps1` |
| macOS | `.app`、`.dmg`（可出 Intel+ARM 通用包） | `bash scripts/build-macos.sh [universal]` |
| Linux | `.deb`、`.AppImage` | `bash scripts/build-linux.sh` |

三个平台都**内置各自的 `adb`**（`src-tauri/platform-tools*/`），由 `tauri.<platform>.conf.json` 的资源重命名统一落到包内 `platform-tools/adb`，因此 `locate.rs` 的探测逻辑三平台一致 —— 装完即用，不依赖系统里的 Android SDK。

---

## 快速开始

### 方式 A：直接用安装包

1. 运行安装包（Windows `.msi` / `.exe`，macOS `.dmg`，Linux `.deb` / `.AppImage`）；
2. 手机上打开 **开发者选项 → USB 调试**，用数据线连接，在手机弹窗里点「允许」；
3. 打开 Droidlog，左上「设备」卡片出现机型 / 序列号即连接成功；
4. 在「采集源」里选一个源（默认 `logcat`），点右上 **开始采集**；要采崩溃 / 开机 / Recovery 日志，用日志面板上方的 **崩溃日志 / 启动日志 / Recovery** 按钮；
5. 想按应用过滤：右栏「目标应用」输入**应用名 / 包名 / UID**，或点「从已安装应用中选择」按真名挑；怕应用一开就崩就先点「监听启动」。

便携版：Windows 解压后直接运行 `droidlog.exe`（与 `platform-tools\` 同目录）；macOS 把 `.app` 拖出来即可；Linux 给 AppImage 加可执行位后运行。

### 方式 B：从源码构建（Windows）

**前置条件**

| 项 | 要求 |
| --- | --- |
| 系统 | Windows 10 / 11 x64 |
| Rust | stable，**MSVC 工具链**（`rustup default stable-msvc`） |
| C++ 构建工具 | Visual Studio Build Tools（勾选「使用 C++ 的桌面开发」） |
| Node.js | 18+，包管理器用 **pnpm** |
| WebView2 | Windows 11 自带；Windows 10 需安装 Evergreen Runtime |

```powershell
git clone <你的仓库地址> droidlog
cd droidlog
pnpm install
pnpm tauri dev                                # 开发（Vite 热更新 + Rust 自动重建）
powershell -File scripts\build-windows.ps1    # 打包：MSI + NSIS + 便携目录，并打印产物哈希
```

### 方式 C：macOS / Linux 打包

安装包**必须在对应系统上构建**（macOS 的 `.app` / `.dmg` 需要 Xcode 工具链与 `hdiutil`；Linux 的 `.deb` / `.AppImage` 需要 Linux 环境），无法在 Windows 上交叉产出。所以每个平台一条命令：

```bash
# Linux
bash scripts/build-linux.sh      # → src-tauri/target/release/bundle/{deb,appimage}

# macOS
bash scripts/build-macos.sh             # 本机架构 → bundle/{macos,dmg}
bash scripts/build-macos.sh universal   # Intel + Apple silicon 通用包
```

脚本会自动检查 `cargo` / `pnpm`（macOS 还检查 `xcode-select`）、必要时给内置 `adb` 补可执行位、缺 `node_modules` 时自动安装，最后打印产物清单。

**Linux 系统依赖**（Debian / Ubuntu）：

```bash
sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Fedora 见 `scripts/build-linux.sh` 头部注释；运行 AppImage 需要 FUSE 2（`sudo apt install libfuse2`）。

### 三平台构建产物对照

```
Windows  src-tauri\target\release\bundle\msi\Droidlog_0.1.0_x64_en-US.msi
         src-tauri\target\release\bundle\nsis\Droidlog_0.1.0_x64-setup.exe
         dist\Droidlog-0.1.0-portable\            （exe + platform-tools）
macOS    src-tauri/target/release/bundle/macos/Droidlog.app
         src-tauri/target/release/bundle/dmg/Droidlog_0.1.0_*.dmg
Linux    src-tauri/target/release/bundle/deb/*.deb
         src-tauri/target/release/bundle/appimage/*.AppImage
```

### 设备准备与权限边界

- **必开**：开发者选项 → USB 调试；部分 ROM 还需打开「USB 调试（安全设置）」或「USB 安装」。
- **不 root 能拿到**：logcat 全部缓冲区、崩溃日志中的 logcat 部分、开机日志（走 `logcat -b kernel`）、**全部应用真名**（探针以 shell 身份运行，不需要 root）。
- **root 之后额外拿到**：`dmesg` 与 `/dev/kmsg`、`/data/tombstones`、`/data/system/dropbox`、`/sys/fs/pstore`。
- 设备处于 **Recovery / Sideload** 模式时，工具栏会显示徽标，此时只有「Recovery 日志」可用（Recovery 下没有 logcat）。

---

## 使用指南

```
┌───────────── 工具栏：设备 · 模式 · 采集源 · 统计 · 主题 · 开始采集 / 全部停止 / 导出 ─────────────┐
│  左栏：设备 / 采集源        │  中栏：日志表格 + 采集报告        │  右栏：过滤规则            │
│  · 设备卡片（型号/序列号）  │  · 仅看崩溃 / 下一个崩溃          │  · 目标应用（真名列表）    │
│  · 采集源列表（含可用性）   │  · 崩溃日志 / 启动日志 / Recovery  │  · 运行中的应用（可收起）  │
│  · 自定义命令（可选）       │  · 虚拟化日志行                   │  · 级别 / TAG / 关键字     │
│                            │  · 会话页脚（逐个停止）            │  · 时间范围 / 高级规则     │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

**常见场景对照**

| 我要什么 | 怎么做 |
| --- | --- |
| 实时看某个 App 的日志 | 右栏「目标应用」→「从已安装应用中选择」→ 点真名（或输入包名回车）→ 选 `logcat` → 开始采集 |
| 抓"一打开就崩"的应用 | 选中该应用 → 点 **监听启动** → 打开那个应用；1 秒内自动开始采集并提示 |
| 抓一次崩溃现场 | 日志面板上方点 **崩溃日志** → 看采集报告 → 点「仅看崩溃」或逐条「定位」 |
| 查上次为什么自动重启 | 点 **启动日志**（先判断内核世代，再读 pstore 或 last_kmsg） |
| 刷机失败 / Recovery 循环 | 手机进 Recovery，用 **Recovery 日志** 读 `/tmp/recovery.log` 与上次日志 |
| 只看 ERROR 以上 | 右栏「日志级别」只勾 E Error / F Fatal |
| 排除刷屏 TAG | 右栏 TAG 排除框填关键字（例如 `SurfaceFlinger`） |
| 只看最近 10 秒 | 右栏「时间范围」选 10s |
| 把这一轮日志发给同事 | 停止采集后点右上 **导出**，文件落在下载目录的 `Droidlog/` 里并自动定位 |
| 复制一段日志 | 在日志区拖选（界面其余文字不可选，设计如此） |

---

## 常见问题

**设备一直不出现？**
确认线支持数据传输（不是纯充电线）、手机上已点「允许 USB 调试」；`unauthorized` 需要在手机上重新确认授权。应用每 2 秒自动重扫，不需要重启。

**提示「未找到 adb」？**
按以下顺序查找，全部未命中才报错：`DROIDLOG_ADB` 环境变量 → 应用资源目录（安装包内置的 `platform-tools`）→ `ANDROID_HOME` / `ANDROID_SDK_ROOT` 下的 `platform-tools` → 系统 `PATH` → 各平台常见安装位置。手工指定：Windows `set DROIDLOG_ADB=D:\path\to\adb.exe`，macOS / Linux `export DROIDLOG_ADB=/path/to/adb`，然后重启应用。

**列表里是包名推导的名字，不是真名？**
说明真名探针这一轮没成功，应用已自动降级（列表仍可用）。开发模式下控制台会打印原因，形如 `[droidlog] 真名探针失败，本次回落到包名：…`；常见原因：ROM 限制 `app_process`、`/data/local/tmp` 不可写、adb 传输被中断。重试通常即可 —— 失败结果不会写缓存，所以不会"卡住"5 分钟。

**运行列表里有些行只有推导名？**
那是 `ps` 的**进程名而非包名**（`android.ext.services`、`android.process.media`、`android.hardware.audio.service` 等），系统里没有对应的已安装应用，因此没有真名可取。

**表格一直是空的，但显示「采集中」？**
先看右栏过滤：级别是否只勾了某些等级、TAG 排除是否命中、时间窗口是否过小、关键字是否写错。也可以在目标应用里点「清除目标」取消应用过滤（未选应用 = 不过滤）。

**采不到崩溃日志？**
「崩溃缓冲区为空」通常意味着设备自开机以来没有崩溃 —— 这是结论，不是故障。若 tombstone / dropbox 显示「无权限」，切 Root 模式再采一次。

**为什么界面文字选不中？**
这是有意的：只有日志区域可选中复制，避免整屏拖选把标题、芯片、统计数字一起带走。

**深色模式下背景是纯黑吗？**
不是。四套配色在深色模式下共用同一条亮度梯子（surface 约 OKLCH 19%），只各自带一点主色色相。

**「Recovery」按钮为什么是灰的？**
它只在设备处于 Recovery / Sideload 模式时可用，普通系统下没有 `recovery.log` 可读。

---

## 技术栈

| 层 | 选型 |
| --- | --- |
| 桌面容器 | Tauri 2（Rust 后端 + 系统 WebView，**不使用 Electron**） |
| 后端 | Rust：`tokio`、`serde`、`thiserror`；生产路径 `deny(unwrap_used, expect_used, panic, indexing_slicing)` |
| 前端 | React 19 + TypeScript（`strict` + `noUncheckedIndexedAccess` + `exactOptionalPropertyTypes`，**禁 `any`**） |
| 构建 | Vite 7、pnpm |
| 状态 | zustand |
| UI | `material-expressive-react` + `@material/web`（Material 3），自建 M3 配色引擎（OKLCH 色调板） |
| 字体 | Roboto（拉丁）+ Noto Sans SC（中文），随包内置 |
| 设备侧助手 | `src-tauri/device-helper/labelprobe.dex`（4 KB，纯反射 Java，经 `app_process` 运行） |

后端按职责分层，依赖只能向左：

```
error → executor → adb → device  ──┐
        └→ source → parser  ───────┼──→ process → commands
             └→ ring, filter  ─────┘        ↑
                  crash, collect, export ────┘
                  state（共享：会话 / 过滤器 / 采集报告 / 真名缓存）
```

---

## 项目结构

```
droidlog/
├── scripts/                   # 三平台打包脚本（Windows / macOS / Linux）
├── docs/                      # DESIGN.md（架构、契约与踩坑记录）
├── src/                       # 前端（React + TS）
│   ├── api/backend.ts         # 类型化 invoke 封装 + 错误归一化
│   ├── components/            # Toolbar / DevicePanel / LogTable / FilterPanel / CollectPanel
│   ├── lib/                   # 展示层格式化、崩溃标签、导出序列化、shadow DOM 修补
│   ├── store/useAppStore.ts   # zustand store（会话、记录、崩溃、报告、真名列表、主题）
│   ├── styles/                # tokens / global / material-bridge / app / collect
│   ├── theme/                 # M3 配色引擎（OKLCH 色调板 + 明暗）
│   └── types/index.ts         # Rust 类型的 TS 镜像
└── src-tauri/                 # 后端（Rust）
    ├── device-helper/         # LabelProbe.java + labelprobe.dex（应用真名探针）
    ├── platform-tools/        # Windows 版 adb（含 NOTICE.txt）
    ├── platform-tools-mac/    # macOS 版 adb
    ├── platform-tools-linux/  # Linux 版 adb
    ├── tauri*.conf.json       # 基础配置 + 各平台资源/目标覆盖
    └── src/
        ├── adb/               # adb 定位（三平台）、守护进程生命周期
        ├── executor/          # ADB / Root（su -c）执行、命令规划、无窗口 spawn
        ├── device/            # 设备发现、探测、目标应用解析、运行列表、真名列表
        ├── source/            # 采集源抽象（6 个源 + 自定义命令）
        ├── parser/            # logcat / 内核日志解析（失败行保留）
        ├── filter/ ring/      # 过滤引擎、环形缓冲
        ├── crash/             # 崩溃分类器（8 个族）
        ├── collect/           # 探针式采集：崩溃 / 启动 / Recovery + 设备能力探测
        ├── export/            # 导出落盘、文件名清洗、打开所在文件夹
        ├── process/           # 会话生命周期与流式读取
        ├── state/ commands/   # 进程内共享状态、Tauri 命令边界
        └── types.rs           # 对外类型再导出中心
```

---

## 开发与测试

```powershell
pnpm tauri dev                  # 开发（Vite 热更新 + Rust 自动重建）
npm run typecheck               # 前端类型检查（严格模式）
npm run build                   # 只构建前端产物

cd src-tauri
cargo test --lib                # 单元测试（解析器、过滤器、崩溃分类、采集探针、真名解析、导出）
cargo clippy --all-targets      # 零警告是硬要求
```

当前状态：`cargo test --lib` **276 passed**、`cargo clippy --all-targets` **0 warning**、`tsc` **0 error**。

调试时给 WebView 打开 DevTools 端口（仅 Windows 需要）：

```powershell
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222"; pnpm tauri dev
```

**改真名探针**时注意两点（都踩过）：`ActivityThread.systemMain()` 之前必须先 `Looper.prepareMainLooper()`，否则报 `Can't create handler inside thread … that has not called Looper.prepare()`；用 d8 编译时必须把**整个 classes 目录打成 jar** 再喂给它（匿名内部类是独立 class，只传外层会 `NoClassDefFoundError`），且 r8 9.x 不接受目录作为输入：

```powershell
javac --release 8 -d classes LabelProbe.java
jar cf probe.jar -C classes .
java -cp r8.jar com.android.tools.r8.D8 --min-api 24 --no-desugaring --output out probe.jar
# out/classes.dex → src-tauri/device-helper/labelprobe.dex（约 4 KB）
```

---

## 已知限制

- 导出目前只提供**文本 `.log`**（CSV / JSON 序列化已实现并有测试，尚未接入界面）；也不提供"只导出选中行 / 只导出当前过滤视图"。
- **不抓应用图标**（真名探针只取文字；图标需要另走 APK 资源解析）。
- **运行列表中非包名的进程行没有真名**（系统里不存在对应应用）。
- 真名探针依赖 `app_process`，个别 ROM 上会被限制；此时整列表回落到包名推导（功能可用，只是名字不好看）。
- 导出后自动定位文件目前只在 Windows 实现（macOS / Linux 上文件正常写出，但不会自动打开文件管理器）。
- macOS / Linux 的配置与脚本已就绪，但**尚未在两平台真机验证**。
- 未做 `bugreport` 一体化采集与 `adb pull` 落盘（读文件用 `cat`，等价但不在主机留副本）。
- 老设备（Android 4.x 或非标准 shell）只做了防御性处理，未逐一验证。

---

## 许可与致谢

- 本项目源码：Apache-2.0。
- `platform-tools*/`（`adb`、`adb.exe`、`AdbWinApi.dll` 等）来自 Google Android SDK Platform-Tools，按其随附 `NOTICE.txt` 分发。
- 界面组件：[`material-expressive-react`](https://www.npmjs.com/package/material-expressive-react) 与 [`@material/web`](https://github.com/material-components/material-web)（Apache-2.0）。
- 字体：[Roboto](https://fonts.google.com/specimen/Roboto) 与 [Noto Sans SC](https://fonts.google.com/noto/specimen/Noto+Sans+SC)（SIL Open Font License 1.1）。
