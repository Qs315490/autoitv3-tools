# autoitv3-preproc — `#include` 展开

**`#include` 展开**：把 `#include` 指令换成被包含文件的顶层条目，递归处理嵌套包含、
`#include-once` 与各种源码编码。AST 定义见 [`autoitv3-ast`](../autoitv3-ast/README.md)，
`au3 run`/`debug`/`evaluate`/`deobfuscate` 的加载路径见
[`au3-cli`](../au3-cli/README.md)。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-preproc/         # 库 crate——#include 展开
      src/lib.rs    Includes（搜索路径）、expand（按指令位置拼接条目）、
                    decode/UTF-16 解码；8 项单元测试（含编译包含文件、环、UTF-16）
```

## 为什么拼 AST 而不是拼文本

AutoIt 的预处理是"把文件内容插到指令所在处"，拼文本同样能跑通，但会把**脚本自己的
行号**整体挪掉——诊断、断点、`au3 debug` 的源码视图都按行号工作，包含文件一多，脚本
第 3 行就不再是第 3 行。这里改拼**解析后的条目**：

```autoit
; main.au3                    ; consts.au3
#include "consts.au3"         ; Global Const $ANSWER = 42
$x = $ANSWER                  ; Func Helper() ... EndFunc
```

展开后 `main.au3` 的 `$x = $ANSWER` 仍是第 2 行，`consts.au3` 的常量与函数已经在
程序里；`au3 debug script.au3 -c "break 3"` 因此照旧命中脚本的第 3 行。

## 搜索顺序

帮助页给了两张表，原样实现：

| 形式 | 顺序 |
| ---- | ---- |
| `#include "file"` | 脚本所在目录 → 用户库（**倒序**）→ 标准库 |
| `#include <file>` | 标准库 → 用户库（正序）→ 脚本所在目录 |

「脚本所在目录」是**当前这个文件**所在目录（嵌套包含时是被包含文件自己的目录，不是
顶层脚本的）。「标准库」= 运行解释器所在目录 + `\Include`，再加常见 AutoIt 安装位置
（本工具旁边没有 `Include`，否则标准库只能靠 `-I` 指名）。「用户库」= `-I` /
`AU3_INCLUDE_PATH`（`;` 分隔），对应帮助页提到的注册表值
`HKCU\Software\AutoIt v3\AutoIt\Include`。

## 容错

| 情况 | 结果 |
| ---- | ---- |
| 找不到文件 | **警告**（列出搜过的目录），脚本继续跑 |
| 包含文件解析失败 | **错误**——它说什么都不知道，脚本没法当作跑过 |
| 编译过的 `.a3x` | 警告，不展开（读回来需要解包器，这一层不依赖它） |
| 循环包含 | 警告并跳过（官方靠 `#include-once` 断环） |
| 嵌套超过 64 层 | 错误，避免病态脚本无限递归 |

`#include-once` 按帮助页的意图生效：**先于**展开就登记，因此互相包含的两个文件不会
打转；没有它时同一文件被包含两次就是两次（官方此时多半报 "Duplicate function"）。
编码：UTF-8（带/不带 BOM）、UTF-16 BOM 按原样解码，其余按 Latin-1 兜底——AutoIt 自己
接受 ANSI 包含文件，ASCII 部分必须照常生效。
