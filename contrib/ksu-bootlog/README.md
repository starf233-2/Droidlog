# Droidlog Boot Log（KernelSU 模块）

在**启动最早期**抢救内核崩溃日志，并持续落盘 logcat，供 Droidlog 读取与分析。

设备上 `/sys/fs/pstore` 为空、`/proc/last_kmsg` 不存在时，内核崩溃证据在 app 连上 adb 之前就消失了。这个模块以 root 身份在 `post-fs-data` 阶段（系统服务还没起来）把 pstore 拷走，从而补上这个窗口。

## 绝对的安全边界（先读这段）

本模块**不含 `system/` 目录**，不修改任何系统分区、不碰 boot 镜像、不改 sepolicy、不写属性。它能写的地方只有两处：

- `/data/adb/modules/droidlog_bootlog/` —— 模块自己的目录（KernelSU 标准位置，卸载即删）
- `/data/adb/droidlog/` —— 采集数据目录（本模块唯一拥有的数据目录），**0700 目录 / 0600 文件**

**没有** `mount` / `umount` / `resetprop` / `setprop` / `sepolicy` / `dd` 到块设备 / `mkfs` —— 这些才是变砖的来源，本模块一个都没有。打包脚本在构建时**逐项扫描并拒绝**包含这些的构建。

唯一与启动路径相关的风险是：`post-fs-data.sh` 会**阻塞启动**。因此该脚本被刻意写得很小：只有几个 `cat`，外部命令用 `timeout` 包裹（缺失时有回退），**没有循环、不联网、不等待**。

## 配置不会被当作代码执行（一次真实修复）

早期版本用 `. "$CONF"` 加载 `config.env`，那意味着**能写这个文件的人就是在写开机 root 脚本**：往配置里追加 `id > /tmp/x` 会在下次开机时以 root 执行；配置里一个引号不闭合还会让 `service.sh` 在启动 logcat 之前**静默退出**。

现在配置是**逐行按 key 解析**的（不是 source）：

- 不是 `KEY = value` 的行**一律忽略**，不会执行；
- 数值经**钳制**：例如 `BOOT_LINES` ∈ [200, 20000]，超长数字串在**任何算术之前**就按长度拒绝（`[ 99999999999999999999 -gt 1 ]` 在有些 shell 里是错误，这样的值也绝不该传给 logcat）；
- 缓冲区名**白名单**校验，未知名字被丢弃而不是传给 logcat；
- 配置坏掉**不会**阻止采集：解析失败就用默认值，采集照常进行。

真机验证（实测，针对当时引入该修复的构建）：植入 `id > /tmp/droidlog_pwned.txt` 与越界数值后，`ctl.sh confcheck` 把恶意行标为 `IGNORED`、并把越界值显示为**生效的钳制结果**，`/tmp/droidlog_pwned.txt` **不存在**；引号不闭合的配置下脚本仍 `exit=0`，采集照常进行。此后轮转相关的键已随"取消持续采集"移除（见下文），钳制规则随之改为作用在 `BOOT_LINES` 上。

## 采集了什么，放在哪

| 内容 | 路径 | 说明 |
| --- | --- | --- |
| **内核证据（Droidlog 直接读这里）** | `/data/adb/droidlog/kernel/pstore.txt` | 上次崩溃的内核日志；**每次开机先清空再写入**，空 = 上次是正常关机 |
| 内核证据 | `/data/adb/droidlog/kernel/last_kmsg.txt` | 内核还提供 `/proc/last_kmsg` 时 |
| 内核证据 | `/data/adb/droidlog/kernel/dmesg.txt` | 本次开机的内核日志 |
| 上一次崩溃的内核日志（历史） | `/data/adb/droidlog/boot/<时间戳>.pstore-*.txt` | 只在**异常重启后**有内容 |
| 上次内核日志（旧接口，历史） | `/data/adb/droidlog/boot/<时间戳>.last_kmsg.txt` | |
| 本次开机的内核日志（历史） | `/data/adb/droidlog/boot/<时间戳>.dmesg.txt` | |
| 开机期用户态日志 | `/data/adb/droidlog/boot/<时间戳>.logcat-boot.txt` | `crash`、`system`、`events` 三个缓冲最近 20000 行 |
| 本次开机属性 | `/data/adb/droidlog/boot/<时间戳>.props.txt` | `ro.boot.bootreason`、指纹、内核版本、uptime |
| 旧版遗留的持续落盘 | `/data/adb/droidlog/live/logcat.txt` | **本版本已不再采集**；这个文件是旧版留下的，可用 ▶ 按钮或 `ctl.sh purge` 清掉 |
| 模块自身日志 / 采集设置 | `logs/module.log`（截断到最后 400 行）、`config.env` | 排查与配置 |
| 最新快照指针 | `/data/adb/droidlog/boot/latest` | 里面是时间戳，app 可据此直接定位 |

**为什么内核证据单独放 `kernel/`**：Droidlog 的目录探针按 `ls -1` 顺序取**前 3 个文件**。把几百 KB 的 dmesg、数 MB 的 logcat 快照和几 KB 的 pstore 混在一起，字母序前三个恰好是大文件，**反而读不到 pstore** —— 而 pstore 才是这个模块存在的理由。`kernel/` 固定 3 个文件、每类一个、开机覆盖，**因此不需要删除任何东西就能保持正确**。

## 为什么不再持续采集 logcat

模块真正**不可替代**的只有两件事，都是"错过就永远拿不到"的：

1. **上一次开机的内核崩溃日志**（pstore/ramoops）—— 必须在 `post-fs-data` 抢在系统清理之前拷走；
2. **本次开机、电脑连上之前**的内核日志与用户态早期日志 —— 环形缓冲会把它转掉。

而**持续 logcat 不属于这一类**：桌面软件随时可以直接采，把它落在手机存储上只是占空间。所以本版本**没有后台采集**，`live/` 里若还有文件，那是旧版留下的。

想让开机快照也完全不占空间：把 `config.env` 里的 `BOOT_LOGCAT` 设为 `0` —— 内核证据（pstore/dmesg）照样采集。

配置键一览（`/data/adb/droidlog/config.env`：逐行按 key 解析，从不执行）：

| 键 | 默认 | 作用 |
| --- | --- | --- |
| `BUFFERS` | `crash system events` | 开机快照采哪些缓冲；名字白名单校验 |
| `BOOT_LOGCAT` | `1` | 是否拍开机快照；`0` = 零 logcat 存储 |
| `BOOT_LINES` | `20000` | 快照行数，钳制到 200-20000 |
| `FORMAT` | `threadtime` | 快照使用的 `logcat -v` 格式 |
| `PMSG` | `1` | 是否连同 pstore 的 pmsg 文件一起抢救（那是**上次开机**的用户态日志） |

`sh ctl.sh confcheck` 会显示这些键的原始值与**生效值**，并列出被忽略的行 —— 旧版的 `ROTATE_KB` / `ROTATE_COUNT` 会出现在那里，提示它们在本版本已无作用。

## 磁盘占用（有上限，且在启动循环里也生效）

- **没有持续采集**：本版本不在后台写任何日志流，所以没有会随时间增长的 logcat 文件；
- 快照保留：**最近 3 套开机记录**（一套 = 同一时间戳下的全部文件：meta、dmesg、logcat 快照、props 与抢救出的 pstore），旧的一整套删掉 —— 不是"保留 N 个文件"。散装计数可能留下旧开机的尾巴却丢掉最新开机的开头，而**残缺的记录比没有记录更糟，因为它看起来是完整的**；
- `kernel/` 里就是三个固定文件名、每次开机覆盖，因此永远是"最新那一套"，不会累积；
- `module.log` 截断到最后 400 行，`logcat.out` 最后 200 行；
- 写入大文件前检查 `/data` 剩余空间（dmesg 需 8 MB、logcat 快照需 16 MB、pstore 需 4 MB），不足就跳过并记一行日志；
- **清理不只在 `boot-completed.sh`**：`post-fs-data.sh` 与 `service.sh` 每次都会清理。启动循环可能永远到不了 boot-completed，而那正是本模块要抓的场景 —— 早期版本只在 boot-completed 清理，等于在循环里从不清理。

## 隐私：会记录什么

- 默认采集 `crash`、`system`、`events` 三个缓冲：**包含应用启动记录、包名与系统服务日志**；
- `radio`、`security` 在允许列表里，但**只有你在配置中显式加入才会采集**；
- 若内核启用了 pmsg 持久化，`/sys/fs/pstore/pmsg-ramoops-*` 里是**上一次开机的用户态 logcat**（可能含 `main`，与 `BUFFERS` 的选择无关）。模块会复制它 —— 想排除就把配置里的 `PMSG` 设为 `0`，此时这些文件会被跳过（`kernel/pstore.txt` 同样跳过）；
- 所有数据都在 `/data/adb/droidlog`，**0700 目录 / 0600 文件**，仅 root 可读。

## 安装

1. 用打包脚本生成 zip：
   `powershell -File "D:\new pjkt\droidlog\contrib\ksu-bootlog\tools\build-zip.ps1"`
   它会先跑五项检查（仅 LF、无 BOM、纯 ASCII、禁用命令/路径扫描、**每一处写目标必须在模块自己目录内**），任一不通过就**拒绝构建**。
2. KernelSU 管理器 → 模块 → 从本地安装 → 选择该 zip。
3. **重启设备**。（从旧版升级：新版第一次运行会停掉旧版遗留的 logcat 写入进程；**只运行脚本而不重装的话，下次开机旧版会把它重新启动** —— 因此要让改动持久，必须重新安装本 zip。）
4. 验证：`su` 后执行 `sh /data/adb/modules/droidlog_bootlog/ctl.sh status`。

## ▶ 动作按钮（KernelSU 左下角那个播放图标）

KernelSU 点击模块的 ▶ 会执行模块根目录下的 **`action.sh`** ✓。本模块的按钮只做一件事：**清空已采集的日志** ✓。

- 只删 `/data/adb/droidlog/` 里 `boot/`、`kernel/`、`live/` 下的**普通文件** ✓；该目录之外的任何东西都不写不删 ✓；
- **`config.env` 保留** ✓（它是设置而不是日志 ✓，删掉等于悄悄重置你的选择 ✓）；`module.log` 是**截断**而不是删除 ✓，以便下一行还记着按钮跑过 ✓；
- **符号链接跳过而不跟随** ✓，子目录也不递归 ✓ —— 有人放个链接进来，也不能把"删我的日志"变成"删别的东西" ✓；
- 会**先停掉旧版遗留的写入进程再删** ✓ —— 因为 logcat 正开着文件时删掉它**一个字节都不会释放** ✓（进程还在写那个已 unlink 的 inode ✓），而且文件会在下一次轮转时冒出来 ✓（本版本自己不再启动任何写入进程 ✓）；
- 输出会写进 KernelSU 的动作日志 ✓，包括释放了多少空间 ✓。

想**停止/永久关闭**而不是清空，用 `ctl.sh stop` / `disable` ✓。

沙箱实测记录（把 `OUT` 指向临时目录运行真实脚本 ✓，绝不碰真实数据 ✓）：删掉 3 个普通文件 ✓、`config.env` 与它的软链目标完好 ✓、软链本身保留 ✓、子目录内容未动 ✓、真实 `/data/adb/droidlog` 分毫未变 ✓。

## 手动控制

```
sh ctl.sh status      查看状态（进程、pstore、最新快照、日志尾部）
sh ctl.sh start       跑一次开机后的清理（本版本没有后台采集可启动）
sh ctl.sh stop        停掉旧版遗留的写入进程（若有）
sh ctl.sh restart     停掉遗留进程后再跑一次清理
sh ctl.sh snapshot    立即拍一次快照
sh ctl.sh confcheck   显示生效设置，并列出配置里被忽略的行
sh ctl.sh sizes       各目录占用了多少空间，/data 还剩多少
sh ctl.sh purge       删除全部采集数据（保留 config.env 与模块本身）
sh ctl.sh disable     停止并永久关闭（跨重启生效）
sh ctl.sh enable      取消关闭
```

`disable` 会在 `/data/adb/droidlog/.disabled` 放一个标记文件；所有脚本开头都检查它，因此**不卸载也能彻底停用**。

## 卸载

在 KernelSU 管理器里卸载即可。`uninstall.sh` 会停掉采集进程，**但不会删除 `/data/adb/droidlog/` 里的数据** —— 卸载正是可能还需要这些证据的时刻。

**请注意这一点**：数据留下之后，模块的保留期（上文的 30 文件 / 32 MB / 64 MB 上限）也**不再运行**，`live/` 里的轮转文件会一直留在那里。想清干净请先执行：

```
sh /data/adb/modules/droidlog_bootlog/ctl.sh purge
```

（或自行 `rm -rf /data/adb/droidlog` —— 这一步由你决定，不由脚本代做。）

## Droidlog 侧怎么读（已接线）

**1. 「模块」采集源（推荐）**

设备探测会通过 root 检查 `/data/adb/modules/droidlog_bootlog` 是否存在；检测到之后：

- 工具栏设备名旁亮起「**模块**」徽章（与 Recovery 徽章同一位置、同一做法）；
- 采集源列表里的「**模块**」变为可用，其命令依次读取 `kernel/` 下三个文件并打上 `== 文件名` 段头。

未检测到时，「模块」源仍会列出，并附原因「需要设备已刷入 Droidlog Boot Log 模块（KernelSU）」—— 列出并说明为什么不可用，比悄悄消失更好懂。

**2. 目录探针**

崩溃采集里的 `ksu-kernel-dir`、启动采集里的 `boot-ksu-dir` 指向 `/data/adb/droidlog/kernel`。三个文件名固定，所以即使列表被拒，`known_files` 回退路径也能精确读到它们（不像墓碑/Dropbox 那些名字只能猜）。

**必须用 Root 模式**：`/data/adb` 是 `drwx------ root root`，ADB（shell）模式下读不到 —— 探针如实报"无材料/被拒"，设备探测如实报"未安装"。两者都不是错误，只是如实结果。

**3. 手工读法**

```
cat /data/adb/droidlog/kernel/pstore.txt
cat /data/adb/droidlog/kernel/dmesg.txt
```

## 已知限制

- pstore 只在**异常重启**后才有内容；正常重启是空的，这是内核语义，不是模块故障。
- 部分内核未配置 ramoops，`/sys/fs/pstore` 根本不存在（`status` 显示 `absent`），此时只有 dmesg 快照与 logcat 落盘可用。
- `dmesg -c` 刻意不使用 —— 清空内核环形缓冲会把日志从设备上其它工具手里抢走。
- 开机日志历史固定为**最近 3 套**。这个数字是三个脚本里 `keep_newest_sets "$OUT/boot" 3 80` 的实参（三个脚本各自自包含，不共享配置文件解析代码 —— 启动路径上的脚本不该依赖另一个文件能否解析）。要改成别的份数，改这三处的 `3` 即可；需要保留很久的历史请先导出。
