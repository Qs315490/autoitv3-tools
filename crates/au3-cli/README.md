# au3-cli — `au3` 命令行工具

**`au3` 命令行工具**：解析、规范化、反混淆、求值、运行、调试、解包——workspace 各库 crate 的装配层（clap 参数、子命令 dispatch、退出码）。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    au3-cli/                # CLI 二进制 crate（产物名为 au3，使用 clap 解析参数）
      src/
        main.rs     入口：Cli::parse() → dispatch → 把 CliError 转成退出码
        cli.rs      顶层 Cli / Command 定义（子命令、别名、缩写开关）
        args.rs     共用参数类型（-o 输出）、CliError、输入加载
        output.rs   输出目标：文件或 stdout（`-` 表示 stdout）
        commands/
          mod.rs        子命令模块与 dispatch 表
          parse.rs      au3 parse（ParseArgs + run）
          pretty.rs     au3 pretty（PrettyArgs + run）
          deobfuscate.rs au3 deobfuscate（DeobfuscateArgs + run，含 --evaluate）
          evaluate.rs   au3 evaluate（EvaluateArgs + run）
          run.rs        au3 run（RunArgs + run，含 --trace 用的 Debugger 示例实现）
          debug.rs      au3 debug（DebugArgs + 交互式 shell：命令解析、步进策略、提示符）
          unpack.rs     au3 unpack（UnpackArgs + run：--script/--raw/--table/--at）
      tests/
        debug.rs     端到端驱动真实二进制：断点/单步/条件/求值/重启/stdin（21 项）
```

## 使用

CLI 采用**子命令**形式（一级参数不带 `--` 前缀），解析由 [clap](https://crates.io/crates/clap) 完成，
因此自带 `--help` / `--version`、**子命令缩写**与**别名**：

```bash
cargo build --release
au3 --help                                 # 查看全部命令

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
au3 evaluate some.au3 --faithful          # 按 AutoIt 语义真跑
# 也可以一步到位：先求值再做常规反混淆
au3 deobfuscate some.au3 --evaluate -o clean.au3

# 选定仿真的 Windows 系统版本（非 Windows 主机默认 win10；见 [`autoitv3-platform`](../autoitv3-platform/README.md) 的「Windows 仿真」）
au3 evaluate some.au3 --win-version win11 -o resolved.au3
au3 evaluate some.au3 --win-version win7 --win-arch x86 -o resolved.au3
au3 evaluate some.au3 --no-win-emu        # 关掉仿真，停在第一个 Windows 调用

# run：用解释器调用函数（--arg 传参，--init 先执行脚本体以建立全局表）
au3 run Add --arg 2 --arg 3 some.au3
au3 run BuildFunctionTable --init some.au3
# --trace 打印解释器执行的语句流
au3 run SomeFunc --trace some.au3

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
```

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

**细粒度行为控制**。两个预设（`--faithful` / 确定性）保持不变，
`--allow <KIND>` / `--deny <KIND>`（可重复）在其上叠加**按效果类型**的开关：

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

`--allow registry` 让确定性的反混淆运行写它要探测的注册表而不放开其它副作用；
`--deny shutdown` 让一次忠实运行永远到不了 `ExitWindowsEx`。
`--emulate registry` 则在 Windows 宿主上把 Reg* 路由到仿真层（文件/内存注册表），
其余函数仍走原生。编程接口：`ExecutionProfile::with_effect(EffectKind, bool)`、
`HostContext::effect_allowed(kind)`、`host_platform_with_options(PlatformOptions)`。

| 子命令 | 别名 | 说明 |
| ------ | ---- | ---- |
| `parse <FILE>` | `p`, `check` | 解析并报告顶层条目/函数数量 |
| `pretty <FILE> [-o FILE]` | `fmt`, `format` | 规范化重打印，保留注释 |
| `deobfuscate <FILE> [-o FILE]` | `deobf`, `deob` | 反混淆流水线，去除注释（`--evaluate` 先做运行时求值；`--inline-tables` 顺带把表声明换成字面量；`--rename` 做标识符重命名，默认关闭；`--win-version` 等选仿真版本） |
| `evaluate <FILE> [-o FILE]` | `eval`, `e` | 跑脚本主体并内联其算出的表值（`--inline-tables` 顺带把表声明换成字面量；`--faithful` 按 AutoIt 语义；`--win-version`/`--no-win-emu` 控制仿真） |
| `run <FUNC> <FILE> [--arg V]… [--init] [--trace]` | `r`, `exec` | 解释执行一个函数（同样接受 `--win-*` 开关） |
| `debug <FILE> [-c CMD]… [-x FILE]…` | `dbg` | 交互式调试 shell：断点、单步、**未捕获异常时 post-mortem**、查看帧/变量、表达式求值（`--stop-at-start` 在第一条语句停下，`--no-catch` 关掉异常停） |
| `unpack <PATH> [-o FILE]` | `unp` | 取回编译产物里的载荷：`--script` 输出编译进去的 `.au3` 源码（`AU3!EA05`/`AU3!EA06`；PE、裸 chunk 都行）；默认解资源打包的载荷（目录或 PE 都行，自动认角色，`--raw` 输出整段文本） |
| `help` | | 帮助（或 `au3 <CMD> --help` 看单个命令） |

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
| `step` / `s` | 单步，进入函数调用 |
| `next` / `n` | 单步，不进入调用（停在同层或更浅的语句） |
| `finish` / `fin` | 跑到当前函数返回 |
| `until <line>` | 跑到某一行 |
| `break <line> [if <expr>]` / `b` | 断点，可带条件 |
| `delete [id]` / `enable` / `disable` | 增删与开关断点 |
| `print <expr>` / `p` | 在当前帧求值（`p $string_table[0x4ea]`、`p Add(1,2)`） |
| `set $x = <expr>` | 在当前帧赋值，**会真的改到正在跑的程序** |
| `info breakpoints\|locals\|globals\|functions` | 查看断点/局部/全局/函数 |
| `backtrace` / `bt` / `where` | 调用栈（`#0` 为最内层） |
| `list [line]` / `l` | 看停点附近的源码，`=>` 标出当前行 |
| `trace on\|off` | 打开后逐条打印执行的语句 |
| `catch on\|off` | 未捕获异常时是否停下（默认 on） |
| `source <file>` | 把一个命令文件的命令插到队首执行，然后回到提示符 |
| `quit` / `q` | 退出 |

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

### 停止是怎么实现的

`Debugger::on_stop` 是**从解释器内部**被调用的 —— Rust 栈还活着，提示符就跑在那里，
用户敲 `continue` 之后它才返回、解释器才继续。因此不需要把解释器改写成状态机或协程：
栈帧、局部变量、求值上下文全都是现成的。单步是 shell 自己的记账（`StepMode`），
解释器只负责"每条语句前问一次"，不做任何步进语义建模。

调试器在回调期间被 `take()` 出字段，因此它能拿到 `&mut Runtime`（`DebugHost`）来读帧、
求值和改断点；同一时刻 `in_debugger` 置位，保证 `print` 引起的嵌套执行不会递归进调试器。
shell 侧的句柄用 `try_borrow_mut`，所以"命令正在执行时又被递语句"这种情况会被安静地
当成 `Continue`。

### 两个容易踩的语义细节

- **断点条件按表达式解析**。AutoIt 里 `$i = 5` 单独成句是**赋值**，出现在表达式位置才是
  **比较**；条件若被当成语句，既会误触发又会改坏被调试的程序。因此条件一律走
  `Runtime::evaluate_expression`（借 `Return` 强制表达式语法），`$i = 5` 是比较。
- **`print` 同理**：`print $i = 3` 是比较，赋值请用 `set`。

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
