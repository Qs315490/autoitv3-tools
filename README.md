# autoitv3-tools (workspace)

AutoIt v3 词法/语法分析工具集：产出**带源码位置（Span）的 AST**，作为反混淆分析的基座。
代码结构分层，便于后续叠加功能（常量折叠、断点调试、解释执行器等）而无需改动核心。

采用 **Cargo workspace**：AST 作为可复用的库 crate，CLI 作为独立二进制 crate 调用库。

## 结构

```
autoitv3-tools/
  Cargo.toml                 # workspace 定义
  crates/
    autoitv3-ast/            # 库 crate（可被下游依赖）——AutoIt AST 分析核心
      src/
        span.rs    位置(Pos)与区间(Span)——每个 AST 节点都带 Span，方便断点/源码映射
        token.rs   词法 token 定义（关键字、运算符、复合赋值、三元 ?:）
        lexer.rs   手写词法分析器：`#指令`整行、字符串""转义、0x 十六进制、$var/@macro
        ast.rs     AST 定义：Program / Item / FuncDef / Stmt / Expr / Lit / Call / IndexCall / Ternary / ArrayLit ...
        parser.rs 递归下降分析器：语句按行/冒号分隔，表达式用优先级爬升
        lib.rs    库入口，统一导出
      tests/
        integration.rs     库集成单元测试（25 项）
        syntax_coverage.rs 语法覆盖回归集（14 项，见下文「语法覆盖」）
    autoitv3-format/         # 库 crate——格式打印（原名 pretty）
      src/lib.rs    把 AST 重新打印为 AutoIt 源码（默认保留注释，可 strip；规范缩进）
      tests/
        format.rs    格式化/注释保留/去除测试（3 项）
    autoitv3-runtime/        # 库 crate——AutoIt v3 运行时（值模型 + 解释器 + 扩展接口）
      src/
        value.rs      运行时值模型（Int/Float/Str/Array/Map/Binary/FuncRef）与 AutoIt 强制转换规则
        interp.rs     Runtime 解释器：加载程序、调用函数、求值表达式、执行语句
        builtins.rs   已实现的内置函数子集（字符串/数值/位运算/数组/Map/Execute/Call...）
        host.rs       Host trait——嵌入方接入原生函数的接口（优先级高于平台层）
        platform/     Platform trait（仅接口；实现见 autoitv3-platform）
        profile.rs    执行配置：忠实语义 vs 确定性分析语义（见下文「执行配置」）
        regexp.rs     StringRegExp* ——基于纯 Rust regex 引擎，平台无关
        debug.rs      Debugger trait / Breakpoint / FrameInfo——后续 debug 模块的接口
        error.rs      RuntimeError 与控制流信号 Flow
        lib.rs        公共 API
      tests/
        runtime.rs    解释器/host/debug 接口 + 真实集成测试（32 项）
        regexp.rs     StringRegExp / StringRegExpReplace（27 项）
    autoitv3-platform/       # 库 crate——平台层（分层：通用 + 系统）
      src/
        lib.rs        Platform 分层组合（CompositePlatform）、host_platform() 工厂、
                      runtime_with_platform() 便捷构造
        portable.rs   通用层：文件/目录 I/O、环境变量、数学、计时器、控制台
                      —— Linux 与 Windows 都安装
        linux.rs      系统层（Linux）：/proc 进程查询、OS 标识宏
        windows.rs    系统层（Windows）：注册表/COM/DllCall/GUI 扩展点（骨架）
      tests/
        platform.rs   分层、选择、注入、通用函数与宏（31 项）
    autoitv3-deobf/          # 库 crate——反混淆 pass（常量折叠 + 函数表解析 + 重命名）
      src/
        fold.rs        常量折叠：遍历 AST，把纯常量表达式交给 runtime 求值后内联
        rename.rs      确定性重命名混淆的变量/函数/宏为可读别名（可复现）
        table.rs       函数表解析：用 runtime 执行 $fn_table 构建函数，把 $fn_table[0x..](...)
                       改写为真实函数名调用（解开函数间接层）
        evaluate.rs    运行时求值：跑脚本主体，把它算出来的表值内联回源码
                       （唯一能解开字符串表的途径）
        orchestrator.rs 按序执行 pass 流水线，产出 Deobfuscator/Report
        lib.rs
      tests/
        deobf.rs      反混淆 pass 单元测试（13 项）
        table_test.rs 函数表解析测试（最小 + 全量样本，2 项）
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
```

### autoitv3-runtime 的定位

反混淆需要**执行**代码：混淆器把函数表和字符串表放在数组里，靠运行生成的辅助函数
构建。纯 AST 改写只能解开函数表（它完全由数组字面量拼成），字符串表依赖字符串运算、
`Execute`、Map 和循环，因此需要一个真正的解释器。

该 crate 提供三样东西：

1. **小型解释器**（`interp.rs`）——供反混淆调用。覆盖 `ByRef`（含数组共享存储）、
   `ReDim` 原地扩容、`For To Step` / `For In`、复合赋值、`@error`/`@extended`、
   `Select`/`Switch`、递归与步数护栏。
2. **完整运行时的接口**（`host.rs`）——`Host` / `HostContext` / `NativeHost`：
   把 Win32、COM、GUI、DllCall 等原生能力注册进来，解释器核心不依赖任何平台。
3. **后续 debug 模块的接口**（`debug.rs`）——`Debugger`（每条语句回调、可返回
   `Continue`/`Pause`/`Abort`）、`Breakpoint`/`Breakpoints`、`FrameInfo` 调用栈快照、
   `StopReason`。解释器每执行一条语句都会调用该接口，因此交互式调试器、DAP 服务端
   或自动化 tracer 都能直接接上。

## 反混淆现状

`au3 deobfuscate` 现在执行 3 个 pass：

1. **常量折叠**（fold）：求值纯算术/字符串/拼接，原地内联。
2. **函数表解析**（table）：静态执行 `BuildFunctionTable()`（纯数组构建，
   `Local $x[]=[...]` + `MergeArrays` + `Return`）得到 `$fn_table` 函数表
   （1108 个函数名），把所有 `$fn_table[0x..](args)` 改写为 `FuncName(args)`、
   `$fn_table[0x..]` 改写为 `FuncName`。在真实脚本上改写约 several thousand 处引用。
3. **标识符重命名**（rename）：确定性重命名变量/函数/宏为可读别名。

### 运行时相关代码的迁移

原先 `autoitv3-deobf` 里自带两处"求值"逻辑，现已全部迁入 `autoitv3-runtime`：

- `table.rs` 曾手写一个数组字面量求值器来模拟 `MergeArrays`；现在直接把
  builder 交给解释器执行（`Runtime::call_function`），不再重复实现 AutoIt 语义。
- `fold.rs` 曾自带一套运算符求值（`apply_binary`/`neg`/`not`）；现在只负责
  遍历 AST 与判断"哪里可以内联"，实际求值交给 `Runtime::eval_expr`，
  并用 `is_constant_expr` 作为安全闸门（保证纯常量才内联）。

好处是 AutoIt 的运算符语义（强制转换、字符串拼接、整数/浮点提升）只有**一份**实现，
不会随两处代码各自演进而产生偏差。

> **TODO（字符串表求值）**：`$string_table`（字符串表）由 `$fn_table[0x33d]()` 构建，
> 其内部依赖 `Execute`、`Map`、二进制运算等。解释器骨架已就绪并跑通函数表，
> 但要完整求值字符串表，还需继续补齐：运行整个脚本体时的数组语义细节
> （当前在 `--init` 全量执行时遇到索引越界）、以及更多内置函数
> （`StringRegExp*`、`DllCall` 真实语义等）。这属于下一步工作。

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

# deobfuscate：常量折叠 + 函数表解析 + 标识符重命名 + 去注释
#              统计信息走 stderr，stdout 保持为干净的 AutoIt 源码
au3 deobfuscate some.au3

# -o FILE 将输出写入文件；-o - 或省略 -o 则输出到 stdout（原文件永不被修改）
au3 pretty      some.au3 -o out.au3
au3 deobfuscate some.au3 -o -

# evaluate：跑一遍脚本主体，把它运行时算出来的表值内联回源码
#           （唯一能解开字符串表的途径；撞到平台边界时会报告并保留已求出的值）
au3 evaluate some.au3 -o resolved.au3
au3 evaluate some.au3 --faithful          # 按 AutoIt 语义真跑
# 也可以一步到位：先求值再做常规反混淆
au3 deobfuscate some.au3 --evaluate -o clean.au3

# run：用解释器调用函数（--arg 传参，--init 先执行脚本体以建立全局表）
au3 run Add --arg 2 --arg 3 some.au3
au3 run BuildFunctionTable --init some.au3
# --trace 打印解释器执行的语句流（演示 debug 接口）
au3 run SomeFunc --trace some.au3
```

| 子命令 | 别名 | 说明 |
| ------ | ---- | ---- |
| `parse <FILE>` | `p`, `check` | 解析并报告顶层条目/函数数量 |
| `pretty <FILE> [-o FILE]` | `fmt`, `format` | 规范化重打印，保留注释 |
| `deobfuscate <FILE> [-o FILE]` | `deobf`, `deob` | 反混淆流水线，去除注释（`--evaluate` 先做运行时求值） |
| `evaluate <FILE> [-o FILE]` | `eval`, `e` | 跑脚本主体并内联其算出的表值（`--faithful` 按 AutoIt 语义） |
| `run <FUNC> <FILE> [--arg V]… [--init] [--trace]` | `r`, `exec` | 解释执行一个函数 |
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
cargo test                     # 全部（123 项）
cargo test -p autoitv3-ast
cargo test -p autoitv3-runtime
cargo test -p autoitv3-platform
cargo test -p autoitv3-deobf
```

## 作为库调用

```rust
use autoitv3_ast::{parse, pretty::PrettyPrinter};

let prog = parse(src)?;               // 得到 span-aware AST
let funcs = prog.items.iter()
    .filter(|it| matches!(it.kind, autoitv3_ast::ast::ItemKind::Func(_)))
    .count();

let mut pp = PrettyPrinter::new();
let out = pp.print_program(&prog);    // 反混淆/规范化输出
```

## 已支持的 AutoIt 语法

- 预处理器指令整行（`#include <file>`、`#NoTrayIcon`、`#AutoIt3Wrapper_...=...`）
- 字符串字面量（含 `""` 转义）、0x 十六进制数、浮点、`True/False/Default/Null`
- 变量 `$x`、宏 `@x`、数组下标 `$a[i][j]`、数组函数引用调用 `$arr[i](args)`
- 数组字面量初始化 `Local $a[] = [$x, $y]` 与 `Enum` 枚举
- 函数 `Func ... EndFunc`，参数 `Const/ByRef`（任意顺序）、默认值
- 复合赋值 `+= -= *= /= ^= &=`、三元条件 `? :`
- 语句：`If/ElseIf/Else/EndIf`（含单行 Then）、`While/WEnd`、`Do/Until`、
  `For ... To ... Step/Next` 与 `For ... In .../Next`、`Select/Case`、`Switch/Case`、
  `With/EndWith`、`Return/Exit/ExitLoop/ContinueLoop`、`#forceref` 等函数内指令
- 声明：`Local/Global/Dim/Static/Const/ReDim`，多个作用域关键字叠加（如 `Static Local`，`Static` 优先级最高）

## 设计说明（面向后续断点调试）

- 每个 `Stmt`、`Expr`、`Item` 都带 `Span { start: Pos, end: Pos }`，调试器可按行/列命中源码行。
- `Stmt` 是一个可执行的单元节点，未来解释器/调试器只需遍历语句并在命中断点位置暂停。
- 解析器与打印器分离：反混淆时可先打印出规范化文本，再对其做常量替换等变换。

## 运行时求值（`au3 evaluate`）

纯语法改写能解开**函数表**（`$fn_table` 完全由数组字面量拼成），但**字符串表**是运行生成的
代码（`Execute`、Map、`Binary`、字符串运算）算出来的——静态方法无解。因此提供运行时求值：

```bash
au3 evaluate sample.au3 -o resolved.au3     # 跑脚本主体，内联它算出来的值
au3 deobfuscate sample.au3 --evaluate -o clean.au3   # 求值 + 常规反混淆一步到位
```

实现（`autoitv3-deobf/src/evaluate.rs`）：跑脚本顶层主体 → 把每个**常量下标**的表引用
换成运行时真正得到的值：

```text
$name_table[0x38]    ->  2
$fn_table[0x33d]() ->  ResolvedFunc()      （函数名调用）
```

安全规则：

- **赋值左值不会被替换**（否则会写出 `0 -= 1` 这种非法语句），只替换其下标
- **裸变量读取仅在 `Global Const` 时内联**（可变全局可能被改写，内联其值会出错）
- 带下标读取视表为"建成后不再变"，这是混淆器的实际用法
- 含换行的字符串**不内联**（AutoIt 字面量无法表示换行，内联会导致输出无法解析）

### 部分求值是常态

真实脚本的启动代码很快会触碰操作系统（`DllCall`、注册表、GUI）——正是平台层标注的边界。
但混淆器**很早就把表建好**，所以中断的运行仍留下可用的表。因此求值失败时**保留已算出的值**
并报告停在哪里，而不是整体丢弃：

```
$ au3 evaluate sample.au3 -o resolved.au3
evaluated: 23 globals, 4 tables, 827 values inlined, 11557 calls resolved
script body did not finish: undefined function: DLLSTRUCTCREATE (at 1769:21)
  (that is the platform boundary: this function is not implemented for the current OS)
  values produced before that point were still inlined
```

在真实脚本上的实际效果：

| 引用 | 求值前 | 求值后 |
| ---- | ------ | ------ |
| `$fn_table[...]` | several thousand | **127** |
| `$name_table[...]` | 817 | **3** |
| 字符串字面量 | 41 | **406**（解出 `Execute` 的动态代码） |
| `$string_table[...]` | several thousand | several thousand（**卡在 Windows 边界**） |

`$string_table` 未解开的原因是它本身依赖 Windows API：其构建路径调用
`DllStructCreate(OSVERSIONINFO)` + `DllCall(GetVersionEx)` 取系统版本，
再按版本选择字符串。这部分要等 `windows.rs` 平台层实现。

## 语法覆盖

AutoIt v3 的语法覆盖由 `crates/autoitv3-ast/tests/syntax_coverage.rs` 固化：
一份 80+ 条构造的清单（预处理指令、行继续符、字面量、运算符、语句、声明、函数、
对象/COM、宏与关键字），外加对**语义**敏感的定点断言（`=` 的上下文含义、
`ContinueLoop`/`ExitLoop` 区分、`ReDim`、`Enum Step`、块注释、`Volatile`、
`With` 隐式主语、成员/方法调用形状）。清单同时包含**必须被拒绝**的非法构造
（嵌套 `Func`、单行 `If … Else`、孤立 `.`、未闭合字符串）。

这样做的原因是：早期覆盖率是靠"能解析手头那一个样本"来推断的，而该样本恰好
没用到若干构造。现在新增语法支持必须同时进清单，避免回归。

已支持的要点：

- **行继续符 `_`**（前面带空格、行尾，可跟注释）——把多行并成一条语句
- **`#cs`/`#ce` 与 `#comments-start`/`#comments-end` 块注释**——原样保留并可重新打印
- **单引号字符串** `'…'`（含 `''` 转义），与双引号等价
- **`=` 的双重含义**——语句层是赋值，表达式内（`If`/`While`/`Case`/实参）是
  大小写不敏感比较（AST 用独立变体 `EqLoose` 表示，`==` 仍为大小写敏感）
- **对象/COM 点语法**——`$obj.Prop`、`$obj.Method(args)`、成员链、以及
  `With` 块内的隐式主语 `.Member`
- **`Enum` 编号**——`Enum $A, $B`、显式初值重置计数、`Enum Step n`

## 平台层

解释器核心、值模型与语言级内置函数都与平台无关。AutoIt 的函数库分成**通用**与
**系统相关**两部分，因此平台层是**分层**的（crate `autoitv3-platform`）：

| 层 | 模块 | 安装于 | 内容 |
| -- | ---- | ------ | ---- |
| 通用 | `portable.rs` | **所有**平台 | 文件与目录 I/O、环境变量、数学、计时器、控制台——AutoIt 在各系统上行为一致的部分 |
| 系统 | `linux.rs` | 仅 Linux | `/proc` 进程查询（`ProcessList`/`ProcessExists`/`ProcessClose`）、OS 标识宏 |
| 系统 | `windows.rs` | 仅 Windows | 注册表、COM、`DllCall`、GUI（**骨架**，后续填充） |

`host_platform()` 按目标平台组装成 `CompositePlatform`（Linux 为 `portable+linux`），
逐层查找；通用层在 Windows 上同样生效，系统层只补真正系统相关的部分。

`Platform` **trait** 留在 `autoitv3-runtime`（解释器调用的接缝），**实现**在此 crate。
依赖方向单向——运行时不知道任何具体操作系统——因此 `Runtime::new()` 默认**没有**平台层，
需要时用 `autoitv3_platform::runtime_with_platform(&prog)` 或 `rt.set_platform(...)` 安装。

查找顺序为 **内置函数 → Host → Platform**，嵌入方可用 `Host` 覆盖任何平台实现。

### 通用层已实现

| 类别 | 函数 |
| ---- | ---- |
| 文件 | `FileOpen`/`FileClose`/`FileFlush`/`FileRead`/`FileReadLine`/`FileWrite`/`FileWriteLine`（句柄表、模式标志 `$FO_READ`/`APPEND`/`OVERWRITE`/`CREATEPATH`）、`FileExists`、`FileGetSize`、`FileGetTime`、`FileGetAttrib`、`FileGetLongName`/`FileGetShortName`、`FileDelete`、`FileCopy`、`FileMove`、`FileSetAttrib` |
| 目录 | `DirCreate`、`DirRemove`、`DirGetSize`、`DirCopy`、`DirMove` |
| 环境 | `EnvGet`、`EnvSet`、`EnvUpdate` |
| 数学 | `Round`（半数远离零）、`Sqrt`、`Sin`/`Cos`/`Tan`/`ASin`/`ACos`/`ATan`（**弧度**）、`Log`、`Exp`、`Floor`、`Ceiling`、`Random`、`RandomSeed` |
| 计时 | `TimerInit`、`TimerDiff` |
| 控制台 | `ConsoleWrite`、`ConsoleWriteError`、`ConsoleRead` |
| 宏 | `@TempDir`、`@AutoItPID`、`@AutoItEXE`、`@WorkingDir`/`@ScriptDir`、`@UserName`、`@HomePath`/`@UserProfileDir`、`@AppDataDir`/`@LocalAppDataDir`（XDG）、`@DesktopDir`、`@MyDocumentsDir` |

宏由**平台**提供（解释器只负责 `@error`/`@extended`/`@ScriptLineNumber`/`@NumParams`/
`@CRLF` 等纯状态与常量），因此 `@TempDir` 之类不再是空串。

### 执行配置（ExecutionProfile）——近似行为按用途区分

解释器有两类调用者，诉求相反：**反混淆**要快、可复现、无害（`Sleep(60000)` 不能真等
一分钟，`Random` 不能每次不同，评估样本不该动磁盘）；**正常运行**要按 AutoIt v3 语义
（真的延时、真的随机、真的产生副作用）。写死任一种都会对另一类造成错误行为，因此这些
行为是一个**值**，由调用方选择：

```rust
rt.set_profile(ExecutionProfile::deterministic());  // 分析：快、可复现、拒绝写入
rt.set_profile(ExecutionProfile::faithful());       // 运行：AutoIt 语义
```

| 维度 | `deterministic()`（反混淆/分析） | `faithful()`（正常运行） |
| ---- | -------------------------------- | ------------------------ |
| `Sleep` | **直接返回**（`SleepPolicy::Skip`） | 真实延时（`Real`），另有 `Capped(时长)` 上限模式 |
| `Random` | **固定种子**，多次运行结果一致 | **OS 熵**，每次不同（`RandomSeed` 显式覆盖两者） |
| 副作用（文件写入/删除/建目录/改环境） | **拒绝**：调用失败并置 `@error = 1`（`EffectPolicy::ReadOnly`） | 真实执行（`Allow`） |
| 读取（`FileRead`/`FileExists`/`EnvGet`…） | 照常工作 | 照常工作 |

**默认是 `faithful()`**——库不应悄悄改变脚本的行为；反混淆相关代码（`deobfuscate`
的常量折叠与函数表解析）**显式**切到 `deterministic()`，保证输出可复现、不触碰机器。

CLI 的 `au3 run` 面向"探查混淆样本"，因此默认用确定性配置；需要按 AutoIt 语义真跑时加
`--faithful`：

```bash
au3 run F sample.au3              # 快速、可复现、不改磁盘
au3 run F --faithful sample.au3   # 真的 Sleep、真的随机、真的写文件
```

其余仍属**有意为之的近似**（与执行配置无关，已在模块文档逐条标注）：

- `FileGetTime` 返回 **UTC**（本地时区需要时区数据库），`YYYY/MM/DD HH:MM:SS` 格式与 AutoIt 一致
- `FileGetAttrib` 返回 `D`（目录）/`A`（普通文件），只读时加 `R`；Windows 专有的 `S`/`H` 无对应概念，不设置
- `FileGetShortName` 无 8.3 短名概念，原样返回长名
- 文本按 UTF-8 读写；`FileOpen` 的 `$FO_UNICODE` 系列标志被接受但按 UTF-8 处理
- 控制台输出不算"修改状态"的副作用，两种配置下都会写出（可重定向）

> **不静默编造值**：注册表、COM、`DllCall`、GUI、剪贴板等 Windows 专有函数在非 Windows
> 上**没有桩**，会如实报 `undefined function`；平台层未提供前同样如此。

### 正则表达式（平台无关）

`StringRegExp` / `StringRegExpReplace` 用**纯 Rust 的 `regex` 引擎**实现（`regexp.rs`），
不依赖 PCRE/C，因而不放进平台层——正则与操作系统无关，放平台层只会造成两套实现。

支持 AutoIt 文档中日常用到的全部要素：字面量、`.`、字符类与 POSIX 类、
`\d \s \w \b`、锚点、量词（含懒惰 `?`）、分支、捕获/命名/非捕获组、
`(?imsxU)` 选项组，以及模式头部 `(*UCP)`/`(*CRLF)` 之类的全局设置（会被剥离）。

`regex` 是有限自动机引擎，PCRE 的**回溯专有特性不支持**：环视 `(?=)`/`(?<=)`、
反向引用 `\1`、原子组 `(?>)`、占有量词、条件与递归。这些会**如实报错**
（`@error = 2` 坏模式），而不是近似匹配。

`offset` 参数语义正确（1-based，从该位置**开始搜索**）；`@extended` 在 flags 1/2 下
报告匹配结束后的下一个位置，在 `StringRegExpReplace` 下报告替换次数。

## 测试

`crates/autoitv3-ast/tests/integration.rs` 覆盖：

- 词法：token 种类、字符串转义、`#指令`整行（含 CRLF 处理）、复合赋值/三元
- 解析与 AST 结构：顶层条目、Global/Const、赋值/复合赋值、三元、函数与参数、
  单行/多行 If、For-In / For-To-Step、Select/Switch/With、数组字面量、Enum、
  叠用作用域关键字、`$arr[i](...)` 索引调用、错误位置报告
- 打印：注释去除、round-trip（重解析条目数/函数数一致）、整份混淆脚本冒烟测试

### 需要样本的集成测试（可选）

少数集成测试需要一个真实混淆脚本作为输入。它们默认**跳过**，只有当环境变量
`AU3_SAMPLE` 指向一个可读文件时才运行：

```bash
AU3_SAMPLE=/path/to/obfuscated.au3 cargo test
```

涉及：`autoitv3-ast`（整份脚本冒烟解析）、`autoitv3-deobf`（全量函数表解析，1108 项）、
`autoitv3-runtime`（用解释器执行函数表构建函数）。

## 验证

对一份真实的混淆样本：

- 解析成功，顶层条目与函数都识别出来
- pretty 规范化输出可被重新解析（round-trip 一致）

## 命名对应

| 概念 | 名称 |
| ---- | ---- |
| 项目/工作区 | `autoitv3-tools` |
| AST 分析库 crate | `autoitv3-ast`（lib 名 `autoitv3_ast`） |
| CLI crate | `au3-cli` |
| CLI 可执行产物 | `au3` |
