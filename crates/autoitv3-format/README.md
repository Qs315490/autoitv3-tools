# autoitv3-format — 规范化打印（pretty）

**规范化打印**（原名 pretty）：把 AST 重新打印为缩进统一的 AutoIt 源码，`au3 pretty` 与反混淆输出都走这里。AST 定义见 [`autoitv3-ast`](../autoitv3-ast/README.md)。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-format/         # 库 crate——格式打印（原名 pretty）
      src/lib.rs    把 AST 重新打印为 AutoIt 源码（默认保留注释，可 strip；规范缩进）
      tests/
        format.rs    格式化/注释保留/Else/空参数括号/字符串定界符测试（14 项）
```

## 字符串字面量：挑值里没有的那个定界符

AutoIt 的字符串用 `"..."` 或 `'...'` 都行，转义就是把当前定界符写两遍（官方帮助原文：
`"here is a ""double quote"""`）。反混淆会内联大量 JSON 与正则，里面几乎全是双引号，
所以打印时**挑值里没有的那个定界符**：

```autoit
Local $o = JsonParse('{"a": 1, "b": 2}')      ; 而不是 "{""a"": 1, ""b"": 2}"
```

两种写法是同一个字符串；`""` 那种只是满屏引号、容易被看错成空串。值里两种引号都有时没有
选择，退回双引号形式（只把 `"` 写两遍）；反斜杠在 AutoIt 里不是转义字符，原样保留。测试
对这几种情况都断言了"打印出来再解析回同一个值"。

> 注意 `autoitv3-unpack` 里自己拼字符串 token 的那条路**不**改：`au3 unpack --script`
> 要跟原脚本逐字节一致，那里仍然照原样写 `""`。

