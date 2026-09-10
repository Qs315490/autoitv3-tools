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
        integration.rs   库集成单元测试（22 项）
    autoitv3-format/         # 库 crate——格式打印（原名 pretty）
      src/lib.rs    把 AST 重新打印为 AutoIt 源码（默认保留注释，可 strip；规范缩进）
      tests/
        format.rs    格式化/注释保留/去除测试（3 项）
    autoitv3-runtime/        # 库 crate——AutoIt v3 运行时（值模型 + 解释器 + 扩展接口）
      src/
        value.rs      运行时值模型（Int/Float/Str/Array/Map/Binary/FuncRef）与 AutoIt 强制转换规则
        interp.rs     Runtime 解释器：加载程序、调用函数、求值表达式、执行语句
        builtins.rs   已实现的内置函数子集（字符串/数值/位运算/数组/Map/Execute/Call...）
        host.rs       Host trait——完整运行时接入原生函数（Win32/COM/GUI/DllCall）的接口
        debug.rs      Debugger trait / Breakpoint / FrameInfo——后续 debug 模块的接口
        error.rs      RuntimeError 与控制流信号 Flow
        lib.rs        公共 API
      tests/
        runtime.rs    解释器/host/debug 接口 + 真实集成测试（28 项）
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
    au3-cli/                # CLI 二进制 crate（产物名为 au3）
      src/
        main.rs     入口：收集 argv、分发、把 CliError 转成退出码
        args.rs     共用参数解析（输入文件、-o、--arg）与 CliError
        output.rs   输出目标：文件或 stdout（`-` 表示 stdout）
        commands/
          mod.rs        子命令注册表、dispatch 与帮助文本
          parse.rs      au3 parse
          pretty.rs     au3 pretty
          deobfuscate.rs au3 deobfuscate
          run.rs        au3 run（含 --trace 用的 Debugger 示例实现）
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

CLI 采用**子命令**形式（一级参数不带 `--` 前缀），每个命令一个模块：

```bash
cargo build --release
au3 help                                   # 查看全部命令

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

| 子命令 | 说明 |
| ------ | ---- |
| `parse <file>` | 解析并报告顶层条目/函数数量 |
| `pretty <file> [-o FILE]` | 规范化重打印，保留注释 |
| `deobfuscate <file> [-o FILE]` | 反混淆流水线，去除注释 |
| `run <Func> <file> [--arg V]… [--init] [--trace]` | 解释执行一个函数 |
| `help` | 帮助 |

退出码：`0` 成功，`1` 输入处理失败（解析/运行时错误），`2` 用法或 IO 错误。

```bash
# 运行库的单元测试
cargo test -p autoitv3-ast
cargo test -p autoitv3-runtime
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
