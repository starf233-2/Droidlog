# Droidlog

**Android 日志采集与分析桌面应用** —— 一根数据线，把 logcat、内核日志、崩溃现场、启动日志、Recovery 日志收进同一个可过滤、可标记、可追溯的窗口。

![Droidlog 主界面](docs/images/droidlog-main.png)

> **Tauri 2 + Rust + React + TypeScript**。安装包内置 `platform-tools`，装完即用，不需要先配 Android SDK。
> 界面遵循 Material Design 3：**零渐变**、低饱和配色，四套配色 × 浅色/深色/跟随系统。

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

`adb logcat` 很原始，而真正的现场往往不在 logcat 里：

| 你想查的事 | 过去要怎么做 | Droidlog 里怎么做 |
| --- | --- | --- |
| 应用为什么闪退 | `logcat -b crash` 翻半天，还要手动对齐时间 | 点「崩溃日志」，崩溃行自动打标，用「下一个崩溃」逐条跳 |
| 上次为什么自动重启 | 得记住不同内核版本该读 `/proc/last_kmsg` 还是 `/sys/fs/pstore` | 点「开机日志」：它先 `uname -r` 判断内核世代，再决定读哪条路径 |
| 是内存不足还是看门狗 | 人肉在 dmesg 里搜关键词 | 崩溃分类器自动分成 8 个族（内核 Panic / Oops / 内核 BUG / 看门狗 / 低内存 / ANR / 系统服务 / Native） |
| 采了一轮却什么都没有 | 不知道是设备没崩、还是路径要 root | 采集报告逐条说明：不存在 / 为空 / 无权限 / 内核受限，并给出下一步 |
| 只想看某个 App 的日志 | 手抄 PID，App 一重启 PID 就失效 | 输入包名即可，UID 下推到设备侧（重启不失效），每 30 秒自动跟随 |

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

### 设备与目标应用

- **ADB / Root 双模式**：Root 走 `su -c`，启动时自动探测并显示徽章；探测对设备本地化输出免疫（中文 ROM 的 `用户id=0(root)` 也能识别）。
- **目标应用跟随**：PID / 包名 / UID 任一即可。优先把 **UID 下推**到设备侧（`logcat --uid=`，App 重启后依然有效），并在主机侧做身份校验；每 30 秒重新解析，App 被杀或重启会自动更新或提示「应用未开启或不存在」。
- **多设备 · 多采集源并行**：每 2 秒轮询热插拔，可同时跑多个会话并各自独立停止。

### 查看与过滤

- **10 万行环形缓冲**：满了丢最旧并计数上报，不会因为一次日志风暴把内存吃光。
- **虚拟化表格 + 平滑跟随**：只渲染视口内约 40 行；滚动用跨批次滑行动画（`prefers-reduced-motion` 下退化为直接跳），贴底自动跟随、用户一滚即停。
- **实时过滤**：级别多选、TAG 包含/排除、消息关键字（可切正则）、时间窗口（最近 N 秒），以及可叠加的「高级规则」（字段 + 比较方式 + 值，全部 AND）。
- **解析失败的行永不丢弃**：不匹配任何已知格式的行按原文保留并标记——很多厂商私有格式正是靠这个才看得到。

### 崩溃识别与采集报告

- **8 个失败族**：内核 Panic、Oops、内核 BUG、看门狗、低内存杀进程、ANR、系统服务崩溃、Native 崩溃。
- **行级标记**：「仅看崩溃」把 10 万行收成几条；崩溃行带暖色徽标与左边线，「下一个崩溃」直接跳，也能从报告里点「定位」。
- **探针式采集报告**：每次采集逐条列出读了什么、结果与原因，并给出可执行建议（例如「读取被拒绝：请切换到 Root 模式」）。
- **内核世代自适应**：用 `uname -r` 判断——内核 **≤3.4** 读 `/proc/last_kmsg`；**≥3.5**（pstore/ramoops）读 `/sys/fs/pstore` 下的 `console-ramoops` / `console-ramoops-0` / `pmsg-ramoops-0`。判断依据会写进报告，不会在 5.10 内核上去找早已被移除的 `/proc/last_kmsg`。

### 界面

- Material Design 3（`material-expressive-react` + `@material/web`），四套配色（灰蓝 / 青瓷 / 靛紫 / 陶土）× 浅色 / 深色 / 跟随系统。
- **零渐变**（`background-image: none !important` 全局封死）、低饱和、无霓虹无玻璃拟态；层级用色阶 + 1px 描边表达。
- 三栏面板 + 圆角卡片契约；四套配色在深色模式下共用同一条亮度梯子。
- **只有日志可以选中复制**：界面文字不可选，避免整屏拖选把标题、芯片、统计数字一起复制走。
- 中文字体随包内置（Roboto + Noto Sans SC），完全离线可用。

### 分发

- **MSI / NSIS 安装包内置 `platform-tools`**：装完直接能用，无需机器上已有 adb。
- **便携版**：`droidlog.exe` + `platform-tools/` 同目录，拷走即用。

---

## 快速开始

### 方式 A：直接用打包好的版本

1. 运行 `Droidlog_0.1.0_x64-setup.exe`（或 `.msi`）安装；
2. 手机上打开 **开发者选项 → USB 调试**，用数据线连接电脑，在手机弹窗里点「允许」；
3. 打开 Droidlog，左上「设备」卡片出现机型 / 序列号即连接成功；
4. 在「采集源」里选一个源（默认 `logcat`），点右上 **开始采集**；要采崩溃 / 开机 / Recovery 日志，用日志面板上方的 **崩溃日志 / 启动日志 / Recovery** 按钮。

便携版：解压后直接运行 `droidlog.exe`（保持它与 `platform-tools/` 在同一目录），无需安装。

### 方式 B：从源码构建

**前置条件**

| 项 | 要求 |
| --- | --- |
| 系统 | Windows 10 / 11 x64 |
| Rust | stable，**MSVC 工具链**（`rustup default stable-msvc`） |
| C++ 构建工具 | Visual Studio Build Tools（勾选「使用 C++ 的桌面开发」） |
| Node.js | 18+，包管理器用 **pnpm** |
| WebView2 | Windows 11 自带；Windows 10 需安装 Evergreen Runtime |

**构建**

```powershell
git clone <你的仓库地址> droidlog
cd droidlog

pnpm install            # 安装前端依赖（含 Tauri CLI）
pnpm tauri dev          # 开发模式：热重载，窗口自动打开
pnpm tauri build        # 打包：产出 MSI 与 NSIS 安装包
```

打包产物：

```
src-tauri/target/release/droidlog.exe                             # 主程序
src-tauri/target/release/bundle/msi/Droidlog_0.1.0_x64_en-US.msi
src-tauri/target/release/bundle/nsis/Droidlog_0.1.0_x64-setup.exe
```

想做绿色便携版，把 `droidlog.exe` 与 `src-tauri/platform-tools/` 复制到同一个目录即可。

**质量门禁（可选）**

```powershell
npm run typecheck                 # 前端严格 TS（禁 any）
cd src-tauri
cargo test --lib                  # 单元测试
cargo clippy --all-targets        # 生产路径禁 unwrap / expect / panic / 切片索引
```

> 仓库自带 `platform-tools/`（Windows 版 adb，随包附带其 `NOTICE.txt`）。
> 根目录 `.npmrc` 指向国内 npm 镜像；在墙外可删除该文件或改用官方源。

### 设备准备与权限边界

- **必开**：开发者选项 → USB 调试；部分 ROM 还需打开「USB 调试（安全设置）」或「USB 安装」。
- **不 root 能拿到**：logcat 的全部缓冲区、崩溃日志中的 logcat 部分、开机日志（走 `logcat -b kernel`）。
- **root 之后额外拿到**：`dmesg` 与 `/dev/kmsg`、`/data/tombstones`（墓碑文件）、`/data/system/dropbox`、`/sys/fs/pstore`。
- 设备处于 **Recovery / Sideload** 模式时，应用会自动识别并在工具栏显示徽标，此时只有「Recovery 日志」可用（Recovery 下没有 logcat）。

---

## 使用指南

界面分三栏：

```
┌────────────────── 工具栏：设备 · 模式 · 采集源 · 统计 · 主题 · 开始采集 ──────────────────┐
│  左栏：设备 / 采集源      │  中栏：日志表格 + 采集报告        │  右栏：过滤规则          │
│  · 设备卡片（型号/序列号）│  · 仅看崩溃 / 下一个崩溃          │  · 目标应用(PID/包名/UID)│
│  · 采集源列表（含可用性） │  · 崩溃日志 / 启动日志 / Recovery  │  · 日志级别              │
│  · 自定义命令（可选）     │  · 虚拟化日志行                   │  · TAG / 关键字 / 时间窗 │
│                          │  · 会话页脚（逐个停止）            │  · 高级规则（AND）       │
└────────────────────────────────────────────────────────────────────────────────────┘
```

**常见场景对照**

| 我要什么 | 怎么做 |
| --- | --- |
| 实时看某个 App 的日志 | 右栏「目标应用」输入包名 → 回车解析 → 选 `logcat` → 开始采集 |
| 抓一次崩溃现场 | 日志面板上方点 **崩溃日志** → 看采集报告 → 点「仅看崩溃」或逐条「定位」 |
| 查上次为什么自动重启 | 点 **启动日志**（它会先判断内核世代，再读 pstore 或 last_kmsg） |
| 刷机失败 / Recovery 循环 | 手机进 Recovery，用 **Recovery 日志** 读 `/tmp/recovery.log` 与上次日志 |
| 只看 ERROR 以上 | 右栏「日志级别」只勾 E Error / F Fatal |
| 排除刷屏 TAG | 右栏 TAG 排除框填关键字（例如 `SurfaceFlinger`） |
| 只看最近 10 秒 | 右栏「时间范围」选 10s（按记录到达本机的时间计算） |
| 复制一段日志给别人 | 在日志区拖选（界面其余文字不可选，设计如此） |

**采集报告怎么读**

| 状态 | 含义 | 该做什么 |
| --- | --- | --- |
| 已找到 | 读到了内容 | —— |
| 为空 | 位置存在但没有内容 | 设备确实没产生这类日志（例如至今没崩过） |
| 不存在 | 该机型没有这个路径 | 换其它采集源，或确认机型与内核世代 |
| 无权限 | 读取被拒绝 | 切到 **Root** 模式重试 |
| 内核受限 | `dmesg_restrict=1` | 用 Root 模式，或 `echo 0 > /proc/sys/kernel/dmesg_restrict` |
| 失败 | 命令没执行成功 | 检查设备是否仍在线、adb 版本是否匹配 |

---

## 常见问题

**设备一直不出现？**
确认线支持数据传输（不是纯充电线）、手机上已点「允许 USB 调试」；`unauthorized` 需要在手机上重新确认授权。应用每 2 秒自动重扫，不需要重启。

**提示「未找到 adb」？**
应用按以下顺序查找 adb，全部未命中才会报错：

1. 环境变量 `DROIDLOG_ADB`（指定即用它，最高优先级）
2. 应用资源目录（安装版 / 便携版内置的 `platform-tools`）
3. `ANDROID_HOME` / `ANDROID_SDK_ROOT` 下的 `platform-tools`
4. 系统 `PATH`
5. 常见安装位置（`%LOCALAPPDATA%\Android\Sdk\platform-tools` 等）

手工指定：`set DROIDLOG_ADB=D:\path\to\adb.exe` 后重启应用即可。

**表格一直是空的，但显示「采集中」？**
先看右栏过滤：级别是否只勾了某些等级、TAG 排除是否命中、时间窗口是否过小、关键字是否写错。也可以在目标应用里点「清空」取消应用过滤（未选应用 = 不过滤）。

**采不到崩溃日志？**
「崩溃缓冲区为空」通常意味着设备自开机以来没有崩溃——这是结论，不是故障。若 tombstone / dropbox 显示「无权限」，切 Root 模式再采一次。

**为什么界面文字选不中？**
这是有意的：只有日志区域可选中复制，避免整屏拖选把标题、芯片、统计数字一起带走。

**深色模式下背景是纯黑吗？**
不是。四套配色在深色模式下共用同一条亮度梯子（surface 约 OKLCH 19%），只各自带一点主色色相，不会出现某套配色明显更黑。

**「Recovery」按钮为什么是灰的？**
它只在设备处于 Recovery / Sideload 模式时可用，普通系统下没有 `recovery.log` 可读。

---

## 技术栈

| 层 | 选型 |
| --- | --- |
| 桌面容器 | Tauri 2（Rust 后端 + WebView2 前端，**不使用 Electron**） |
| 后端 | Rust：`tokio`、`serde`、`thiserror`；生产路径 `deny(unwrap_used, expect_used, panic, indexing_slicing)` |
| 前端 | React 19 + TypeScript（`strict` + `noUncheckedIndexedAccess` + `exactOptionalPropertyTypes`，**禁 `any`**） |
| 构建 | Vite 7、pnpm |
| 状态 | zustand |
| UI | `material-expressive-react` + `@material/web`（Material 3），自建 M3 配色引擎（OKLCH 色调板） |
| 字体 | Roboto（拉丁）+ Noto Sans SC（中文），随包内置 |

后端按职责分层，依赖只能向左：

```
error → executor → adb → device  ──┐
        └→ source → parser  ───────┼──→ process → commands
             └→ ring, filter  ─────┘        ↑
                  crash, collect ────────────┘
                  state（共享：会话 / 过滤器 / 采集报告）
```

---

## 项目结构

```
droidlog/
├── src/                       # 前端（React + TS）
│   ├── api/backend.ts         # 类型化 invoke 封装 + 错误归一化
│   ├── components/            # Toolbar / DevicePanel / LogTable / FilterPanel / CollectPanel
│   ├── lib/                   # 展示层格式化、崩溃标签、shadow DOM 修补
│   ├── store/useAppStore.ts   # zustand store（会话、记录、崩溃、报告、主题）
│   ├── styles/                # tokens / global / material-bridge / app / collect
│   ├── theme/                 # M3 配色引擎（OKLCH 色调板 + 明暗）
│   └── types/index.ts         # Rust 类型的 TS 镜像
└── src-tauri/                 # 后端（Rust）
    ├── platform-tools/        # 随包分发的 Windows 版 adb（含 NOTICE.txt）
    └── src/
        ├── adb/               # adb 定位、守护进程生命周期
        ├── executor/          # ADB / Root（su -c）执行、命令规划
        ├── device/            # 设备发现、探测、目标应用解析
        ├── source/            # 采集源抽象（6 个源 + 自定义命令）
        ├── parser/            # logcat / 内核日志解析（失败行保留）
        ├── filter/ ring/      # 过滤引擎、环形缓冲
        ├── crash/             # 崩溃分类器（8 个族）
        ├── collect/           # 探针式采集：崩溃 / 启动 / Recovery + 设备能力探测
        ├── process/           # 会话生命周期与流式读取
        ├── state/ commands/   # 进程内共享状态、Tauri 命令边界
        └── types.rs           # 对外类型再导出中心
```

---

## 开发与测试

```powershell
pnpm tauri dev                 # 开发（Vite 热更新 + Rust 自动重建）
npm run typecheck              # 前端类型检查
npm run build                  # 只构建前端产物

cd src-tauri
cargo test --lib               # 单元测试（解析器、过滤器、崩溃分类、采集探针…）
cargo clippy --all-targets     # 零警告是硬要求
cargo tauri build              # 打包安装包
```

调试时可用 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222` 打开 WebView2 的 DevTools 端口。

---

## 已知限制

- **仅 Windows**：内置的 `platform-tools` 是 Windows 版。Tauri 本身跨平台，但本项目未在 macOS / Linux 上验证。
- 暂**不支持导出**（文本 / CSV / JSON）。
- 未做 `bugreport` 一体化采集与 `adb pull` 落盘（读文件用 `cat`，等价但不在主机留副本）。
- 老设备（Android 4.x 或非标准 shell）只做了防御性处理，未逐一验证。

---

## 许可与致谢

- Apache-2.0许可。
- `platform-tools/`（adb.exe、AdbWinApi.dll 等）来自 Google Android SDK Platform-Tools，按其随附 `NOTICE.txt` 分发。
- 界面组件：[`material-expressive-react`](https://www.npmjs.com/package/material-expressive-react) 与 [`@material/web`](https://github.com/material-components/material-web)（Apache-2.0）。
- 字体：[Roboto](https://fonts.google.com/specimen/Roboto) 与 [Noto Sans SC](https://fonts.google.com/noto/specimen/Noto+Sans+SC)（SIL Open Font License 1.1）。
