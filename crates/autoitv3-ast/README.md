# autoitv3-ast — AutoIt v3 AST 分析核心

AutoIt v3 **AST 分析核心**：手写词法器 + 递归下降分析器，产出带源码位置（Span）的 AST，并配一个可做规范化输出的打印器。是反混淆与解释器的共同基座。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-ast/            # 库 crate（可被下游依赖）——AutoIt AST 分析核心
      src/
        span.rs    位置(Pos)与区间(Span)——每个 AST 节点都带 Span，方便断点/源码映射
        token.rs   词法 token 定义（关键字、运算符、复合赋值、三元 ?:）
        lexer.rs   手写词法分析器：`#指令`整行、字符串""转义、0x 十六进制、$var/@macro
        vocab.rs   关键字词表（AutoIt 的编译器编号顺序）+ 大小写还原；
                   词法分析器与它交叉校验（唯一一份关键字表）
        ast.rs     AST 定义：Program / Item / FuncDef / Stmt / Expr / Lit / Call / IndexCall / Ternary / ArrayLit ...
        parser.rs 递归下降分析器：语句按行/冒号分隔，表达式用优先级爬升
        lib.rs    库入口，统一导出
      tests/
        integration.rs     库集成单元测试（29 项）
        syntax_coverage.rs 语法覆盖回归集（15 项，见下文「语法覆盖」）
        unit/vocab.rs      vocab 的单元测试（4 项，`#[path]` 回挂，见[根 README](../../README.md) 的「测试」）
```

## 作为库调用

```rust
// 打印器在 autoitv3-format crate（`au3 pretty` 的实现库）
use autoitv3_ast::parse;
use autoitv3_format::PrettyPrinter;

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
  `ContinueCase`（`Select`/`Switch` 的贯穿，解释器按 AutoIt 语义实现）、
  `With/EndWith`、`Return/Exit/ExitLoop/ContinueLoop`、`#forceref` 等函数内指令
- 声明：`Local/Global/Dim/Static/Const/ReDim`，多个作用域关键字叠加（如 `Static Local`，`Static` 优先级最高）
## 设计说明（面向断点调试）

- 每个 `Stmt`、`Expr`、`Item` 都带 `Span { start: Pos, end: Pos }`，调试器可按行/列命中源码行。
- `Stmt` 是可执行的单元节点，解释器在每条语句前把 span 交给调试器（`au3 debug` 的交互式 shell 见 [`au3-cli`](../au3-cli/README.md) 的「交互式调试」）。
- 解析器与打印器分离：反混淆时可先打印出规范化文本，再对其做常量替换等变换。
## 语法覆盖

AutoIt v3 的语法覆盖由 `crates/autoitv3-ast/tests/syntax_coverage.rs` 固化：
一份 80+ 条构造的清单（预处理指令、行继续符、字面量、运算符、语句、声明、函数、
对象/COM、宏与关键字），外加对**语义**敏感的定点断言（`=` 的上下文含义、
`ContinueLoop`/`ExitLoop` 区分、`ContinueCase` 是控制语句而不是标识符、`ReDim`、
`Enum Step`、块注释、`Volatile`、`With` 隐式主语、成员/方法调用形状）。清单同时包含
**必须被拒绝**的非法构造（嵌套 `Func`、单行 `If … Else`、孤立 `.`、未闭合字符串）。

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
