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
        regexp.rs     StringRegExp* ——基于纯 Rust regex 引擎，平台无关
        debug.rs      Debugger trait / Breakpoint / FrameInfo——后续 debug 模块的接口
        error.rs      RuntimeError 与控制流信号 Flow
        lib.rs        公共 API
      tests/
        runtime.rs    解释器/host/debug 接口 + 真实集成测试（32 项）
        regexp.rs     StringRegExp / StringRegExpReplace（27 项）
    autoitv3-platform/       # 库 crate——各操作系统集成（平台单独成 crate）
      src/
        lib.rs        host_platform() 工厂、runtime_with_platform() 便捷构造
        generic.rs    Linux（及其他非 Windows）：如实返回「未提供」
        windows.rs    Windows：注册表/COM/DllCall/GUI 的扩展点（当前为骨架）
      tests/
        platform.rs   平台选择、注入、OS 函数供给（4 项）
    autoitv3-deobf/          # 库 crate——反混淆 pass（常量折叠 + 函数表解析 + 重命名）
      src/
        fold.rs        常量折叠：遍历 AST，把纯常量表达式交给 runtime 求值后内联
        rename.rs      确定性重命名混淆的变量/函数/宏为可读别名（可复现）
        table.rs       函数表解析：用 runtime 执行 $fn_table 构建函数，把 $fn_table[0x..](...)
                       改写为真实函数名调用（解开函数间接层）
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
          deobfuscate.rs au3 deobfuscate（DeobfuscateArgs + run）
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
| `deobfuscate <FILE> [-o FILE]` | `deobf`, `deob` | 反混淆流水线，去除注释 |
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

解释器核心、值模型与可移植内置函数子集都与平台无关；AutoIt 自身的函数库大多是
Win32 的封装，因此**平台单独成一个 crate** `autoitv3-platform`：

| 模块 | 适用 | 内容 |
| ---- | ---- | ---- |
| `generic.rs` | Linux（及任何非 Windows 目标） | 不提供 OS 特有功能——AutoIt 是 Windows 工具，这里如实返回"未提供" |
| `windows.rs` | Windows | **扩展点骨架**：注册表、COM、`DllCall`、GUI、进程/窗口等后续在此填充 |

`Platform` **trait** 留在 `autoitv3-runtime`（它是解释器调用的接缝），**实现**放在
`autoitv3-platform`。依赖方向单向：平台 crate 依赖运行时，运行时不知道任何具体操作
系统，因此 `Runtime::new()` 默认**没有**平台层；需要时用
`autoitv3_platform::runtime_with_platform(&prog)` 或 `rt.set_platform(...)` 安装。

查找顺序为 **内置函数 → Host → Platform**，因此嵌入方可以用 `Host` 覆盖任何平台默认实现。

> **不再静默编造值**：内置函数层只保留**可移植**的中性调用（`Sleep`、`ConsoleWrite`、
> `Opt` 等）。注册表、COM、`DllCall`、GUI、剪贴板、进程控制等 OS 特有函数一律**不再**
> 返回假值——在平台层提供实现之前，它们如实报「undefined function」，避免悄悄污染
> 反混淆结果。

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
