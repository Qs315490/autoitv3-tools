# au3-cli — `au3` 命令行工具

**`au3` 命令行工具**：解析、规范化、反混淆、求值、运行、调试、解包——workspace 各库 crate 的装配层（clap 参数、子命令 dispatch、退出码）。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    au3-cli/                # CLI 二进制 crate（产物名为 au3，使用 clap 解析参数）
      src/
        main.rs     入口：定语言 → i18n_cli::parse() → dispatch → 把 CliError 转成退出码
        cli.rs      顶层 Cli / Command 定义（子命令、别名、缩写开关、--lang）
        args.rs     共用参数类型（-o 输出、-I 搜索路径）、CliError、输入加载
        i18n_cli.rs 语言选择（--lang 预扫描）+ 把 clap 的帮助/用法错误过一遍译文表
        tests_i18n.rs  （测试用）帮助文本与 tr()/msg!() 键的覆盖率守卫
        elevate.rs   #RequireAdmin：判断指令、起提权副本、说明为什么不提权
        output.rs   输出目标：文件或 stdout（`-` 表示 stdout）
        commands/
          mod.rs        子命令模块与 dispatch 表
          parse.rs      au3 parse（ParseArgs + run）
          pretty.rs     au3 pretty（PrettyArgs + run）
          deobfuscate.rs au3 deobfuscate（DeobfuscateArgs + run，含 --evaluate）
          evaluate.rs   au3 evaluate（EvaluateArgs + run）
          run.rs        au3 run（RunArgs + run，含 --trace 用的 Debugger 示例实现）
          debug.rs      au3 debug（DebugArgs + 交互式 shell：命令解析、步进策略、提示符、Tab 补全）
          unpack.rs     au3 unpack（UnpackArgs + run：--script/--raw/--table/--at）
      tests/
        debug.rs     端到端驱动真实二进制：断点/单步/条件/求值/重启/stdin/trace 过滤（46 项）
        run.rs       FILE [FUNC] 的参数顺序、`@Compiled` 覆盖（7 项）
        steps.rs     --max-steps / --no-progress 的端到端校验
        input.rs     FILE 走源码路径 / 编译产物路径 / 两者都不是
        includes.rs  #include 的端到端行为（引号/尖括号搜索顺序、缺失、--no-includes）
        require_admin.rs  #RequireAdmin 的判断、--no-elevate/--deny spawn、无提权机制的主机
        unit/args.rs 输入加载器的构建识别（`#[path]` 回挂进 src/args.rs）
        unit/debug_completion.rs Tab 补全的候选集与分词（`#[path]` 回挂进 commands/debug.rs）
```

## 使用

CLI 采用**子命令**形式（一级参数不带 `--` 前缀），解析由 [clap](https://crates.io/crates/clap) 完成，
因此自带 `--help` / `--version`、**子命令缩写**与**别名**：

```bash
cargo build --release
au3 --help                                 # 查看全部命令

# 输入既可以是 .au3 源码，也可以是编译产物（aut2exe 的 .exe / 裸 AU3!EA05|EA06 chunk）：
# 按文件头识别，用 autoitv3-unpack 把编译进去的脚本读回来，并把该产物本身当资源镜像
au3 parse build.exe
au3 debug build.exe                        # 源码视图就是解出来的脚本
au3 deobf build.exe --evaluate -o clean.au3
# 产物输入时 @Compiled = 1，.au3 输入时 = 0（脚本据此选重开 x64 / 剥离命令行等分支）；
# 同理 @AutoItX64 按构建回答：产物看 PE 头，.au3 看 #AutoIt3Wrapper_UseX64，
# 都没有才回落到 --win-arch 选的机器；@Unicode 恒为 1（AutoIt 3.3.14 起没有 ANSI 版）。
# 两个方向都能用 --compiled / --no-compiled 强制，方便拿解出来的源码和产物对照：
au3 debug deobf.au3 --compiled             # 源码按"编译产物"跑，走产物那一侧分支

# parse：只做解析与统计（顶层条目数、函数数）
au3 parse some.au3

# pretty：规范化重打印（默认保留注释、统一缩进）——反混淆输出基础
au3 pretty some.au3

# deobfuscate：常量折叠 + 函数表解析 + 间接调用简化 + 去注释
#              统计信息走 stderr，stdout 保持为干净的 AutoIt 源码
au3 deobfuscate some.au3                  # 默认保留原始变量/函数名
au3 deobfuscate some.au3 --rename         # 额外做确定性重命名（$l_str_003 / f042）

# -o FILE 将输出写入文件；-o - 或省略 -o 则输出到 stdout（原文件永不被修改）
au3 pretty      some.au3 -o out.au3
au3 deobfuscate some.au3 -o -

# evaluate：跑一遍脚本主体，把它运行时算出来的表值内联回源码
#           （唯一能解开字符串表的途径；撞到平台边界时会报告并保留已求出的值）
au3 evaluate some.au3 -o resolved.au3
au3 evaluate some.au3 --faithful          # 分析默认不写盘；这一条按 AutoIt 语义真跑
# 也可以一步到位：先求值再做常规反混淆
au3 deobfuscate some.au3 --evaluate -o clean.au3

# 选定仿真的 Windows 系统版本（非 Windows 主机默认 win10；见 [`autoitv3-platform`](../autoitv3-platform/README.md) 的「Windows 仿真」）
au3 evaluate some.au3 --win-version win11 -o resolved.au3
au3 evaluate some.au3 --win-version win7 --win-arch x86 -o resolved.au3
au3 evaluate some.au3 --no-win-emu        # 关掉仿真，停在第一个 Windows 调用

# run：执行整个脚本，或调用其中一个函数
#      --cmdline 始终是脚本的 $CmdLine/$CmdLineRaw；--arg 是函数入参，
#      没写函数时也归入 $CmdLine
au3 run some.au3                          # 不写函数 = 执行整个脚本体（按 AutoIt 语义真跑）
au3 run some.au3 --deterministic          # 改回分析配置：不写盘、不联网、Sleep 跳过
au3 run some.au3 --arg a --arg b          # 脚本里读 $CmdLine[0]/[1]/[2]…
au3 run some.au3 Add --arg 2 --arg 3
au3 run some.au3 Add --cmdline s1 --arg 2 # 脚本拿 --cmdline，函数拿 --arg
au3 run some.au3 BuildFunctionTable --init   # --init 先跑脚本体建立全局表
# --trace 打印解释器执行的语句流
au3 run some.au3 SomeFunc --trace

# 带 GUI 的脚本：GUI 语义（165 个函数）由仿真层回答，画不画由 --gui 决定。
# 每个取值就是一个后端，以后加 gtk/qt 也只是并列多一个取值。
#   auto（默认）——平台自己那套：Windows 上就是真 Win32 窗口和控件，
#                    其他平台不画（无窗口系统可依赖）。**确定性配置下 auto
#                    等于 headless**（分析不该开窗口，更不该弹一个等人点的对话
#                    框）；`--faithful` 时才真的是平台自己那套。
#   headless    —— 任何平台都不画：调用照旧有返回值，屏幕上什么都没有，
#                    分析/CI 要的就是这个
#   native      —— 点名要 Windows 原生后端（真 Win32 窗口和控件，和 auto 在
#                    Windows 上选的是同一个）；没有 Windows 主机时直接报错
#   egui        —— 跨平台的 eframe 窗口（构建时加 --features gui-egui）；
#                    窗口归主线程（winit 要求），脚本跑在它的工作线程上，
#                    关闭窗口或脚本结束即退出。不带该 feature 构建时会明确报错。
au3 run some-gui.au3                     # Windows：真窗口、真对话框；别处：不画
au3 run some-gui.au3 --gui headless      # 不画窗口：MsgBox/InputBox 只按脚本化答案回答（不等人）
au3 run some-gui.au3 --deterministic     # 确定性配置：--gui auto 本来就会解析成 headless
au3 run some-gui.au3 --gui native        # 点名 Win32 原生后端（需 Windows 主机）
au3 run some-gui.au3 --gui egui          # 没有原生路径的主机也能看窗口

# --lang：消息/帮助/诊断用哪种语言（en 英文、zh-CN 简体中文、auto 跟随环境，默认 auto）
au3 --help --lang zh-CN
AU3_LANG=zh-CN au3 run some.au3
au3 debug some.au3 --lang zh-CN

# debug：加载脚本并进入交互式调试 shell（断点/单步/异常时停/查看/求值）
au3 debug some.au3
au3 debug some.au3 -c "break 68" -c run -c "print $string_table[0x4ea]" -c quit
au3 debug some.au3 -c "break 69; run; backtrace"     # -c 里可以用 ; 串多条
au3 debug some.au3 -x breakpoints.au3dbg             # 先跑命令文件，再把 prompt 交给你
au3 debug some.au3 --no-catch                        # 不在未捕获异常处停下
echo 'break 68
run
backtrace
quit' | au3 debug some.au3        # 管道同样可以驱动（不画提示符）
au3 debug some-gui.au3                # Windows 默认用真窗口，别处不画
au3 debug some-gui.au3 --gui egui     # 会话跑在 eframe 窗口下（需 --features gui-egui 构建）
```

### 语言（i18n）

`au3` 的**消息、帮助文本和诊断**都有简体中文版，但只有这一层是"文案"：标识符、
参数名、路径、类型名、值、生成的 AutoIt 源码、反汇编文本、脚本自己的输出
（`ConsoleWrite`/`MsgBox` 的内容）**不翻译**——那些是数据或输出，翻译会破坏可解析性。

```bash
au3 --lang zh-CN --help          # 中文帮助
au3 run some.au3                 # 选语言：--lang > AU3_LANG > LC_ALL/LC_MESSAGES/LANG
AU3_LANG=zh-CN au3 debug some.au3
au3 debug some.au3 --lang en     # 单次强制英文
```

- `--lang` 是全局参数，可以放在子命令前后（`au3 run f.au3 --lang zh-CN` 也行），
  取值只有三个：`auto` / `en` / `zh-CN`——`au3 --help`（以及 `-h`）里会列出来，
  写错时会当作用法错误报出来并再列一次：
  `--lang <LANG> … [默认值：auto] [可选值：auto, en, zh-CN]`。
- 不写 `--lang` 时默认 `auto`，依次看：
  `AU3_LANG` → `LC_ALL` / `LC_MESSAGES` / `LANG` → **本机语言**。
  这几处里 `zh*`（`zh_CN.UTF-8`、`zh-Hans`…）算中文、`en*`/`C`/`POSIX` 算英文，
  不认识的标签（`fr_FR`）**跳过**、继续找下一个来源；
  **Windows 上没有 `LANG` 这类变量**，所以最后一个来源是系统的用户界面语言
  （`GetUserDefaultLocaleName`），中文 Windows 不设任何环境变量也是中文。
  `LANG=C` 或 `CI` 里则是英文。
- 缺翻译时**静默退回英文**，不会漏掉消息；`AU3_I18N_STRICT=1` 会把还没翻译的键
  逐条打到 stderr，方便补。

译文在 [`autoitv3-i18n`](../autoitv3-i18n/README.md) 的
`src/catalog/*.rs` 里，按 `(英文, 中文)` 一条一条加；英文侧留在调用点上，
既是默认文案也是翻译键（`tr("...")` / `msg!("... {name}", name = x)`）。
`cargo test --offline -p au3-cli --bin au3 -- --ignored` 里的两个覆盖率
测试会列出没翻译的键（帮助文本与源码里的键各一个）。

### `#include`

`run` / `debug` / `evaluate` / `deobfuscate` 会按官方语义把 `#include` 展开：被包含
文件的内容插在指令的位置，因此它定义的常量与函数对脚本可见（`parse` / `pretty` 只
读写这一个文件，指令原样保留）。搜索顺序就是帮助页的两张表：

* `#include "file"` —— 先脚本所在目录，再用户库（**倒序**），最后标准库；
* `#include <file>` —— 先标准库，再用户库（正序），最后脚本所在目录。

「标准库」是 AutoIt 安装的 `Include` 目录（帮助页说的是"当前解释器所在目录 +
`\Include`"，而本工具自己就是解释器、旁边没有这个目录，所以改为搜索
`Program Files (x86)\AutoIt3\Include` 等常见位置）。「用户库」是本工具的
`-I/--include-path` 与 `AU3_INCLUDE_PATH`（`;` 分隔，等价于注册表
`HKCU\Software\AutoIt v3\AutoIt\Include`）：

```bash
au3 run script.au3 -I 'D:\Programs\autoitv3\Include'
AU3_INCLUDE_PATH='D:\Programs\autoitv3\Include' au3 run script.au3
```

嵌套包含、`#include-once`、UTF-8（带/不带 BOM）与 UTF-16 BOM 的包含文件都支持；
找不到的文件只报警告（列出搜过的目录）并继续，解析不了的包含文件才是错误，
编译过的 `.a3x` 只报警告。`--no-includes` 完全不展开，脚本按"指令不存在"运行。

### `#RequireAdmin`（仅 Windows）

脚本里写 `#RequireAdmin` 就是要求管理员权限。Windows 没法给一个已经在跑的进程提权，
解释器的做法是**再起一个自己**（shell 的 `runas` 动词，会弹 UAC），由那个进程跑脚本。
`au3 run` 照做，区别只在这几处：

* 原进程**等到**提权副本结束再退出，并把它的退出码写进提示行——AutoIt 会立刻退出，
  在批处理里等一等更有用；
* 原进程的退出码**原样跟随**副本：副本正常结束（`0`，跑完或脚本自己 `Exit`）时写提示行、
  原进程也以 `0` 退出；副本以 `1`/`2` 失败时原进程以**同一个**码退出；副本**崩了**
  （Windows 状态码，例如堆损坏的 `0xC0000374`）时也**原样**跟随——打一行 `error:`
  （码的十进制与十六进制都在里面）并以同一个状态码退出，崩溃因此仍与普通失败区分得开。
  查的时候注意高位为 1 的码：按有符号读它是负数（`0xC0000374` = `-1073740939`），
  批处理里的 `if errorlevel 1` 会把它读成成功，要判崩溃得按十六进制或负数值比对；
* 提权副本拿到同一条命令行外加内部开关 `--elevated-copy`（不会再提权，也不会把
  `#RequireAdmin` 当成"被跳过"再报一次）；
* 提权副本被 shell 服务放进**它自己的控制台**（也就是新开一个窗口）。原进程会把
  自己的 pid 通过 `--attach-console` 传过去，副本先 `FreeConsole` 再
  `AttachConsole(parent)`，把 `CONOUT$`/`CONIN$` 重新装回标准句柄——`ConsoleWrite`
  与提示就留在你敲命令的那个窗口里。原进程没有控制台（输出被重定向、从 GUI 启动）
  时附不上去，副本就自己开一个窗口并说明这一点。

```bash
au3 run setup.au3                  # 脚本里有 #RequireAdmin 且当前不是管理员 → 弹 UAC
au3 run setup.au3 --no-elevate     # 就在当前进程里跑，stderr 说明原因
au3 run setup.au3 --deny spawn     # 同上：显式禁掉 spawn 也算不想要提权
au3 debug setup.au3                # 调试也一样：提权副本接回本窗口的控制台
au3 debug setup.au3 --no-elevate   # 就在本进程里调，stderr 说明原因
```

**已经是管理员就不申请**：从提权后的 shell 启动、UAC 关闭（令牌本来就是完整的）、或者
这个进程本身就是启动器起的副本时，既不会再起副本，也不会弹 UAC，也不会打印任何
"跳过"说明——指令已经满足。（判据是解释器自己的 `IsAdmin()`，不是从命令行参数猜的；
管理员这一条优先于 `--no-elevate`/`--deny spawn`，那两个开关不会把已满足的指令变成
一行抱怨。）

**确定性配置下不真申请，改成模拟**：`--deterministic`（以及 `evaluate`、
`deobfuscate --evaluate` 的默认）遇到 `#RequireAdmin` 时**不**弹 UAC、**不**起副本——
那正是这个配置要拒绝的副作用——而是把提权"模拟"出来：脚本的 `IsAdmin()` 按 `1` 回答，
于是它走管理员那条分支，stderr 打一行
`note: #RequireAdmin: the deterministic profile simulates the elevation instead of asking for it …`。
`--no-elevate` / `--deny spawn` 仍然表示"不要满足这个指令"：那时连模拟也不做，脚本看到
的是真实的、未提权的用户。

非 Windows 主机没有提权机制：读到指令、打印一行说明，脚本照常在本进程里跑。

`au3 debug` 和 `au3 run` 一样默认把会话交给提权副本（UAC 之后副本 `--attach-console`
接回本窗口的控制台，提示符和输出都留在原地）；两种情况下留在本进程并只打印一行说明：
**输入或输出不是终端**（管道/重定向的会话没有控制台可交，交出去还会把重定向的日志挪到
屏幕上）、以及 **`--gui egui`**（窗口归本进程）。`--no-elevate` / `--deny spawn` 也留着，这时打印的是对应的跳过原因。

### 其他指令

| 指令 | 行为 |
| ---- | ---- |
| `#OnAutoItStartRegister "函数"` | **已实现**：脚本每次启动时先调用这个函数，再跑本体第一条语句（`run`/`debug`/`evaluate` 的每次启动都算），返回值和函数体都按普通函数处理；函数不存在按未定义函数报错（`undefined function: …`）。大小写、带不带引号都认，写在函数体里的不算（和 AutoIt 的前处理器一样只看顶层，`#region` 里算）。 |
| `#NoTrayIcon` | 接受，但**在我们这里没有可抑制的东西**：这个工具从不创建托盘图标（`TraySetState`/`TraySetIcon` 这些由内存模型回答，返回值与有无该指令一致）。 |
| `#NoAutoIt3Execute` | 接受，但**不是我们执行的检查**：它的意思是"本脚本不允许被 `AutoIt3.exe /AutoIt3ExecuteScript` 或 `/AutoIt3ExecuteLine` 这样启动"，而本工具不是 `AutoIt3.exe`、也没有这两个开关，普通运行不受影响。真要用那种方式自重启，得由真正的 AutoIt 解释器拦。 |
| `#AutoIt3Wrapper_*` | 打包期指令（图标、版本资源、UPX、`File_Add`……），只在 Aut2Exe 打包时起效，本工具不执行打包。但会把它当**构建指纹**报一行（`# AutoIt3Wrapper settings (N): …`），并用 `_UseX64` 参与回答 `@AutoItX64`；`_Res_File_Add` 的产物读取见「资源」一节。 |

`--win-version` / `--win-arch` / `--no-win-emu` 三个开关同时适用于 `evaluate`、
`deobfuscate --evaluate`、`run` 与 `debug`；省略时读环境变量，再回落到默认值：

| 开关 | 环境变量 | 默认 |
| ---- | -------- | ---- |
| `--win-version <VER>` | `AU3_WIN_VERSION` | `win10` |
| `--win-arch <ARCH>` | `AU3_WIN_ARCH` | `x64` |
| `--resource-module <FILE>` | `AU3_RESOURCE_MODULE` | 自动查找脚本旁的 PE 镜像 |
| （无开关） | `AU3_WIN_REGISTRY` | `./.au3_registry` |
| `--no-win-emu` | `AU3_WIN_EMU=0` | 启用（非 Windows 主机） |
| `--emulate <AREA>` | — | 不启用；`AREA`=`registry`（Reg*）/`clipboard`（Clip*）/具体函数名，可重复 |

选出来的 PE 镜像（`--resource-module`/`AU3_RESOURCE_MODULE`，或输入构建本身，或脚本旁自动找到的那张）
**同时**交给仿真层和 Windows 原生层，所以 `--no-win-emu` 不影响它——那个开关只抽掉仿真层，Windows
上原生层照样把镜像按资源映射进来。没有镜像时，`FindResourceW`/`FileInstall` 还会依次查脚本自己的
`#AutoIt3Wrapper_Res_File_Add=<file>, <type>, <name>, <lang>` 表、再查 `__NAME`/`__Res64/NAME` 这类
staging 文件（这条兜底是仿真层的服务，只在拿不到镜像时用得上）。`# resource module: …`、`# no resource image …` 这类环境说明
**每个进程只打印一次**（`debug` 每次 `run` 都重建 runtime，不收敛就会一行行刷）。

**两个预设，默认按命令分**。`run` 与 `debug` 默认 `--faithful`（AutoIt 自己的语义：
真实延时、真实熵、真实副作用）——跑脚本就该做脚本说的事；`evaluate` 与
`deobfuscate --evaluate` 默认 `--deterministic`（跳过 `Sleep`、`Random` 固定种子、
拒绝一切副作用）——那是分析，不该动这台机器。两个开关都能显式给出（互斥），
覆盖命令自己的默认。

确定性配置还会把**画不画**一起管起来：它下面的 `--gui auto` 解析成 `headless`。
理由是 Windows 上 `auto` 就是真 Win32 后端，`MsgBox`/`InputBox` 会开一个**等人点**
的对话框——分析跑一半卡在弹窗上就白跑了。显式写的 `--gui headless`/`--gui native`/
`--gui egui` 不受影响（后两个是你自己点名要的后端）。
`--allow <KIND>` / `--deny <KIND>`（可重复）在预设之上叠加**按效果类型**的开关：

| KIND | 覆盖的效果 |
| ---- | ---------- |
| `file` | 文件/目录/INI 写入、属性与时间戳 |
| `env` | `EnvSet`/`EnvUpdate` |
| `registry` | `RegWrite`/`RegDelete`（真实或仿真注册表） |
| `clipboard` | `ClipPut`（真实或仿真剪贴板） |
| `spawn` | `Run`/`ShellExecute*`/`RunAs*`/`ProcessWait*` |
| `shutdown` | `Shutdown`（可在 `--faithful` 下单独禁用） |
| `net` | `TCP*`/`UDP*`/`Inet*`/`Ping`/`DriveMap*` |
| `process` | `ProcessClose`/`ProcessSetPriority` |

被配置拒绝的副作用**会报一行**（每种 KIND 只报一次，第一次真被脚本触发时），
否则它和"系统真的不让写"长得一模一样——调用返回失败值、`@error = 1`，
像 `DirCreate` 失败就很容易被当成权限/ACL 问题去查：

```text
note: a file side effect was refused by the execution profile — use --allow file to permit this kind, or --faithful to let the script do its side effects for real
```

`--allow registry` 让确定性的反混淆运行写它要探测的注册表而不放开其它副作用；
`--deny shutdown` 让一次忠实运行永远到不了 `ExitWindowsEx`。
`--emulate registry` 则在 Windows 宿主上把 Reg* 路由到仿真层（文件/内存注册表），
其余函数仍走原生。编程接口：`ExecutionProfile::with_effect(EffectKind, bool)`、
`HostContext::effect_allowed(kind)`、`host_platform_with_options(PlatformOptions)`。

**执行预算**。`--max-steps <N>`（`0` = 不限）同时适用于 `run`、`debug`、
`evaluate` 与 `deobfuscate`，默认 `20000000`。该默认值由
`autoitv3_runtime::interp::DEFAULT_MAX_STEPS` 统一定义：裸 `Runtime`、
`evaluate` 的脚本求值、`deobfuscate` 的函数表求值三条入口共用同一个值，
不再各自写死。

**长任务心跳**。`evaluate` 与 `deobfuscate --evaluate` 的脚本求值一旦超过
1 秒，就会在 stderr 每秒打印一次当前进度（不影响 stdout 的程序输出）：

```text
evaluating: 120 globals, 4 tables (1.0s)
evaluating: 120 globals, 4 tables (2.0s)
evaluated: 120 globals, 4 tables, 1800 values inlined, 420 calls resolved
```

它由 `evaluate_with_debugger` 在运行时挂一个 `Debugger` 实现——解释器每条
语句都会回调 `on_statement`，报告器只在累计若干条后才看一次时钟，因此对
执行本身几乎无开销。`--no-progress` 关掉它（`evaluate` 与
`deobfuscate --evaluate` 都接受），此时根本不挂 debugger。

| 子命令 | 别名 | 说明 |
| ------ | ---- | ---- |
| `parse <FILE>` | `p`, `check` | 解析并报告顶层条目/函数数量 |
| `pretty <FILE> [-o FILE]` | `fmt`, `format` | 规范化重打印，保留注释 |
| `deobfuscate <FILE> [-o FILE]` | `deobf`, `deob` | 反混淆流水线，去除注释（`--evaluate` 先做运行时求值；`--inline-tables` 顺带把表声明换成字面量；`--rename` 做标识符重命名，默认关闭；`--win-version` 等选仿真版本） |
| `evaluate <FILE> [-o FILE]` | `eval`, `e` | 跑脚本主体并内联其算出的表值（`--inline-tables` 顺带把表声明换成字面量；`--faithful` 按 AutoIt 语义；`--win-version`/`--no-win-emu` 控制仿真） |
| `run <FILE> [FUNC] [--cmdline V]… [--arg V]… [--init] [--trace]` | `r`, `exec` | 执行整个脚本；给了 `FUNC` 则调用该函数。`--cmdline` 始终是脚本的 `$CmdLine`/`$CmdLineRaw`；`--arg` 是 `FUNC` 的入参，未给 `FUNC` 时也并入 `$CmdLine`（同样接受 `--win-*` 开关）；默认按 AutoIt 语义**真跑**，`--deterministic` 改回分析配置） |
| `debug <FILE> [-c CMD]… [-x FILE]…` | `dbg` | 交互式调试 shell：断点、单步、**未捕获异常时 post-mortem**、查看帧/变量、表达式求值（`--stop-at-start` 在第一条语句停下，`--no-catch` 关掉异常停）；同样默认 `--faithful` 真跑 |
| `unpack <PATH> [-o FILE]` | `unp` | 取回编译产物里的载荷：`--script` 输出编译进去的 `.au3` 源码（`AU3!EA05`/`AU3!EA06`；PE、裸 chunk 都行）；默认解资源打包的载荷（目录或 PE 都行，自动认角色，`--raw` 输出整段文本） |
| `help` | | 帮助（或 `au3 <CMD> --help` 看单个命令） |

以上 `<FILE>` 一律接受 `.au3` 源码或编译产物（`.exe` / 裸 `AU3!EA05|EA06` chunk）；
后者会被就地解出脚本并同时充当资源镜像（见上文「输入」）。

**缩写**：只要前缀无歧义即可使用，例如 `au3 deob`、`au3 pars`、`au3 pret`。
`-o` 同时支持短名 `-o` 与长名 `--output`。

退出码：`0` 成功，`1` 输入处理失败（解析/运行时错误），`2` 用法或 IO 错误（用法错误由 clap 报出）。

> **受限环境**：若 `~/.cargo` 只读（部分沙箱如此），首次构建需要把 `CARGO_HOME`
> 指向可写目录并离线构建：
> ```bash
> CARGO_HOME=/path/to/writable/cargo-home cargo build --offline
> ```

```bash
# 运行库的单元测试
cargo test                     # 全部（513 项 #[test]，平台/feature 门控下全量约 465）
cargo test -p autoitv3-ast
cargo test -p autoitv3-runtime
cargo test -p autoitv3-platform
cargo test -p autoitv3-deobf
```
## 交互式调试（`au3 debug`）

`au3 debug <FILE>` 加载脚本后**不立即运行**，进入一个 gdb/pdb 风格的 shell：`run` 启动
（再敲一次就是重启），在**断点、单步、以及未捕获异常**处停下；停下时可以查看调用栈、
当前帧的局部变量、全局变量，以及直接在**当前帧**里求值。

```text
$ au3 debug a.au3
(au3) break 69
Breakpoint 1 at line 69
(au3) run
Breakpoint 1, line 69
    69  $fn_table[0x439]($string_table[0x784], $string_table[0xa9a])
(au3:69:1) print $name_table[169]
"DataPswAlgo1"
(au3:69:1) next
(au3:69:1) info breakpoints
  1  line 69     enabled=y  hits=1
```

| 命令 | 说明 |
| ---- | ---- |
| `run` / `restart` | 启动脚本体；在断点处再敲一次 = 从头再来（断点保留） |
| `continue` / `c` | 继续到下一个断点 |
| `step [n]` / `s` | 单步（或连跑 `n` 步，默认 1），进入函数调用 |
| `next [n]` / `n` | 单步（或连跑 `n` 步），不进入调用（停在同层或更浅的语句） |
| `finish` / `fin` | 跑到当前函数返回 |
| `until <行表达式>` / `u` | `tbreak <行表达式>` 的别名（跑到某一行） |
| `frame [n]` / `f`、`up [n]`、`down [n]` | 选帧，编号与 gdb 一致（`#0` 是最内层）：选完 `print`、`info locals`（`list` 也按该帧的行）都在那一帧里求值，`up` 往调用者走、`down` 往最内层走 |
| `untilcall <函数>` / `untilc` / `uc` | 跑到下一次调用该函数，**在它执行之前**停下（内置函数也行，`tbreak` 对内置函数无效）；一次性，停完就清掉 |
| `untilret <函数>` / `untilr` / `ur` | 跑到下一次调用该函数**返回**，停在调用之后的那条语句（想问"它返回了什么""框点掉之后"用这个） |
| `untilgui` / `gui` | `untilcall GUICreate` 的简写，停在建窗口之前 |
| `stopat <函数>...` / `sa` | 和 `untilcall` 停在同一处，但**每次都停**、而且可以一次给多个（带目标名的 catchpoint，一个目标一条命令、可累加，像 gdb 的 `catch syscall <名>`／`catch load <库>`）：内置函数停在它执行前（`stopat MsgBox DllOpen` 把要弹的对话框内容和它要开的 DLL 名先打出来，两者都不会发生），脚本函数停在它的入口（参数已经绑定，`print $x` 直接能读）。`stopat` 单独用是列出，`stopat off` 全清 |
| | 一句话：`untilcall` = 停在调用前（一次），`stopat` = 停在调用前（一直），`untilret` = 停在调用后（一次） |
| `break <行表达式> [if <expr>] [skip <n>] [every <n>] [nostop] [do <cmd>]` / `b` | 断点：条件、命中规则（先消费 skip，再按 every-n 触发；hits 含被 skip 的命中）、`nostop` 纯打印模式（logpoint）、`do` 命中动作（调试命令，命中即执行）；`break <func>` 停在函数第一条语句 |
| `jmp <行表达式>` / `j` | **无条件跳转**：跳过当前帧内直到目标行的语句（不执行），循环条件照常推进；目标行必须是当前帧内的语句起始行 |
| `tbreak <行表达式>` / `tb` | 一次性断点：继续执行直到命中（命中自删）；`run` 前可用 |
| `eval [stmt]` | 在当前帧**执行语句**（赋值真实生效；`print` 是表达式求值）。裸 `eval` 进入多行块：逐行输入 AutoIt 源码（可含 If/For/函数定义），单独一行 `end` 结束；块没读完时提示符变成 `eval> `（`commands <id>` 块则是 `commands <id>> `），明确告诉你这一行是给 `eval` 的，读完才回到 `(au3)`；`-c` 参数里也可以直接内嵌换行（末尾的 `end` 行会被剥离，**空块**按"没有语句"报 `usage: eval <statement>`，不会去求值那个 `end`） |
| `ignore <id> <count>` | 给断点追加 skip 预算 |
| `commands <id> [do <cmd> \| off]` | 查看/追加/清空**命中动作**——动作是调试命令（`print`/`eval`/`set`/`jmp`…），命中即执行；裸 `commands <id>` 进入多行块：每行一条命令，`end` 结束，整块作为该断点的动作 |
| `nostop <id>` / `stop <id>` | 把断点切成/切离 logpoint 模式 |
| `watch <expr>` / `unwatch <id>` / `watch` | 数据断点：表达式值变化即停（首次观察只设基线）；restart 后基线重置 |
| `delete [id]` / `enable` / `disable` | 增删与开关断点 |
| `print <expr>` / `p` | 在当前帧求值（`p $string_table[0x4ea]`、`p Add(1,2)`） |
| `set $x = <expr>` | 在当前帧赋值，**会真的改到正在跑的程序** |
| `info breakpoints\|locals\|globals\|functions` | 查看断点/局部/全局/函数 |
| `backtrace` / `bt` / `where` | 调用栈（`#0` 为最内层） |
| `list [行表达式]` / `l` | 看停点附近的源码，`=>` 标出当前行 |
| `trace on\|off` | 打开后逐条打印执行的语句；`trace depth <n>` 只看深度 ≤ n 的语句，`trace skip <函数>` 折叠热点函数（可叠加、可 `unskip`），`trace` 查看当前配置 |
| `catch on\|off` | 未捕获异常时是否停下（默认 on） |
| `source <file>` | 把一个命令文件的命令插到队首执行，然后回到提示符 |
| `quit` / `q` | 退出 |

停在调用前的两种停法（`untilcall` / `stopat`）把会话的**当前行和当前帧设在调用点上**，而不是“最后执行过的那条语句”上：像 `DllCall($getter(), …)` 这种实参由另一个函数算出来的调用，`list` 会指向 `DllCall` 那条语句、`bt` 的 `#0` 是发起调用的那一帧（否则会落在 getter 的 `Return` 里）。

**行表达式**（`break`/`tbreak`/`until`/`jmp`/`list` 的行号参数都接受）：

| 写法 | 含义 |
| ---- | ---- |
| `123` | 绝对行号 |
| `+N` / `-N` | 相对当前停点 |
| `Func` | 函数第一条语句 |
| `Func+N` / `Func-N` | 函数入口行加减偏移（如 `Main-1` 是函数声明行） |

配合计数步进就能方便地走循环体：`next 5` 连走 5 条同层语句，`step 20` 连走 20 条
（会进入调用），`break Main+1` 直接钉在函数体的第一条语句上。

**跑到建窗口**：脚本的 `GUICreate` 是通过函数表调用的内置函数，没有脚本行可下断点，
`untilgui`（= `untilcall GUICreate`）监听解析后的调用名，**在它执行之前**停下（想看
建完之后的下一步就用 `untilret GUICreate`）。`untilcall`/`untilret` 对任意内置/脚本
函数都适用。`untilgui` 只是让你跳到那段代码，窗口本身由 `--gui` 决定（Windows 上默认
就是真窗口，别处默认不渲染）。

**trace 过滤**：真实脚本的启动会执行上百万条语句，`trace on` 直接刷屏。用
`trace skip <热点函数>` 折叠（例如随机 ID 生成器），`trace depth <n>` 只看浅层调用：

```text
(au3) trace on
(au3) trace skip SomeHotFunc    # 这个函数体不再打印
(au3) trace depth 3             # 只打印深度 <= 3 的语句
```

### Tab 补全与历史

stdin 是终端时，提示符走 `rustyline`：**Tab 补全**、**上/下键翻历史**、`Ctrl-C` 清空当前行
不退出、`Ctrl-D` 退出。补全是**按命令逐个子集**给的，取的是当前会话的实时内容：

| 补齐位置 | 候选来源 |
| ---- | ---- |
| 第一个词 | 所有调试命令（含简写，如 `b`/`c`/`p`） |
| `break` / `tbreak` / `until` / `jmp` 的参数 | 脚本函数名 |
| `untilcall` / `untilret` 的参数 | **内置函数** + 脚本函数名（`untilcall GUI<Tab>` → `GUICreate`） |
| `stopat` 的参数 | 同上（`stopat Ms<Tab>` → `MsgBox`） |
| `print` / `set` / `eval` / `watch` 的参数 | 当前全局变量 `$x` + `@宏` |
| `info` 的参数 | `breakpoints` / `locals` / `globals` / `functions` / `frame` |
| `delete` / `enable` / `disable` / `ignore` / `commands` 等 | 断点 id |

候选在**每次提示符前**从会话里现取（函数名、全局变量、断点都是那一刻的真值），所以
`run` 之后变量出现了，`p $<Tab>` 就补得出来。stdin 是管道时照旧用朴素读行、不画提示符，
脚本化行为不变；终端不可用时也会安静回落到朴素读行。

### 未捕获异常时停下（post-mortem）

`RuntimeError` 一旦抛出就没人会接住（AutoIt 没有 try/catch，解释器把它当作终止），
所以"未捕获异常"就是"任何运行时错误"。默认会**在抛出错误的那条语句处停下**：

```text
(au3) run
[uncaught error] index 5 out of bounds (len 2) (at 6:12)
     6      Return $list[$n]
(au3:6:5) backtrace
#0  Boom at 6:5
#1  Outer at 11:5
(au3:6:5) info locals
list = Array[2] {"1", "2"}
n = 5
x = 10
(au3:6:5) print $x
10
```

关键在**停的位置**：钩子挂在 `exec_stmt` 的错误返回路径上，而抛出错误的那个栈帧要到
`call_user` 返回时才弹出，所以此刻 `#0` 那层的局部变量仍然活着——这正是 post-mortem
要有用的前提。同一个错误会向上穿过每一层 `exec_stmt`，`error_reported` 保证只上报一次。

- `catch off`（或命令行 `--no-catch`）关掉它，错误就只作为 `[script stopped: …]` 报出。
- 调试器主动中止（`quit`、在停点敲 `run` 重启）走的是独立的 `RuntimeError::Aborted`，
  **不会**被当成脚本异常，所以不会反过来弹出一个 post-mortem 停点。

**和 gdb 的对应**：gdb 的 `catch` 是**一族**事件 catchpoint（`catch throw`、`catch catch`、
`catch syscall <名>`、`catch load <库>`、`catch fork`、`catch signal` ……），不是单个命令。
这里的两组各对上其中一支：

| 本调试器 | gdb 里对应的一支 | 差别 |
| ---- | ---- | ---- |
| `catch on\|off` | `catch throw`：停在抛出点、栈还没展开（gdb 是停在 `__cxa_throw` 这类抛出坝上） | gdb 用"装一个 catchpoint / `delete` 撤掉"来管；这里是常驻开关（默认开，`--no-catch` 关），不进断点编号列表 |
| `stopat <函数>...` | 带目标名的那一类：`catch syscall <名>`、`catch load <库>`，一个目标一条命令、累加 | 目标是 AutoIt 的内置/脚本函数名，不是 syscall 号或库名 |
| `untilcall` / `untilret` | `tcatch <事件>`：一次性的 catchpoint | 停在调用前 / 调用后（`untilret` 是 gdb 没有的"返回后"这一侧） |

pdb 这边没有可对应的别名：原版 pdb 没有异常 catchpoint（同类功能是 Visual Studio 的
"break when thrown"、windbg 的 `sxe eh`/`sxe clr`），所以 `catch` 不能拿来当 `stopat` 的短写。

### 停止是怎么实现的

`Debugger::on_stop` 是**从解释器内部**被调用的 —— Rust 栈还活着，提示符就跑在那里，
用户敲 `continue` 之后它才返回、解释器才继续。因此不需要把解释器改写成状态机或协程：
栈帧、局部变量、求值上下文全都是现成的。单步是 shell 自己的记账（`StepMode`），
解释器只负责"每条语句前问一次"，不做任何步进语义建模。

调试器在回调期间被 `take()` 出字段，因此它能拿到 `&mut Runtime`（`DebugHost`）来读帧、
求值和改断点；同一时刻 `in_debugger` 置位，保证 `print` 引起的嵌套执行不会递归进调试器。
shell 侧的句柄用 `try_borrow_mut`，所以"命令正在执行时又被递语句"这种情况会被安静地
当成 `Continue`。

### 三个容易踩的语义细节

- **断点条件按表达式解析**。AutoIt 里 `$i = 5` 单独成句是**赋值**，出现在表达式位置才是
  **比较**；条件若被当成语句，既会误触发又会改坏被调试的程序。因此条件一律走
  `Runtime::evaluate_expression`（借 `Return` 强制表达式语法），`$i = 5` 是比较。
- **`print` 同理**：`print $i = 3` 是比较，赋值请用 `set`。
- **未定义变量在 `print` 里点名报错**。脚本里读一个从没赋过值的名字，默认得到 `""`
  （`Opt("MustDeclareVars", 0)`），这是脚本期待的行为，`run` 照旧；但提示符上敲出来的
  名字是**问题**，不是程序里的值，`print $contuer` 回一个 `""` 会让人以为真有个空变量。
  所以调试器的表达式求值（`print`、`watch`，断点条件走同一条路）遇到从没赋值的名字
  直接报 `undefined variable: $contuer`；已经存在、值就是 `""` 的变量照常打印 `""`。
  条件里出现未定义变量时按"不成立"处理（条件求值失败即 false），不会静默当成空串。

### 命令来源：`-x` 文件、`-c`、stdin

命令来自**同一个队列**，顺序是：先用 `-x FILE` 里的命令，再用 `-c` 的命令，最后是 stdin。
外层循环与断点处的提示符都从这个队列取，所以 `-c run -c next -c 'print $x' -c quit`
的含义和字面一致 —— `run` 停下后，剩下的命令由提示符消费。

```bash
au3 debug a.au3 -x breakpoints.au3dbg          # 先跑常用断点，然后交回提示符
au3 debug a.au3 -c "break 69; run; print \$string_table[0x4ea]"
au3 debug a.au3 -x setup.au3dbg -c run -c quit # 文件 → -c → stdin，依次消费
```

- **`;` 分隔多条命令**（`-c`、`-x` 文件、交互输入都适用）。双引号内的 `;` 不切分，
  所以 `print "a;b"` 是一整条；AutoIt 的 `""` 转义也照旧。文件里空行与 `#` 开头的行忽略。
- **命令文件跑完不会退出**：stdin 是终端时继续给提示符，是管道时继续读管道，读到 EOF 才结束——
  `-x setup.au3dbg` 就是"先加载我惯用的断点，再把 prompt 交给我"。
- 会话中还可以用 `source <file>` 再塞一个文件进来（插到队首，优先于已排队的命令）。
- stdin 是管道时不画提示符，方便脚本化。
