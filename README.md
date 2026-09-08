# au3-parser

AutoIt v3 的词法 / 语法分析器，产出**带源码位置（Span）的 AST**，作为反混淆分析的基座。
代码结构刻意分层，便于后续叠加功能（常量折叠、断点调试、解释执行器等）而无需改动核心。

## 结构

```
src/
  span.rs   位置(Pos)与区间(Span)——每个 AST 节点都带 Span，方便断点/源码映射
  token.rs  词法 token 定义（含关键字、运算符、复合赋值、三元 ?:）
  lexer.rs  手写词法分析器：`#指令`整行、字符串""转义、0x 十六进制、$var/@macro 等
  ast.rs    AST 定义：Program / Item / FuncDef / Stmt / Expr / Lit / Call ...
  parser.rs 递归下降分析器：语句按行/冒号分隔，表达式用优先级爬升
  pretty.rs 把 AST 重新打印为 AutoIt 源码（去注释、规范化，反混淆输出的基础）
  lib.rs    库入口，统一导出
  main.rs   CLI 入口
```

## 使用

```bash
cargo build --release
# 统计信息（顶层条目数、函数数）
./target/release/au3-parser some.au3
# 规范化重打印（去注释、统一缩进）——反混淆输出基础
./target/release/au3-parser --pretty some.au3
```

## 已支持的 AutoIt 语法

- 预处理器指令整行（`#include <file>`、`#NoTrayIcon`、`#AutoIt3Wrapper_...=...`）
- 字符串字面量（含 `""` 转义）、0x 十六进制数、浮点、`True/False/Default/Null`
- 变量 `$x`、宏 `@x`、数组下标 `$a[i][j]`
- 数组字面量初始化 `Local $a[] = [$x, $y]` 与 `Enum` 枚举
- 函数 `Func ... EndFunc`，参数 `Const/ByRef`、默认值
- 复合赋值 `+= -= *= /= ^= &=`、三元条件 `? :`
- 语句：`If/ElseIf/Else/EndIf`（含单行 Then）、`While/WEnd`、`Do/Until`、
  `For ... To ... Step/Next` 与 `For ... In .../Next`、`Select/Case`、`Switch/Case`、
  `With/EndWith`、`Return/Exit/ExitLoop/ContinueLoop`
- 声明：`Local/Global/Dim/Static/Const/ReDim`，多个作用域关键字叠加（如 `Static Local`）

## 设计说明（面向后续断点调试）

- 每个 `Stmt`、`Expr`、`Item` 都带 `Span { start: Pos, end: Pos }`，
  调试器可直接按行/列命中源码行。
- `Stmt` 是一个可执行的单元节点，未来解释器/调试器只需遍历语句并在命中断点位置暂停。
- 解析器与打印器分离：反混淆时可先打印出规范化文本，再对其做常量替换等变换。

## 当前限制 / 待办

- 对超大型混淆文件（如 `sample.au3`，~1.4MB）仍在逐步补齐极端写法；当前在若干行
  （例如涉及 `$arr[i]()` 数组函数引用调用等）的解析还有待完善。
- 尚不包含表达式求值/常量折叠（反混淆的下一步），已预留 `ExprKind` 节点便于实现。