# autoitv3-tools (workspace)

AutoIt v3 词法/语法分析工具集：产出**带源码位置（Span）的 AST**，作为反混淆分析的基座。
代码结构分层，便于后续叠加功能（常量折叠、断点调试、解释执行器等）而无需改动核心。

采用 **Cargo workspace**：AST 作为可复用的库 crate，CLI 作为独立二进制 crate 调用库。

> 内置函数的实现进度与待办看板见 [`docs/任务看板.md`](docs/任务看板.md)，
> 逐函数状态表见 [`docs/函数实现状态.tsv`](docs/函数实现状态.tsv)
> （按「通用」与「需要仿真」分类，并按目标样本的调用频次排优先级）。

## 组件

| crate | 职责 | 文档 |
| ----- | ---- | ---- |
| `autoitv3-ast` | AutoIt AST 分析核心（词法/语法/Span/打印） | [README](crates/autoitv3-ast/README.md) |
| `autoitv3-format` | 规范化打印（`au3 pretty` 的实现库） | [README](crates/autoitv3-format/README.md) |
| `autoitv3-runtime` | 值模型 + 解释器 + 宿主/平台/调试器接缝 | [README](crates/autoitv3-runtime/README.md) |
| `autoitv3-platform` | 平台层：Windows 原生 + 通用层 + winemu 仿真 | [README](crates/autoitv3-platform/README.md) |
| `autoitv3-deobf` | 反混淆 pass（折叠/函数表/简化/重命名/求值） | [README](crates/autoitv3-deobf/README.md) |
| `autoitv3-gui` | GUI 控件模型 + `GuiBackend` 接缝（零依赖） | [README](crates/autoitv3-gui/README.md) |
| `autoitv3-gui-egui` | GUI 渲染后端：离屏 PNG / 真窗口 | [README](crates/autoitv3-gui-egui/README.md) |
| `autoitv3-unpack` | 编译脚本与资源载荷解包 | [README](crates/autoitv3-unpack/README.md) |
| `au3-cli` | `au3` 命令行（parse/pretty/deobfuscate/evaluate/run/debug/unpack） | [README](crates/au3-cli/README.md) |

每个 crate 的 README 收录了它的目录树、设计说明与用法细节（此前集中在本文件的
章节已按主题拆分过去）；执行配置、平台层语义、GUI 后端、解包格式等长文都在对应 crate 里。

## 测试

测试代码不进 `src/`：单元测试放在各 crate 的 `tests/unit/<模块>.rs`，源文件只留三行
回挂声明：

```rust
#[cfg(test)]
#[path = "../../tests/unit/script_keys.rs"]
mod tests;
```

这样实现文件只有实现，测试也能像内联 `mod tests` 一样访问模块私有状态——`tests/unit/`
是 `tests/` 的子目录，Cargo 只把 `tests/*.rs` 当集成测试目标，所以它不会变成一个只会
编译失败的独立 target。需要**公开 API** 才测得了的行为（解析、打印、运行时语义、
平台层）则照常写成 `tests/*.rs` 集成测试；两者都在 `cargo test` 的同一趟里跑。

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
AU3_SAMPLE=/path/to/obfuscated.au3 cargo test --release
```

涉及：`autoitv3-ast`（整份脚本冒烟解析）、`autoitv3-deobf`（全量函数表解析（上千项）；
用 `--evaluate` 跑完整脚本体、断言加密表被解出）、`autoitv3-runtime`（用解释器执行
函数表构建函数）。整份样本的解释执行在 debug 构建下要几分钟，所以配上 `--release`
（8 秒左右）。脚本旁边的 `.exe` 会被自动发现，不需要额外设环境变量。

解包也有一条同样的可选集成测试——而且它是**逐字节**的，不是"有输出就算过"：

```bash
AU3_UNPACK_SCRIPT=/path/to/build.exe \
AU3_UNPACK_EXPECTED=/path/to/source.au3 \
cargo test -p autoitv3-unpack
```

`AU3_UNPACK_SCRIPT` 可以是 `.exe`，也可以是已经 dump 出来的 chunk；
`AU3_UNPACK_EXPECTED` 给出编译时用的 `.au3`，测试会断言解包结果与它完全相同。

## 验证

对一份真实的混淆样本：

- 解析成功，顶层条目与函数都识别出来
- pretty 规范化输出可被重新解析（round-trip 一致）

对一份真实的 `aut2exe` 产物（`AU3!EA06`）：

- `au3 unpack build.exe --script` 输出与参考实现逐字节一致：**SHA-1 相同**
- 取回的源码能被本项目自己的解析器吃下
## 命名对应

| 概念 | 名称 |
| ---- | ---- |
| 项目/工作区 | `autoitv3-tools` |
| AST 分析库 crate | `autoitv3-ast`（lib 名 `autoitv3_ast`） |
| CLI crate | `au3-cli` |
| CLI 可执行产物 | `au3` |
