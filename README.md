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
        pretty.rs 把 AST 重新打印为 AutoIt 源码（去注释、规范化，反混淆输出的基础）
        lib.rs    库入口，统一导出
      tests/
        integration.rs   库集成单元测试（24 项）
    autoitv3-deobf/          # 库 crate——反混淆 pass（常量折叠 + 标识符重命名）
      src/
        fold.rs        常量折叠：纯算术/字符串/拼接表达式原地求值内联
        rename.rs      确定性重命名混淆的变量/函数/宏为可读别名（可复现）
        orchestrator.rs 按序执行 pass 流水线，产出 Deobfuscator/Report
        lib.rs
      tests/
        deobf.rs      反混淆 pass 单元测试（13 项）
    au3-cli/                # CLI 二进制 crate（产物名为 au3）
      src/main.rs
```

## 使用

```bash
cargo build --release
# 统计信息（顶层条目数、函数数）
./target/release/au3 some.au3
# 规范化重打印（默认保留注释、统一缩进）——反混淆输出基础
./target/release/au3 --pretty some.au3
# 反混淆（常量折叠 + 标识符重命名 + 去除注释），输出可重解析的规范 AutoIt 源码
./target/release/au3 --deobfuscate some.au3
# 运行库的单元测试
cargo test -p autoitv3-ast
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
- 打印：注释去除、round-trip（重解析条目数/函数数一致）、整份混淆目标文件冒烟测试

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
