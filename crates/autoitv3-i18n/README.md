# autoitv3-i18n — 消息本地化（English / 简体中文）

整个工具**原生就写英文**：每条消息在调用点上就是英文原文，这段英文同时是**翻译键**。
译文放在 `src/catalog/*.rs` 的表里（`(英文, 中文)` 二元组）；当前语言是 `zh-CN` 且
表里有这条键就用中文，否则原样用英文——**缺翻译永远不会让消息消失或变形**，
纯英文构建就是这个 crate 的默认语言。

## 用法

```rust
use autoitv3_i18n::{msg, tr};

println!("{}", tr("no such file"));                              // 无占位符
println!("{}", msg!("cannot write {path}: {e}", path = path, e = e)); // 有值
println!("{}", msg!("literal text"));                            // 返回 String
```

- `tr(key) -> &'static str`：无占位符的消息。
- `msg!("… {name} …", name = expr) -> String`：占位符是**具名**的，名字就是调用点上
  参数标识符（宏用 `stringify!` 取），所以译文可以自由调整顺序；值按 `Display` 渲染。
  需要 `Debug`（例如给字符串加引号）时先自己格式化：`let shown = format!("{x:?}");`。
- `set_lang(Lang)` / `lang()`：进程级全局，和 locale 一样。
- `resolve(Option<&str>)` / `lang_from_env()`：`--lang` > `AU3_LANG` > `LC_ALL` /
  `LC_MESSAGES` / `LANG`；`auto`、空值、未设置都走环境；`zh*` → `zh-CN`，其它 → 英文。
- `AU3_I18N_STRICT=1` 时，每个**没有译文**的键会往 stderr 打一行
  `[i18n] no zh-CN translation: …`，用来手工跑一遍中文会话查漏。

## 目录

```text
  src/lib.rs              Lang、语言解析、tr/msg!/render、缺失回退与 strict 提示
  src/catalog/mod.rs      表清单（TABLES）与 lookup；一张表一个文件，方便并行翻译
  src/catalog/cli_help.rs 命令行帮助（clap 的 about/help，由 au3-cli 的 i18n_cli 走一遍）
  src/catalog/cli_debug.rs au3 debug 交互式 shell 的消息
  src/catalog/cli_run.rs  au3 的 run/parse/pretty/deobfuscate/evaluate/unpack 等命令
  src/catalog/runtime.rs  autoitv3-runtime（解释器/错误）
  src/catalog/platform.rs autoitv3-platform + autoitv3-unpack
  src/catalog/misc.rs     autoitv3-ast / -preproc / -deobf / -format 的消息
```

**一个表一个文件**是刻意的：两个人（两个 agent）翻译不同部分时不会改同一个文件。
新增表：建文件 → `catalog/mod.rs` 里 `pub mod` 并在 `TABLES` 里登记。

## 加一条翻译

1. 调用点用 `tr("...")` 或 `msg!("...", ...)` 包起来（英文原文不要改，测试和键都靠它）。
2. 在对应的 `catalog/*.rs` 里加一条 `("英文", "中文")`，键与调用点**逐字节一致**
   （跨行的 `\` 续行按 Rust 的取值算：接起来）。
3. 跑覆盖率测试，缺哪条会直接列出来。

## 什么不翻译

只翻译**给人看的说明性文字**。以下保持英文原样：

- 标识符、选项/枚举值名、占位符名（`<FILE>`）；
- 路径、类型名、值本身、`Debug` 输出、数值与统计；
- 命令的**输出**：生成的 AutoIt 源码（`pretty`/`deobfuscate`）、反汇编（`unpack`）、
  值渲染（`Array[2] {…}`）、JSON/TSV 等机器可读输出；
- 脚本自己的输出与回给脚本的数据（`ConsoleWrite`、`MsgBox` 文本、`@error` 相关文本）；
- 内部不变式消息（`panic!` / `assert!` / `expect!` / `unreachable!`）。

clap 自己写的脚手架（`Usage:`、`Options:`、`error:`、`tip:` …）由 `au3-cli` 的
`i18n_cli` 在渲染后逐行替换，常见错误句子也按前缀翻译；不认识的句子保持英文，
不会被翻坏。

另外两类保持原文：**操作系统给的错误串**（`std::io::Error` 的 `Display`，如
`No such file or directory (os error 2)`、`Access is denied`）由系统提供，语言跟着
平台/`LC_MESSAGES` 走，本 crate 不翻译；**宿主/引擎的错误文本**（如 PCRE 引擎的
正则错误）同理。

## 测试

`au3-cli` 的 binary 目标里有两个**覆盖率守卫**，跟着 `cargo test` 自动跑：
`clap_strings_are_translated` 走一遍 clap 的 Command 树、`source_strings_are_translated`
扫 `crates/*/src` 里所有 `tr()`/`msg!()` 字面量，只要有一个键没有译文就失败。
翻译前想看清单（而不是断言失败）时用那个 `#[ignore]` 的工具版本：

```bash
# 列出 clap 收集到的全部帮助文本（113 条，去重 59 条）
cargo test --offline -p au3-cli --bin au3 list_clap_strings -- --ignored --nocapture
```

`catalog/mod.rs` 的单元测试保证：键非空、译文非空、同一张表内键不重复、同一键不会
被译成两种说法。测试套件用 `.cargo/config.toml` 把 `AU3_LANG` 钉成 `en`，所以断言
英文文案的测试不受开发机 locale 影响。
