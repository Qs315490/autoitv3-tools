# autoitv3-format — 规范化打印（pretty）

**规范化打印**（原名 pretty）：把 AST 重新打印为缩进统一的 AutoIt 源码，`au3 pretty` 与反混淆输出都走这里。AST 定义见 [`autoitv3-ast`](../autoitv3-ast/README.md)。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-format/         # 库 crate——格式打印（原名 pretty）
      src/lib.rs    把 AST 重新打印为 AutoIt 源码（默认保留注释，可 strip；规范缩进）
      tests/
        format.rs    格式化/注释保留/Else/空参数括号测试（9 项）
```
