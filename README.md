# autoitv3-tools (workspace)

AutoIt v3 词法/语法分析工具集：产出**带源码位置（Span）的 AST**，作为反混淆分析的基座。
代码结构分层，便于后续叠加功能（常量折叠、断点调试、解释执行器等）而无需改动核心。

采用 **Cargo workspace**：AST 作为可复用的库 crate，CLI 作为独立二进制 crate 调用库。

> 内置函数的实现进度与待办看板见 [`docs/任务看板.md`](docs/任务看板.md)，
> 逐函数状态表见 [`docs/函数实现状态.tsv`](docs/函数实现状态.tsv)
> （按「通用」与「需要仿真」分类，并按目标样本的调用频次排优先级）。

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
        format.rs    格式化/注释保留/Else/空参数括号测试（6 项）
    autoitv3-runtime/        # 库 crate——AutoIt v3 运行时（值模型 + 解释器 + 扩展接口）
      src/
        value.rs      运行时值模型（Int/Float/Str/Array/Map/Binary/FuncRef）与 AutoIt 强制转换规则
        interp.rs     Runtime 解释器：加载程序、调用函数、求值表达式、执行语句、停机交还控制权
        builtins.rs   已实现的内置函数子集（字符串/数值/位运算/数组/Map/Execute/Call...）
        host.rs       Host trait——嵌入方接入原生函数的接口（优先级高于平台层）
        platform/     Platform trait（仅接口；实现见 autoitv3-platform）
        profile.rs    执行配置：忠实语义 vs 确定性分析语义（见下文「执行配置」）
        regexp.rs     StringRegExp* ——基于纯 Rust regex 引擎，平台无关
        debug.rs      Debugger / DebugHost / Breakpoint / FrameInfo——调试接口（`au3 debug` 的实现端）
        error.rs      RuntimeError 与控制流信号 Flow
        lib.rs        公共 API
      tests/
        runtime.rs    解释器/host/debug 接口 + 可选样本集成测试（32 项）
        regexp.rs     StringRegExp / StringRegExpReplace（27 项）
    autoitv3-platform/       # 库 crate——平台层（分层：仿真 + 通用 + 系统）
      src/
        lib.rs        Platform 分层组合（CompositePlatform）、host_platform() /
                      host_platform_with() 工厂、runtime_with_platform() 便捷构造
        common/       通用层：文件/目录 I/O、INI、环境变量、数学、计时器、控制台
                      + 进程执行与网络 —— Linux 与 Windows 都安装
          mod.rs        CommonPlatform：直接分发 + 委托给下面两个服务
          proc.rs       Run/RunWait/ProcessWait*/StdoutRead…（std::process）
          net.rs        Inet*/TCP*/UDP*/Ping/代理设置（std::net）
        linux/        系统层（Linux）：/proc 进程查询、OS 标识宏
          mod.rs
        windows/      系统层（Windows）：注册表/COM/DllCall/GUI 扩展点（骨架）
          mod.rs
        winemu/       Windows 仿真层（非 Windows 主机；见下文「Windows 仿真」）
          mod.rs        WindowsEmulation：宏表、DllCall/注册表/剪贴板/驱动器分发
          version.rs    WindowsVersion / WindowsArch：选定仿真系统版本（默认 win10）
          paths.rs      WindowsPaths：C:\ 目录布局（@WindowsDir、@AppDataDir…）
          dllstruct.rs  DllStruct* 定义解析与按字段读写（OSVERSIONINFO 等）
          registry.rs   RegistryStore 接口 + FileRegistry（默认，落盘 .au3_registry）
                        + MemoryRegistry（可选，不落盘）
          pe.rs / compress.rs / crypto.rs  PE 资源、LZNT1、CryptoAPI 仿真
          verinfo.rs    PE RT_VERSION 解析（FileGetVersion）
          shortcut.rs   .lnk Shell Link 读写（FileCreateShortcut/FileGetShortcut）
          shell.rs      ShellExecute*/RunAs*
          gui/          GUI 无头语义：messages.rs（$EM_*/$LVM_* 默认）、
                        mod.rs（165 个 GUI 函数的 AutoIt 语义；控件模型与
                        GuiBackend 接缝在 autoitv3-gui crate）
      tests/
        platform.rs   分层、选择、注入、通用函数与宏（33 项）
        profile.rs    执行配置（忠实 / 确定性）（14 项）
        winemu/       Windows 仿真层（DllStruct / CryptoAPI / LZNT1 / PE 资源 / 注册表 …）
    autoitv3-gui/            # 库 crate（零依赖）——GUI 控件模型 + 后端接缝
      src/model.rs           Window/Control/GuiModel、绘制指令、$GUI_* 状态位
      src/backend.rs         GuiBackend trait + HeadlessBackend + GuiEvent/GuiImage
    autoitv3-gui-egui/       # 库 crate——egui 渲染后端（feature "egui" 离屏，"window" 真窗口）
      src/widgets.rs         控件 → egui 控件的唯一映射：29 种 ControlKind 全画，
                             返回 Interaction（点击/输入/勾选/列表选择）
      src/render.rs          EguiBackend：把模型布局成帧
      src/raster.rs          egui 三角形 → RGBA 的 CPU 光栅器（无 GPU、确定性）
      src/png.rs             极简 PNG 写出（stored deflate，无依赖）
      src/live.rs            LiveBackend：真窗口（feature "window"），点击/编辑回灌
      tests/controls.rs      29 种控件逐一渲染 + 整窗画廊 + 确定性
      examples/live.rs       实时窗口示例：Label + Input + Button
    autoitv3-deobf/          # 库 crate——反混淆 pass（常量折叠 + 函数表解析 + 可选重命名）
      src/
        fold.rs        常量折叠：遍历 AST，把纯常量表达式交给 runtime 求值后内联
        rename.rs      确定性重命名：变量按 作用域_类型_序号（$g_int_000 /
                       $l_str_003 / $arg_arr_001），函数 fNNN（**只改脚本内
                       定义**的名字；内置函数与宏不动）；整趟 pass 可关（可复现）
        table.rs       函数表解析：用 runtime 执行 $fn_table 构建函数，把 $fn_table[0x..](...)
                       改写为真实函数名调用（解开函数间接层）
        simplify.rs    间接调用简化：Call("Foo", ...) / Execute("Foo(...)") 改写为
                       直接调用 Foo(...)，让藏在字符串里的目标现形
        evaluate.rs    运行时求值：跑脚本主体，把它算出来的表值内联回源码
                       （唯一能解开字符串表的途径）
        orchestrator.rs 按序执行 pass 流水线，产出 Deobfuscator/Report
        lib.rs
      tests/
        deobf.rs      反混淆 pass 单元测试（30 项）
        table_test.rs 函数表解析测试（最小 + 全量样本，2 项）
    autoitv3-unpack/         # 库 crate——资源打包载荷的解包（au3 unpack）
      src/lib.rs            7 阶段解码 + 资源角色自动识别（loader/member 靠
                            SHA1 校验确认，不做名字假设）
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
          deobfuscate.rs au3 deobfuscate（DeobfuscateArgs + run，含 --evaluate）
          evaluate.rs   au3 evaluate（EvaluateArgs + run）
          run.rs        au3 run（RunArgs + run，含 --trace 用的 Debugger 示例实现）
          debug.rs      au3 debug（DebugArgs + 交互式 shell：命令解析、步进策略、提示符）
      tests/
        debug.rs     端到端驱动真实二进制：断点/单步/条件/求值/重启/stdin（13 项）
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
3. **调试接口**（`debug.rs`）——`Debugger`（每条语句回调，可返回
   `Continue`/`Pause`/`Abort`；停顿时 `on_stop` 拿到活的 `DebugHost`）、
   `Breakpoint`/`Breakpoints`（含条件）、`FrameInfo` 调用栈快照、`StopReason`。
   解释器每执行一条语句都会调用该接口，因此交互式调试器（`au3 debug`）、DAP 服务端
   或自动化 tracer 都能直接接上。

## 反混淆现状

`au3 deobfuscate` 现在执行 4 个 pass：

1. **常量折叠**（fold）：求值纯算术/字符串/拼接，原地内联。
2. **间接调用简化**（simplify）：`Call("Foo", ...)` 改写为直接调用，并把
   `Execute("<表达式>")` 的字符串代码**内联进 AST**（见下文「间接调用简化」）。
3. **函数表解析**（table）：静态执行 `BuildFunctionTable()`（纯数组构建，
   `Local $x[]=[...]` + `MergeArrays` + `Return`）得到 `$fn_table` 函数表
   （上千个函数名），把所有 `$fn_table[0x..](args)` 改写为 `FuncName(args)`、
   `$fn_table[0x..]` 改写为 `FuncName`。在一次完整运行中改写上万处引用。
   表名按 AutoIt 语义**大小写不敏感**匹配——样本代码里写 `$fn_table`，而
   `Execute` 字符串里写 `$FN_TABLE`。
4. **标识符重命名**（rename，**默认关闭**）：加 `--rename` 才做，别名自带**作用域**与
   **推断类型**（见下文「标识符重命名」）。默认保留原始名字，输出与输入还能对得上。

simplify 必须排在 table **之前**：把 `Execute("<代码>")` 摊平成普通代码，table 才看得见
`$FN_TABLE[1094](...)` 并把它解析成真实函数名；rename 最后跑，于是字符串里提到的
`$FN_TABLE`/`$name_table` 会和它们的定义拿到同一个别名（否则改名后的脚本里，这些动态调用
指向的变量已经不存在了）。

### 间接调用简化（simplify）

混淆器常用"代码放在字符串里"的方式藏调用目标，静态看不出调用图：

```autoit
Call("Foo", 1)                          ; 调用 Foo(1)
Execute("Foo(1)")                       ; 求值该字符串，调用 Foo(1)
Execute("$FN_TABLE[1094]($name_table[175])")  ; 调用函数表第 1094 项
```

能静态去掉的间接就去掉：

```autoit
Call("Foo", 1)                          ->  Foo(1)
Call("Foo")                             ->  Foo()
Execute("Foo(1)")                       ->  Foo(1)
Execute("$FN_TABLE[1094]($name_table[175])")  ->  $FN_TABLE[1094]($name_table[175])
                                            （随后 table 解析成真实函数名）
```

- **`Call`** 只在该名字是字面量、且指向**脚本自己定义**的函数时改写；大小写不敏感，
  重写用定义处拼写（`Call("foobar")` → `FooBar()`）。
- **`Execute`** 只要字符串能解析成**单个表达式**，就整个搬进 AST——不限于函数调用。
  这一步是关键：搬进来的表达式是普通代码，后面的 `table`、`rename` 一视同仁地处理它。
  `Execute` 是在**当前作用域**求值的（混淆器正是靠这点在字符串里引用 `$FN_TABLE`/`$name_table`），
  所以纯表达式内联后语义不变。
- **刻意不动**的情况：
  - `Call($name)` / `Execute($code)` —— 参数本身是算出来的；先跑 `--evaluate` 把字符串表的
    元素（`$string_table[42]`）内联成字面量，下一次就能处理；
  - `Call` 指向脚本里没有定义的名字，如 `Call("MsgBox", ...)`（没有内置函数表，无法验证直接调用，而且它本来就好读）；
  - `Execute` 的字符串**不是单个表达式**：赋值、多条语句、解析不了的代码都不动。AutoIt 里赋值只是**语句**（没有赋值表达式），`Execute("$x = 1")` 放进表达式位置后重新解析，`=` 会变成**比较**（`Local $v = ($x = 5)` 求值为 `true`），所以塞不进去；
  - 函数名不是整段字面量，如 `Call("Foo" & $suffix)` —— 目标随 `$suffix` 变化，静态不可知。若它其实可静态求值（全字面量拼接由 `fold` 折成 `Call("FooBar")`；`Global Const` 变量由 `evaluate` 内联），后面的 pass 折完照样能处理。

### 标识符重命名（rename）

变量别名形如 `$<作用域>_<类型>_<序号>`，三段信息一眼可读，且可 grep：

| 作用域 | 含义 |
| ------ | ---- |
| `g` | 脚本级：`Global` 声明，或顶层（函数外）的声明/赋值 |
| `l` | 函数内局部：`Local`/`Dim`/`Static`、`For` 循环变量，或函数内首次赋值 |
| `arg` | 函数参数（含 `ByRef`） |

| 类型 | 来源 |
| ---- | ---- |
| `int` / `float` / `str` / `bool` | 初始值或首次赋值的字面量 |
| `arr` | 数组字面量 `[...]`，或声明带维度 `Local $a[3]` |
| `map` | `Map()` |
| `var` | 静态推不出来（无初值、参数无默认值、运算结果等） |

```autoit
Global $count = 1              ->  Global $g_int_000 = 1
Func F($p, $ratio = 1.5)       ->  Func f000($arg_var_000, $arg_float_001 = 1.5)
    Local $name = "x"          ->      Local $l_str_000 = "x"
    Local $items[] = [1, 2]    ->      Local $l_arr_001[] = [1, 2]
    For $i = 1 To 10           ->      For $l_int_002 = 1 To 10
EndFunc
```

作用域是**静态推断**的，规则按 AutoIt 的实际语义来：

- `Global` 声明（无论在哪儿）与顶层声明/赋值 → 该名字在**任何位置**都用全局别名；
  函数内读一个脚本级变量必须保持同一别名，否则函数就看不到它了。
- 函数内的 `Local`/`Dim`/`Static`、`For` 变量 → 该函数自己的局部别名
  （与同名全局变量**不同**别名，因为它们是两个变量）。
- 函数内未声明就赋值 → 视为局部；但若该名字同时是脚本级变量，则仍用全局别名
  （AutoIt 的隐式规则是"读全局、写建局部"，同名才能保持行为不变）。
- 参数名在其所属函数内优先。
- **变量名大小写不敏感**（AutoIt 语义）：`$Foo`/`$foo`/`$FOO` 是同一个变量，
  必定得到同一个别名——否则重命名会改变行为。

类型只是可读性提示，纯静态推断、从不执行脚本；推不出来就是 `var`。

**函数与宏**：只重命名**脚本自己 `Func` 定义**的函数（改 `f000` 这类别名），
定义处与所有调用点一致；**内置函数**（`MsgBox`、`UBound`、`StringLen`…）和
**宏**（`@error`、`@CRLF`…）一律原样保留——它们是运行时按名字解析的，改名只会把
脚本改坏。

**重命名可选（默认不做）**：

```bash
au3 deobfuscate sample.au3              # 默认：常量折叠 / 函数表解析照做，名字全保留
au3 deobfuscate sample.au3 --rename     # 额外做确定性重命名
```

重命名**默认关闭**的理由：折叠、表解析、间接调用简化改变的是*结构*，而重命名改变的是
*名字*——一旦改名，输出就没法和输入（或任何引用它的东西）逐行对照了。需要 `$l_str_003`
这类自带作用域/类型的别名时再开。

库层同款默认：`Deobfuscator::new()` / `deobfuscate()` 只跑折叠、简化、表解析，
**不重命名**；要别名就显式用 `Deobfuscator::renaming()`：

```rust
use autoitv3_deobf::{deobfuscate, Deobfuscator, RenameOptions};

deobfuscate(&mut prog);                                           // 默认：不重命名
Deobfuscator::new().run(&mut prog);                               // 同上
Deobfuscator::renaming().run(&mut prog);                          // 完整流水线（含重命名）
Deobfuscator::renaming()
    .with_rename_options(RenameOptions { vars: true, funcs: false })
    .run(&mut prog);                                              // 只改变量
```

`Pass::DEFAULT` = `[Fold, Simplify, Table]`，`Pass::ALL` = 再加上 `Rename`。

### 运行时相关代码的迁移

原先 `autoitv3-deobf` 里自带两处"求值"逻辑，现已全部迁入 `autoitv3-runtime`：

- `table.rs` 曾手写一个数组字面量求值器来模拟 `MergeArrays`；现在直接把
  builder 交给解释器执行（`Runtime::call_function`），不再重复实现 AutoIt 语义。
- `fold.rs` 曾自带一套运算符求值（`apply_binary`/`neg`/`not`）；现在只负责
  遍历 AST 与判断"哪里可以内联"，实际求值交给 `Runtime::eval_expr`，
  并用 `is_constant_expr` 作为安全闸门（保证纯常量才内联）。

好处是 AutoIt 的运算符语义（强制转换、字符串拼接、整数/浮点提升）只有**一份**实现，
不会随两处代码各自演进而产生偏差。

> **字符串表求值**：`$string_table`（字符串表）由 `$fn_table[0x33d]()` 构建，它先用
> `DllStructCreate(OSVERSIONINFO)` + `DllCall(GetVersionExW)` 取系统版本，再把内嵌在
> PE 资源里的**加密**数据解开。整条链路 `winemu` 都已实现：`CryptAcquireContext` /
> `CryptCreateHash` / `CryptHashData` / `CryptDeriveKey` / `CryptDecrypt`
> （`CALG_RC4`、`CALG_AES_128/192/256`）、`RtlGetCompressionWorkSpaceSize` +
> `RtlDecompressBuffer`（LZNT1）以及 `FindResourceW` / `SizeofResource` / `LoadResource` /
> `LockResource`。真实脚本上 `$string_table` 现在**能完整建出来**，脚本体继续跑到 GUI 创建为止。
> 也就是说边界已经推到 **GUI/窗口层**，不再是 CryptoAPI。
> 资源镜像（编译后的 `.exe`）默认**自动查找**：先看脚本所在目录，再看当前工作目录，
> 优先选与脚本同名的镜像，否则选第一个带资源的 PE；用 `--resource-module <FILE>` 或
> `AU3_RESOURCE_MODULE` 可显式指定（显式指定优先，自动发现时会打印一行提示）。
> 用 `--no-win-emu` 可关闭仿真，回到"停在第一个 Windows 调用"的行为。
>
> `$name_table`（结构体/API 名表，`$fn_table[0x454]()`，即反混淆输出里的 `f001`）
> 同样被内联，`simplify` 再把 `EXECUTE($name_table[i])` 摊平成真实调用。

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

# deobfuscate：常量折叠 + 函数表解析 + 间接调用简化 + 去注释
#              统计信息走 stderr，stdout 保持为干净的 AutoIt 源码
au3 deobfuscate some.au3                  # 默认保留原始变量/函数名
au3 deobfuscate some.au3 --rename         # 额外做确定性重命名（$l_str_003 / f042）

# -o FILE 将输出写入文件；-o - 或省略 -o 则输出到 stdout（原文件永不被修改）
au3 pretty      some.au3 -o out.au3
au3 deobfuscate some.au3 -o -

# evaluate：跑一遍脚本主体，把它运行时算出来的表值内联回源码
#           （唯一能解开字符串表的途径；撞到平台边界时会报告并保留已求出的值）
au3 evaluate some.au3 -o resolved.au3
au3 evaluate some.au3 --faithful          # 按 AutoIt 语义真跑
# 也可以一步到位：先求值再做常规反混淆
au3 deobfuscate some.au3 --evaluate -o clean.au3

# 选定仿真的 Windows 系统版本（非 Windows 主机默认 win10；见「Windows 仿真」）
au3 evaluate some.au3 --win-version win11 -o resolved.au3
au3 evaluate some.au3 --win-version win7 --win-arch x86 -o resolved.au3
au3 evaluate some.au3 --no-win-emu        # 关掉仿真，停在第一个 Windows 调用

# run：用解释器调用函数（--arg 传参，--init 先执行脚本体以建立全局表）
au3 run Add --arg 2 --arg 3 some.au3
au3 run BuildFunctionTable --init some.au3
# --trace 打印解释器执行的语句流
au3 run SomeFunc --trace some.au3

# debug：加载脚本并进入交互式调试 shell（断点/单步/异常时停/查看/求值）
au3 debug some.au3
au3 debug some.au3 -c "break 68" -c run -c "print $string_table[0x4ea]" -c quit
au3 debug some.au3 -c "break 69; run; backtrace"     # -c 里可以用 ; 串多条
au3 debug some.au3 -x breakpoints.au3dbg             # 先跑命令文件，再把 prompt 交给你
au3 debug some.au3 --no-catch                        # 不在未捕获异常处停下
echo 'break 68
run
backtrace
quit' | au3 debug some.au3        # 管道同样可以驱动（不画提示符）
```

`--win-version` / `--win-arch` / `--no-win-emu` 三个开关同时适用于 `evaluate`、
`deobfuscate --evaluate` 与 `run`；省略时读环境变量，再回落到默认值：

| 开关 | 环境变量 | 默认 |
| ---- | -------- | ---- |
| `--win-version <VER>` | `AU3_WIN_VERSION` | `win10` |
| `--win-arch <ARCH>` | `AU3_WIN_ARCH` | `x64` |
| `--resource-module <FILE>` | `AU3_RESOURCE_MODULE` | 自动查找脚本旁的 PE 镜像 |
| （无开关） | `AU3_WIN_REGISTRY` | `./.au3_registry` |
| `--no-win-emu` | `AU3_WIN_EMU=0` | 启用（非 Windows 主机） |

| 子命令 | 别名 | 说明 |
| ------ | ---- | ---- |
| `parse <FILE>` | `p`, `check` | 解析并报告顶层条目/函数数量 |
| `pretty <FILE> [-o FILE]` | `fmt`, `format` | 规范化重打印，保留注释 |
| `deobfuscate <FILE> [-o FILE]` | `deobf`, `deob` | 反混淆流水线，去除注释（`--evaluate` 先做运行时求值；`--inline-tables` 顺带把表声明换成字面量；`--rename` 做标识符重命名，默认关闭；`--win-version` 等选仿真版本） |
| `evaluate <FILE> [-o FILE]` | `eval`, `e` | 跑脚本主体并内联其算出的表值（`--inline-tables` 顺带把表声明换成字面量；`--faithful` 按 AutoIt 语义；`--win-version`/`--no-win-emu` 控制仿真） |
| `run <FUNC> <FILE> [--arg V]… [--init] [--trace]` | `r`, `exec` | 解释执行一个函数（同样接受 `--win-*` 开关） |
| `debug <FILE> [-c CMD]… [-x FILE]…` | `dbg` | 交互式调试 shell：断点、单步、**未捕获异常时 post-mortem**、查看帧/变量、表达式求值（`--stop-at-start` 在第一条语句停下，`--no-catch` 关掉异常停） |
| `unpack <PATH> [-o FILE]` | `unp` | 解包资源打包的载荷（目录或 PE 都行，自动认角色，`--raw` 输出整段文本） |
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
cargo test                     # 全部（286 项，含 doctest）
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

## 设计说明（面向断点调试）

- 每个 `Stmt`、`Expr`、`Item` 都带 `Span { start: Pos, end: Pos }`，调试器可按行/列命中源码行。
- `Stmt` 是可执行的单元节点，解释器在每条语句前把 span 交给调试器（见下节 `au3 debug`）。
- 解析器与打印器分离：反混淆时可先打印出规范化文本，再对其做常量替换等变换。

## 运行时求值（`au3 evaluate`）

纯语法改写能解开**函数表**（`$fn_table` 完全由数组字面量拼成），但**字符串表**是运行生成的
代码（`Execute`、Map、`Binary`、字符串运算）算出来的——静态方法无解。因此提供运行时求值：

```bash
au3 evaluate sample.au3 -o resolved.au3     # 跑脚本主体，内联它算出来的值
au3 deobfuscate sample.au3 --evaluate -o clean.au3   # 求值 + 常规反混淆一步到位
```

实现（`autoitv3-deobf/src/evaluate.rs`）：跑脚本顶层主体 → 把每个**常量下标**的表引用
换成运行时真正得到的值：

```text
$name_table[0x38]    ->  2
$fn_table[0x33d]() ->  ResolvedFunc()      （函数名调用）
```

安全规则：

- **赋值左值不会被替换**（否则会写出 `0 -= 1` 这种非法语句），只替换其下标
- **裸变量读取仅在 `Global Const` 时内联**（可变全局可能被改写，内联其值会出错）
- 带下标读取视表为"建成后不再变"，这是混淆器的实际用法
- 含换行的字符串**不内联**（AutoIt 字面量无法表示换行，内联会导致输出无法解析）
- **函数参数默认值同样会被代入**：默认值在调用时求值，读的是同一批表

代入不是"一遍过"。`Simplify` 会把 `Execute("$FN_TABLE[1094]($name_table[175])")` 这类字符串
**拼接成真实代码**，而那批代码读的还是同一批表。所以 `deobfuscate --evaluate` 的流水线是
在 `Simplify` 处切开跑两段，中间再代入一次（`Deobfuscator::after_simplify` 标出切点）：

```
Fold, Simplify  →  再代入一次  →  Table, Rename
```

（`--inline-tables` 只影响这一步是否顺带改写表声明本身。）

少了这一步，`Execute` 拼出来的代码里会残留 `$table[i]` 引用 —— 真实脚本上正是如此：
`For $i = 1 To f084($name_table[175])` 这类 上百处引用（另有数十处 `$name_table[...]`）会留在输出里。

**表声明内联默认关闭**。所有读取代入之后，`Global Const $t = Build()` 就是这张表最后的
痕迹；把它换成值能让数据直接可见，但那张表可能很大（真实脚本的字符串表数千项、数十万
字符），而且声明本身记录了"表是怎么建出来的"。所以这是 `--inline-tables`（`evaluate` 与
`deobfuscate` 都有）显式开启的行为；不开时声明保持 `= Build()`，读取代入照常进行。

开启后 `Global Const` 的表声明会被写成字面量数组（嵌套数组、`Binary("0x…")` 都支持；
Map 没有字面量语法，保持原样）：

```autoit
Global Const $g_arr_026 = [3, "alpha", "beta", "gamma", _
    "delta", "epsilon", "zeta", ...]
```

超过 24 项的数组字面量会按每行 8 项用 AutoIt 的 `_` 续行折行（`WRAP_ARRAY_AFTER`）
—— 否则开启 `--inline-tables` 后输出里会出现一行数十万字符。

**不带 `--evaluate` 时**，`deobfuscate` 无法知道这些表的值（得跑脚本才知道），因此会在
摘要后提示还有多少个"由函数调用构建的全局"：

```
deobfuscated: 17280 folds, ... ; table: <T> entries, <C> calls, <R> refs; 0 indirect calls simplified
note: 7 global(s) are built by a function call at load time; re-run with --evaluate to run the script body and inline their values
```

### 部分求值是常态

真实脚本的启动代码很快会触碰操作系统（`DllCall`、注册表、GUI）——正是平台层标注的边界。
但混淆器**很早就把表建好**，所以中断的运行仍留下可用的表。因此求值失败时**保留已算出的值**
并报告停在哪里，而不是整体丢弃：

```
$ au3 evaluate sample.au3 -o resolved.au3
evaluated: <G> globals, <T> tables, <V> values inlined, <C> calls resolved
script body did not finish: undefined function: GUICREATE (at 10569:31)
  (that is the platform boundary: this function is not implemented for the current OS)
  values produced before that point were still inlined
```

在真实脚本上的实际效果（`au3 deobfuscate a.au3 --evaluate`）：

| 引用 | 求值前 | 求值后 |
| ---- | ------ | ------ |
| `$fn_table[...]`（函数表） | 上万 | **0** |
| `$name_table[...]`（名字表） | 数百 | **0** |
| `$string_table[...]`（字符串表） | 数万 | **0** |
| 输出里残留的表读取 | 上百 | **38**（全部是可变 `Global` 数组，本就不该内联） |
| 表声明 | `Global Const $t = Build()` | 默认保持原样；`--inline-tables` 时 **`= [ ... ]`** |
| 目录里没有资源镜像 | — | 资源调用返回 `0` + `@error = 1`（诚实边界） |

跑完的规模：`evaluated: <G> globals, <T> tables, <V> values inlined` →
`deobfuscated: <F> folds, <V> vars, <N> funcs renamed; table: <T> entries, <C> calls,
<R> refs; <S> indirect calls simplified`。脚本体停在 `GUICreate`：GUI 不在仿真范围内，
但**在那之前求出的表都已经内联**（--evaluate 的设计即如此）。

## 交互式调试（`au3 debug`）

`au3 debug <FILE>` 加载脚本后**不立即运行**，进入一个 gdb/pdb 风格的 shell：`run` 启动
（再敲一次就是重启），在**断点、单步、以及未捕获异常**处停下；停下时可以查看调用栈、
当前帧的局部变量、全局变量，以及直接在**当前帧**里求值。

```text
$ au3 debug a.au3
(au3) break 69
Breakpoint 1 at line 69
(au3) run
Breakpoint 1, line 69
    69  $fn_table[0x439]($string_table[0x784], $string_table[0xa9a])
(au3:69:1) print $name_table[169]
"DataPswAlgo1"
(au3:69:1) next
(au3:69:1) info breakpoints
  1  line 69     enabled=y  hits=1
```

| 命令 | 说明 |
| ---- | ---- |
| `run` / `restart` | 启动脚本体；在断点处再敲一次 = 从头再来（断点保留） |
| `continue` / `c` | 继续到下一个断点 |
| `step` / `s` | 单步，进入函数调用 |
| `next` / `n` | 单步，不进入调用（停在同层或更浅的语句） |
| `finish` / `fin` | 跑到当前函数返回 |
| `until <line>` | 跑到某一行 |
| `break <line> [if <expr>]` / `b` | 断点，可带条件 |
| `delete [id]` / `enable` / `disable` | 增删与开关断点 |
| `print <expr>` / `p` | 在当前帧求值（`p $string_table[0x4ea]`、`p Add(1,2)`） |
| `set $x = <expr>` | 在当前帧赋值，**会真的改到正在跑的程序** |
| `info breakpoints\|locals\|globals\|functions` | 查看断点/局部/全局/函数 |
| `backtrace` / `bt` / `where` | 调用栈（`#0` 为最内层） |
| `list [line]` / `l` | 看停点附近的源码，`=>` 标出当前行 |
| `trace on\|off` | 打开后逐条打印执行的语句 |
| `catch on\|off` | 未捕获异常时是否停下（默认 on） |
| `source <file>` | 把一个命令文件的命令插到队首执行，然后回到提示符 |
| `quit` / `q` | 退出 |

### 未捕获异常时停下（post-mortem）

`RuntimeError` 一旦抛出就没人会接住（AutoIt 没有 try/catch，解释器把它当作终止），
所以"未捕获异常"就是"任何运行时错误"。默认会**在抛出错误的那条语句处停下**：

```text
(au3) run
[uncaught error] index 5 out of bounds (len 2) (at 6:12)
     6      Return $list[$n]
(au3:6:5) backtrace
#0  Boom at 6:5
#1  Outer at 11:5
(au3:6:5) info locals
list = Array[2] {"1", "2"}
n = 5
x = 10
(au3:6:5) print $x
10
```

关键在**停的位置**：钩子挂在 `exec_stmt` 的错误返回路径上，而抛出错误的那个栈帧要到
`call_user` 返回时才弹出，所以此刻 `#0` 那层的局部变量仍然活着——这正是 post-mortem
要有用的前提。同一个错误会向上穿过每一层 `exec_stmt`，`error_reported` 保证只上报一次。

- `catch off`（或命令行 `--no-catch`）关掉它，错误就只作为 `[script stopped: …]` 报出。
- 调试器主动中止（`quit`、在停点敲 `run` 重启）走的是独立的 `RuntimeError::Aborted`，
  **不会**被当成脚本异常，所以不会反过来弹出一个 post-mortem 停点。

### 停止是怎么实现的

`Debugger::on_stop` 是**从解释器内部**被调用的 —— Rust 栈还活着，提示符就跑在那里，
用户敲 `continue` 之后它才返回、解释器才继续。因此不需要把解释器改写成状态机或协程：
栈帧、局部变量、求值上下文全都是现成的。单步是 shell 自己的记账（`StepMode`），
解释器只负责"每条语句前问一次"，不做任何步进语义建模。

调试器在回调期间被 `take()` 出字段，因此它能拿到 `&mut Runtime`（`DebugHost`）来读帧、
求值和改断点；同一时刻 `in_debugger` 置位，保证 `print` 引起的嵌套执行不会递归进调试器。
shell 侧的句柄用 `try_borrow_mut`，所以"命令正在执行时又被递语句"这种情况会被安静地
当成 `Continue`。

### 两个容易踩的语义细节

- **断点条件按表达式解析**。AutoIt 里 `$i = 5` 单独成句是**赋值**，出现在表达式位置才是
  **比较**；条件若被当成语句，既会误触发又会改坏被调试的程序。因此条件一律走
  `Runtime::evaluate_expression`（借 `Return` 强制表达式语法），`$i = 5` 是比较。
- **`print` 同理**：`print $i = 3` 是比较，赋值请用 `set`。

### 命令来源：`-x` 文件、`-c`、stdin

命令来自**同一个队列**，顺序是：先用 `-x FILE` 里的命令，再用 `-c` 的命令，最后是 stdin。
外层循环与断点处的提示符都从这个队列取，所以 `-c run -c next -c 'print $x' -c quit`
的含义和字面一致 —— `run` 停下后，剩下的命令由提示符消费。

```bash
au3 debug a.au3 -x breakpoints.au3dbg          # 先跑常用断点，然后交回提示符
au3 debug a.au3 -c "break 69; run; print \$string_table[0x4ea]"
au3 debug a.au3 -x setup.au3dbg -c run -c quit # 文件 → -c → stdin，依次消费
```

- **`;` 分隔多条命令**（`-c`、`-x` 文件、交互输入都适用）。双引号内的 `;` 不切分，
  所以 `print "a;b"` 是一整条；AutoIt 的 `""` 转义也照旧。文件里空行与 `#` 开头的行忽略。
- **命令文件跑完不会退出**：stdin 是终端时继续给提示符，是管道时继续读管道，读到 EOF 才结束——
  `-x setup.au3dbg` 就是"先加载我惯用的断点，再把 prompt 交给我"。
- 会话中还可以用 `source <file>` 再塞一个文件进来（插到队首，优先于已排队的命令）。
- stdin 是管道时不画提示符，方便脚本化。

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

解释器核心、值模型与语言级内置函数都与平台无关。AutoIt 的函数库分成**通用**与
**系统相关**两部分，因此平台层是**分层**的（crate `autoitv3-platform`）：

| 层 | 模块 | 安装于 | 内容 |
| -- | ---- | ------ | ---- |
| 仿真 | `winemu/` | **仅非 Windows** | Windows 身份、路径、`DllStruct*`/`DllCall`、注册表、剪贴板、驱动器——让 Windows 目标脚本能在 Linux 上继续跑（见下文「Windows 仿真」） |
| 通用 | `common/`（`mod.rs` + `proc.rs` + `net.rs`） | **所有**平台 | 文件与目录 I/O（含 `FileFind*`、`FileGetPos`/`FileSetPos`/`FileSetEnd`、`FileGetEncoding`、`FileReadToArray`、`FileSetTime`）、INI（`Ini*` 7 个）、环境变量、数学、计时器、控制台，以及进程（`Run`/`ProcessWait*`/`StdoutRead`…）与网络（`Inet*`/`TCP*`/`UDP*`/`Ping`）——AutoIt 在各系统上行为一致的部分 |
| 系统 | `linux/` | 仅 Linux | `/proc` 进程查询（`ProcessList`/`ProcessExists`/`ProcessClose`）、OS 标识宏 |
| 系统 | `windows/` | 仅 Windows | 注册表、COM、`DllCall`、GUI（**骨架**，后续填充） |

`host_platform()` 按目标平台组装成 `CompositePlatform`：Windows 为
`common+windows`，其余平台为 `winemu+common+linux`（仿真层在最前，
因此它的宏会**有意覆盖**通用层的同名宏）。逐层查找；通用层在 Windows 上同样生效，
系统层只补真正系统相关的部分。需要显式指定仿真配置时用
`host_platform_with(WindowsEmulation::new()...)`。

`Platform` **trait** 留在 `autoitv3-runtime`（解释器调用的接缝），**实现**在此 crate。
依赖方向单向——运行时不知道任何具体操作系统——因此 `Runtime::new()` 默认**没有**平台层，
需要时用 `autoitv3_platform::runtime_with_platform(&prog)` 或 `rt.set_platform(...)` 安装。

查找顺序为 **内置函数 → Host → Platform**，嵌入方可用 `Host` 覆盖任何平台实现。

### 通用层已实现

| 类别 | 函数 |
| ---- | ---- |
| 文件 | `FileOpen`/`FileClose`/`FileFlush`/`FileRead`/`FileReadLine`/`FileWrite`/`FileWriteLine`（句柄表、模式标志 `$FO_READ`/`APPEND`/`OVERWRITE`/`CREATEPATH`）、`FileExists`、`FileGetSize`、`FileGetTime`、`FileGetAttrib`、`FileGetLongName`/`FileGetShortName`、`FileGetPos`/`FileSetPos`/`FileSetEnd`、`FileGetEncoding`、`FileReadToArray`、`FileFindFirstFile`/`FileFindNextFile`（搜索句柄与 `FileClose` 共用句柄表）、`FileSetTime`、`FileChangeDir`、`FileDelete`、`FileCopy`、`FileMove`、`FileSetAttrib` |
| 目录 | `DirCreate`、`DirRemove`、`DirGetSize`、`DirCopy`、`DirMove` |
| INI | `IniRead`、`IniWrite`、`IniDelete`、`IniReadSection`、`IniReadSectionNames`、`IniRenameSection`、`IniWriteSection`（行式解析，保留注释；写入走 `ExecutionProfile` 门控） |
| 环境 | `EnvGet`、`EnvSet`、`EnvUpdate` |
| 数学 | `Round`（半数远离零）、`Sqrt`、`Sin`/`Cos`/`Tan`/`ASin`/`ACos`/`ATan`（**弧度**）、`Log`、`Exp`、`Floor`、`Ceiling`、`Random`、`RandomSeed` |
| 计时 | `TimerInit`、`TimerDiff` |
| 控制台 | `ConsoleWrite`、`ConsoleWriteError`、`ConsoleRead` |
| 宏 | `@TempDir`、`@AutoItPID`、`@AutoItEXE`、`@WorkingDir`/`@ScriptDir`、`@UserName`、`@HomePath`/`@UserProfileDir`、`@AppDataDir`/`@LocalAppDataDir`（XDG）、`@DesktopDir`、`@MyDocumentsDir` |

### 通用层的进程与网络（`common/proc.rs` + `common/net.rs`）

`CommonPlatform` 除了直接分发上面的文件/环境/数学函数，还把进程与网络委托给
`common/proc.rs`、`common/net.rs` 两个子服务，因此它们是**同一个通用层**的一部分：

| 类别 | 函数 | 说明 |
| ---- | ---- | ---- |
| 执行 | `Run`、`RunWait` | `std::process`；`$STDIO_*` 标志决定是否接管标准流 |
| 标准 IO | `StdoutRead`、`StderrRead`、`StdinWrite`、`StdioClose` | 每条流一个后台读取线程，读操作永不阻塞；进程结束后 join，缓冲完整 |
| 进程 | `ProcessWait`、`ProcessWaitClose`、`ProcessGetStats`、`ProcessSetPriority` | `ProcessWait*` 超时为**秒**、0 表示无限（与 AutoIt 一致）；`ProcessGetStats` 读 `/proc/<pid>/status` |
| 网络 | `InetGet`、`InetGetInfo`、`InetGetSize`、`InetRead`、`InetClose`、`Ping`、`FtpSetProxy`、`HttpSetProxy`、`HttpSetUserAgent`、`TCP*`（9）、`UDP*`（7） | `Inet*` 仅明文 `http://`（无 TLS）；`TCP*`/`UDP*` 用 `std::net`，句柄放进各自的套接字表 |

> **执行配置门控**：启动进程、打开套接字、联网下载都属于外部副作用，在
> `ExecutionProfile::deterministic()` 下一律失败并置 `@error = 1`，也不会阻塞
> （`ProcessWait*` 立即返回）。只有 `faithful()` 才真正执行。

宏由**平台**提供（解释器只负责 `@error`/`@extended`/`@ScriptLineNumber`/`@NumParams`/
`@CRLF` 等纯状态与常量），因此 `@TempDir` 之类不再是空串。

### Windows 仿真（`winemu`）——非 Windows 主机上的 Windows 机器

AutoIt 是 Windows 工具，真实的 Windows 主机上 `windows/` 才是正解。但在 Linux/macOS
上分析 Windows 样本时，"如实报 `undefined function`"会让求值卡在第一个 Win32 调用上。
`winemu` 用一台**仿真机器**回答这些调用，让脚本继续跑：

| 区域 | 行为 |
| ---- | ---- |
| OS 身份 | `WindowsVersion` 决定 `@OSVersion`、`@OSType`、`@OSBuild`、`@OSServicePack`、`@OSArch`/`@ProcessorArch`/`@CPUArch`、`@AutoItX64` |
| 目录 | `WindowsPaths` 给出传统 `C:` 布局：`@WindowsDir`、`@SystemDir`、`@ProgramFilesDir`、`@HomeDrive`、`@TempDir`、`@AppDataDir`、`@LocalAppDataDir`、`@UserProfileDir`、`@StartMenuDir`、`@StartupDir`…… |
| 原生结构 | `DllStructCreate`/`GetData`/`SetData`/`GetSize`/`GetPtr`/`IsDllStruct`——定义解析器支持 `struct;…;endstruct`、常见整型/浮点/指针、`char`/`wchar` 数组、无名段、`align N`；句柄指向一块本层持有的字节缓冲 |
| 原生调用 | `DllCall(dll, rettype, func, type, arg…)`，已实现 `GetVersionExW`/`A`、`RtlGetVersion`、`GetVersion`、`GetSystemInfo`/`GetNativeSystemInfo` 以及几个无副作用的查询。返回 **AutoIt 风格的数组**（`[0]` = 返回值，其余为 by-ref 参数）——脚本普遍写 `$r = DllCall(...)` / `If @error Or Not $r[0]`，返回标量会让它们全部报类型错误；调用失败时按 AutoIt 语义返回 `0` 并置 `@error = 1` |
| 注册表 | `RegRead`/`RegWrite`/`RegDelete`/`RegEnumKey`/`RegEnumVal` 全部重定向到可插拔的 `RegistryStore` 接口。默认实现是 `FileRegistry`：注册表状态落在**工作目录的 `.au3_registry` 文本文件**里，读在加载时进入内存、写立刻回写文件；`MemoryRegistry`（不落盘）用 `with_memory_registry()` 选回 |
| 剪贴板 | `ClipGet`/`ClipPut` 落到**工作目录下的文件**（默认 `.au3_clipboard`，可用 `with_clipboard_file()` 改名） |
| 驱动器 | `DriveGetDrive`/`DriveGetType`/`DriveGetFilesystem`/`DriveGetLabel`/`DriveGetSerial`/`DriveSpaceTotal`/`DriveSpaceFree`/`DriveStatus`，默认一台 `C:`（`DriveSpec` 可配）；网络映射 `DriveMapAdd`/`DriveMapDel`/`DriveMapGet` 与 `DriveSetLabel` 维护本层的映射/卷标状态 |
| Windows 文件 | `FileGetVersion`（解析 PE `RT_VERSION`）、`FileCreateShortcut`/`FileGetShortcut`（读写真实 `.lnk` Shell Link）、`FileCreateNTFSLink`、`FileRecycle`/`FileRecycleEmpty`（落到 `.au3_recycle`，可用 `with_recycle_dir()` 改名）、`FileInstall`（磁盘文件或已加载模块的 `RT_RCDATA` 资源） |
| 回调 | `DllCallbackRegister`/`DllCallbackGetPtr`/`DllCallbackFree` 发放合成指针；`DllCallAddress` 无加载器，按边界失败 |
| 系统信息 / Shell | `MemGetStats`（固定机器画像，可复现）、`IsAdmin`（`AU3_WIN_ADMIN`/`with_admin()`）；`ShellExecute`/`ShellExecuteWait`/`RunAs`/`RunAsWait` 委托宿主进程，`Shutdown` 只记录请求 |
| COM | 无 COM 运行时：`ObjCreate`/`ObjCreateInterface`/`ObjEvent`/`ObjGet`/`ObjName` 返回 `0`/`""` 并置 `@error = 1`，`IsObj` 恒为 `0`（不编造对象） |
| GUI | `GUICreate`/`GUICtrlCreate*`/`GUICtrlSet*`/`GUIGetMsg`/`Win*`/`Control*`/对话框/托盘/输入/像素 共 165 项，全部在 `winemu/gui/` 的**内存控件树**上实现：控件=对象、句柄=整数、`GUIGetMsg` 无事件返回 `0`、`GUICtrlSendMsg` 对 `$EM_*`/`$LVM_*` 给默认值（未知消息置 `@error`）。渲染与事件是 `GuiBackend` 接口（模型与接缝在 `autoitv3-gui`），默认 `HeadlessBackend` 不画任何东西；`autoitv3-gui-egui` 提供**离屏**（`EguiBackend`，可出 PNG）与**真窗口**（`LiveBackend`）两种渲染，29 种控件全部落地（见下节）；`with_gui_events`/`with_gui_auto_close`/`with_gui_answers` 提供**脚本化事件**，让消息循环可确定终止、对话框不阻塞 |

**选定仿真系统版本**——`WindowsVersion` 有 `WinXp`/`WinVista`/`Win7`/`Win8`/`Win81`/
`Win10`/`Win11`，**默认 Win10**：

```rust
use autoitv3_platform::winemu::{WindowsEmulation, WindowsVersion};

let emu = WindowsEmulation::new().with_version(WindowsVersion::Win11);
let rt  = /* Runtime::with_program(&prog) */;
rt.set_platform(autoitv3_platform::host_platform_with(emu));
```

选择版本的三种方式（优先级由低到高）：代码里 `with_version()` → 环境变量
`AU3_WIN_VERSION` → CLI `--win-version`。同族的还有 `AU3_WIN_ARCH`/`--win-arch`
（`x86`/`x64`/`arm64`，影响指针宽度与结构体布局），以及
`AU3_RESOURCE_MODULE`/`--resource-module`（`FindResourceW` 从哪个 PE 镜像取资源，
不给就自动查找，见上文）。

#### 注册表落盘（`FileRegistry`）

注册表操作**重定向到文件**：默认路径 `./.au3_registry`（可用 `AU3_WIN_REGISTRY`
或 `with_registry_file()` 改）。行式 UTF-8，四个制表符分隔字段
（键 / 值名 / 类型 / 载荷），`KEY` 记录只有键没有值：

```text
# au3-registry v1
HKLM\SOFTWARE\Vendor            KEY
HKLM\SOFTWARE\Vendor    Name    REG_SZ      hello
HKLM\SOFTWARE\Vendor    Count   REG_DWORD   7
```

- **文件是记录，不是种子快照**：先铺按版本生成的种子（`CurrentVersion`、`Shell Folders`、
  会话管理器环境……），再把文件记录覆盖上去；回写时只写文件自己的记录与新写入，
  所以换 `--win-version` 后未被文件提及的键仍随版本更新。
  把真实机器抓下来的注册表放进这个文件，就是给仿真一台特定机器。
- **没写就不建文件**：`evaluate` 的确定性配置会拒绝 `RegWrite`，因此分析样本不会在
  工作目录留下文件；`--faithful` 真跑时才会落盘。
- `MemoryRegistry`（纯内存、不落盘）用 `WindowsEmulation::with_memory_registry()`
  选回；任何自定义 `RegistryStore` 仍可用 `with_registry()` 注入。
- 写入走"同目录临时文件 + rename"，写到一半崩溃不会留下半个注册表；`\`、`|`、
  制表符、CR/LF、NUL 都会被转义，`REG_MULTI_SZ` 用 `|` 连接（项内的 `|` 转义），
  因此任意文本都不会破坏记录边界。已知取舍：删除**种子里的**值不会跨运行记住
  （格式里没有墓碑记录）。

#### CryptoAPI 的密钥派生

`CryptDeriveKey` 不是"取摘要前 n 字节"那么简单。MSDN 写明：**当 hash 不属于 SHA-2 家族、
且目标算法是 3DES 或 AES** 时，CSP 会把摘要混进 64 字节 `0x36` 和 64 字节 `0x5c`，各自
用同一算法再 hash 一次，然后**拼接**两个摘要，取前 n 字节做密钥。正因如此，16 字节的
MD5 口令摘要才能填满 256 位的 AES 密钥 —— 真实脚本里那个 `0x6610`（AES-256）
走的正是这条。块密码沿用 CryptoAPI 的默认 CBC + 全零 IV；RC4 按"摘要前 n 字节"处理。
回归测试 `an_aes_key_wider_than_the_hash_uses_the_documented_expansion` 用样本里真实的
口令钉住了整条派生（MD5 → 32 字节密钥）。

S-box 与轮密钥只在每次解密开头算一次（此前是每个 16 字节块都重算，一份 700 KB
的载荷要 8 秒，现在是 0.03 秒）。这同时把真实脚本的 `--evaluate` 从 8.3 秒降到 0.2 秒。

`DllStruct` 的内存是 `Rc<RefCell<Vec<u8>>>`：`DllStructCreate($def, $ptr)` 会**映射**到
已存在的地址而不是另开一块，于是 `_Crypt_DecryptData` 那套"交给 `DllCall` 解密、
再用第二个 struct 从同一地址按实际长度读回明文"的写法才成立。写入经过 `write_at()`
（内部可变），所以经 `struct*` 参数回写的字节对两个视图都可见。

**边界仍然存在，而且是有意的**：仿真层不是 PE 加载器，没有 COM、没有窗口管理器、
不调用真实 DLL。因此
- 未列举的 `DllCall` 置 `@error = 1`、返回 `0`，把决定权交回脚本；`DllCallAddress` 同理；
- COM 内建函数存在但**可判定失败**（返回 `0`/`""` + `@error = 1`），不编造对象；
- GUI 已由 `winemu/gui/` 的**无头语义**回答（不再是 `undefined function`）；默认后端
  **不渲染**。可选 crate `autoitv3-gui-egui`（feature `gui-egui`）提供**离屏渲染 + PNG
  截图**；`window` feature 另有**实时窗口**后端，点击/输入会回灌 `GUIGetMsg`/`GUICtrlRead`；
- 写文件、回收站、驱动器映射、启动进程同样遵循 `ExecutionProfile`：确定性分析配置下被拒绝
  （`@error = 1`），也就不会生成 `.au3_registry` / `.au3_clipboard` / `.au3_recycle`；
- 用 `--no-win-emu` / `AU3_WIN_EMU=0` / `WindowsEmulation::new().disabled()` 可整体关闭，
  回到"停在第一个 Windows 调用"的诚实行为；`with_host_paths()` 则只让**目录**宏回落到
  主机路径（`@TempDir` 等仍可用于真实文件 I/O），Windows 专有宏照旧仿真。

### GUI 渲染后端（可选 `egui`）——离屏 + 截图

控件模型与 `GuiBackend` 接缝在零依赖的 `autoitv3-gui`；`autoitv3-gui-egui` 用 egui 把模型
布局成帧，再用自带的 **CPU 光栅器**渲染成 RGBA。**不依赖 GPU、不依赖显示服务器**，
因此可复现、可在 CI 里断言像素。

**`widgets.rs` 是控件 → egui 控件的唯一映射**，离屏与实时窗口共用，所以两边画的是同一套：
29 种 `ControlKind` 全部落地——`Label`/`Button`/`Input`/`Edit`/`Checkbox`/`Radio`/`Group` 用原生
组件，`List`/`Combo`/`ListView`/`TreeView` 画成可选中的列表（选择写回 `selection`），
`Progress`/`Slider`/`Updown` 读 `text`/`limit` 显示数值，`Graphic` 重放 `GUICtrlSetGraphic`
的 `DrawCmd`，`Pic`/`Icon`/`Avi`/`Obj`/`Date`/`MonthCal` 画带标题的占位框，
`Menu`/`MenuItem` 组成菜单栏。控件自带的字体、文字色、背景色和 `tip`（悬停提示）也一并生效；
脚本给的 `width`/`height` 会用作控件尺寸。绘制时返回 `Interaction`
（`Clicked`/`Menu`/`Text`/`Checked`/`Selected`），实时后端再把它变成事件或模型更新。

```rust
use autoitv3_platform::winemu::WindowsEmulation;
use autoitv3_gui_egui::EguiBackend;

// feature "gui-egui" 时也可用 WindowsEmulation::with_egui_backend()
let backend = EguiBackend::new()
    .with_size(800, 600)
    .with_screenshot("/tmp/frame.png");   // GUISetState 时落一张 PNG
let _emu = WindowsEmulation::new().with_gui_backend(Box::new(backend));
// … 跑脚本（GUICreate/GUICtrlCreate*/GUISetState…）后即可得到截图
```

也可直接拿 `EguiBackend::snapshot()`（RGBA）或 `screenshot(path)`；`with_screenshot` 只是把
这一步挂到 `present()` 上，省去把仿真层再取回来。

**中文（及其它 CJK）文字**：egui 自带的字体只有拉丁/希腊/西里尔与 emoji，没有汉字，
所以脚本控件里的中文会画成方块。这里**不打包**几 MB 的 CJK 字体，而是用机器上已有的：
先看 `AU3_GUI_FONT`（`.ttc` 合集用 `AU3_GUI_FONT_INDEX` 指定第几个面），否则依次试
`C:\Windows\Fonts\msyh.ttc`（微软雅黑）等 Windows 路径、Linux 的 `NotoSansCJK-*.ttc`/
`wqy-microhei.ttc`、macOS 的 `PingFang.ttc`，最后浅扫 `/usr/share/fonts` 等目录。
字体作为**最低优先级回退**装入，所以拉丁文字仍用 egui 自带字体。找不到时会打印一条提示
（"Chinese text will draw as boxes. Set AU3_GUI_FONT ..."）。实时窗口与离屏渲染器都会装它。

离屏渲染器自己维护字体图集缓存：egui 只在首帧发整张图集、之后用**局部补丁**加新字形，
所以缓存必须把补丁贴回去——否则"前几帧用过的字"能画，"后面才出现的字"会静默丢失。
这段逻辑现在是公开的 `apply_textures()`（与 `rasterize()`/`Texture` 一起导出，写自己的
光栅器时直接复用）。

egui 是**可选依赖**，默认 `cargo test` 不编译它：

```bash
cargo test -p autoitv3-gui-egui --features egui     # 离屏渲染 + PNG 的测试
cargo test -p autoitv3-platform --features gui-egui # 平台接线（脚本建窗→截图）
```

> `~/.cargo` 只读的环境需要把 `CARGO_HOME` 指到可写目录才能拉取 egui（见上文「受限环境」）。

#### 实时窗口（feature `window`）

`LiveBackend` 是交互版后端。winit 要求事件循环必须在**主线程**上创建（放到别的线程上会
直接 panic："Initializing the event loop outside of the main thread is a significant
cross-platform compatibility hazard"），所以 `LiveBackend::run` **占用主线程**跑 eframe，
把解释器放到工作线程——两边靠一个模型镜像 + 两个通道通信：

- **脚本 → 窗口**：`on_window`/`on_control` 更新镜像，窗口每帧读取；
- **窗口 → 脚本**：点击/菜单/列表选择进入 `poll()`（`GUIGetMsg` 消费），输入框/复选框/
  列表的改动通过 `take_updates()` → `GuiUpdate::{SetText,SetChecked,Select}` 回写模型，
  于是 `GUICtrlRead` 能读到用户刚输入/选中的内容。

窗口画什么、怎么交互都由 `widgets.rs` 决定，和离屏渲染完全一致；`LiveBackend::simulate`
可以脱离窗口喂一条交互，便于测试。脚本线程结束时窗口会自己关闭，因此 `run()` 一定返回，
不会留下一个没人更新的空窗口。

示例（需要显示服务器）：

```bash
cargo run -p autoitv3-gui-egui --features window --example live
# 窗口里：Label + Input + Button；点 Greet 打印 "Hello, <输入>!"，关窗结束脚本

cargo run -p autoitv3-gui-egui --features window --example live -- --titlebar
# 最小化时保留标题栏（双击标题栏 / 点标题栏按钮恢复），而不是让窗口离开屏幕

cargo run -p autoitv3-gui-egui --features window --example live -- --states
# 脚本自己驱动窗口的演示：WinMove → @SW_MAXIMIZE → @SW_RESTORE →
# @SW_MINIMIZE → @SW_RESTORE，每步打印 WinGetPos/WinGetState 看到的值；
# 之后停在消息循环里，等你点标题栏按钮或双击标题栏，控制台会打印脚本收到的事件

cargo run -p autoitv3-gui-egui --features window --example live -- --auto
# 不用人操作：脚本建完控件就返回，窗口自动关闭（验证接线用）
```

> `--states` 的第一步会打印**桌面尺寸**：实时窗口的**原生视口就是模拟桌面**，所以
> `@DesktopWidth`/`@DesktopHeight` 报的就是那个父窗口的分辨率（离屏渲染用画布尺寸，
> 无头分析回落到 1024×768 的假定显示模式）。窗口最大化时会把桌面矩形换成自己的几何
> （`WinGetPos` 于是返回桌面尺寸、`WinGetClientSize` 同理），并把原矩形记下来供
> `@SW_RESTORE` 还原——和 Windows 一致；之后你拖动父窗口改变分辨率，已最大化的窗口会跟着变
> 并收到 `$GUI_EVENT_RESIZED`。

示例里的 **Minimise** 按钮会 `WinSetState(@SW_MINIMIZE)`：默认模式下窗口消失、底部出现
恢复按钮；`--titlebar` 模式下窗口收成标题栏、双击恢复。

```rust
use autoitv3_gui_egui::LiveBackend;
use autoitv3_platform::winemu::WindowsEmulation;

let backend = LiveBackend::new("AutoIt GUI PoC");
backend.run(move |backend| {                    // 主线程；阻塞到窗口关闭
    let emu = WindowsEmulation::new().with_gui_backend(Box::new(backend));
    // … 在 emu 上跑脚本；GUISetState() 之后窗口出现
})?;
```

**模拟桌面**：实时窗口的原生视口（`AutoIt GUI PoC` 那层）就是被仿真机器的桌面，
`@DesktopWidth`/`@DesktopHeight` 读它，`@DesktopDepth`/`@DesktopRefresh` 给假定显示模式的
32/60；离屏渲染后端把画布当桌面，无头运行回落到 1024×768。`GuiBackend::desktop_size()`
就是后端报桌面的接口（不实现即用回落值），自定义后端记得转发它。

**窗口尺寸与状态**：每个模拟窗口就是脚本 `GUICreate` 的**客户区大小**（第一次画时按它
铺开，不再缩成内容大小），四个边和右下角的拖拽都能用，横竖都能拉。两个方向都通：

- **用户拖拽 → 脚本**：新尺寸回写模型，`WinGetPos`/`WinGetClientSize` 立刻反映，`GUIGetMsg`
  收到 `$GUI_EVENT_RESIZED`（-12）；
- **脚本 → 屏幕**：`WinMove`（`WinMove($h, "", x, y [, w [, h]])`，`-1` 表示那一维不动）
  真的会移动/缩放窗口，`WinSetState`/`GUISetState` 的 `@SW_HIDE`/`@SW_MINIMIZE`/
  `@SW_MAXIMIZE`/`@SW_RESTORE` 也照做——最大化的窗口铺满视口。

**用户拖动 = 拥有几何**：拖动边框会改大小、拖动标题栏会移动，两者都**回写模型**
（`GuiUpdate::Resize`/`Move`），所以 `WinGetPos`/`WinGetClientSize` 与屏幕一致；AutoIt 对"移动"
没有消息，脚本靠轮询。**拖动一个最大化的窗口（边框或标题栏）会先退出最大化**——和 Windows
一样先回到正常摆放位置，再让指针接管；为此实时窗口会记住"用户要求的那个状态"，
在脚本应用之前不回弹（否则中间几帧会被最大化规则拉回去）。

> **位置只在"真的挪了"时回写**：判断依据是**相邻两帧画出的位置**，不是"画出的位置 ≠ 模型里的
> 位置"。区别在于用户拖动后脚本要下一帧才应用：中间那几帧窗口还画在旧位置（最大化时就是
> 0,0），按后者会把 0,0 写回模型，正好抹掉 `Window::restore` 里记的原位置——表现为**最大化后
> 还原，位置停在 0,0**。同理，等脚本应用状态的那几帧，实时后端直接用 `Window::restore` 的原
> 矩形来画（`effective_window`），所以不会先闪一下最大化矩形再跳回原处。

**标题栏控件**：窗口标题栏右侧有 **最小化** 和 **最大化/还原** 两个按钮（最大化后同一个按钮变成还原），
点一下即可操作，脚本相应收到 `$GUI_EVENT_MINIMIZE`/`RESTORE`/`MAXIMIZE`。egui 只自带关闭按钮，
所以这两个是我们自己画的，但**样式与 egui 关闭按钮完全一致**：同样的 `spacing.icon_width` 方形尺寸、
同样的 `item_spacing.x` 间距、同样的 `fg_stroke` **线条画法**（无底色），悬停时同样按 `visuals.expansion`
略微放大——所以标题栏看起来还是一整条。实现用 `ui.interact` + 自绘（不进布局流，
因此不会影响 egui 对窗口尺寸的测量）。

**标题栏双击 = 最大化/还原**（Windows 的手势）。egui 默认是"双击折叠"（保留标题、藏掉内容），
这里特意关掉折叠；再双击一次（窗口已最大化或最小化）则还原。点在标题栏按钮上不算双击，
所以连点最小化不会"最小化又马上还原"。

**最小化的呈现可以选**（`LiveBackend::with_minimize_style`）：

| 取值 | 行为 |
| ---- | ---- |
| `MinimizeStyle::Hidden`（默认，**仿真**） | 窗口离开屏幕，和 Windows 一样；viewpor 底部出现一条 "Minimised: <标题>" 的**任务栏**，点一下恢复（脚本收到 `$GUI_EVENT_RESTORE`） |
| `MinimizeStyle::TitleBar` | 保留标题栏、只藏内容（egui 折叠的观感），窗口仍在屏幕上，**双击标题栏恢复** |

用户的其他状态操作也会告诉脚本：双击标题栏最大化 → `$GUI_EVENT_MAXIMIZE`(-6)，还原 →
`$GUI_EVENT_RESTORE`(-5)，任务栏恢复 → 同样是 -5；`WinGetState` 的位标志照旧跟着变
（它和 `@SW_*` 是两套值）。

`@SW_*`（0…11）现在是真正的宏，`WinSetState` 按 AutoIt 的 `@SW_*` 解释（注意它**不是**
`WinGetState` 的位标志：`@SW_MINIMIZE` 是 6，而 `WIN_MINIMIZED` 位是 16）。

> egui 的 `Window` 是按**内容**决定尺寸的（`Resize::end` 对窗口回退到内容尺寸），
> 所以内容不填满窗口时，拖动那一维会在下一帧弹回去——这正是"能左右拉、不能拉高"的
> 原因。`widgets.rs` 用"首帧按脚本尺寸、之后按上一帧画出的尺寸设下限、指针按下时放开
> 下限"来解决（见 `show_autoit_window` 的注释与 `tests/window_resize.rs`）。

**其它已知近似**（不是 bug，是模型里没有的信息）：控件按创建顺序纵向排列而不是按脚本的
绝对 `x`/`y`；模型没有保留菜单→菜单项的父子关系，所以每个菜单列出本窗口所有菜单项；
`Tab` 只画成一行标签而不是真正的分层面板；`ListView` 没有列宽/表头点击，`TreeView`
用前导缩进表现层级。`Win*`/`Control*`（`WinMove`/`ControlClick` 等）只改内存模型，
不会真的搬动或缩放这个窗口；托盘、像素与输入注入也还没接。

```bash
cargo test -p autoitv3-gui-egui --features window   # 后端管线 + 交互映射测试
```

## 解包资源（`au3 unpack`）

有些编译产物把载荷加密后放进 4 个 `RT_RCDATA` 资源（一个 loader + 三个 member），
只有它自己的脚本能读回来。`au3 unpack` 直接按该格式解码，**不需要 `.au3`，
也不需要那个几 MB 的 `.exe`**：

```bash
au3 unpack ./staged/          # 目录：AutoIt3Wrapper 落盘的 __NAME / __Res64/NAME / __ResImage/_NAME
au3 unpack build.exe          # 或者直接给 PE，自动枚举它的 RT_RCDATA
au3 unpack build.exe --raw    # --raw 输出拼接后的整段文本，默认一行一条
au3 unpack build.exe --table  # --table 带 1-based 索引编号
au3 unpack build.exe --at 152,1263,3147-3149   # 只取这几项
```

四个资源的**角色是自动认出来的**：包里没有名字标签，所以把每个候选依次当 loader、
每个有序三元组当 member 试，只有当整条链跑通——包括最后一层的 `SHA1(明文) == Hash`
——才接受。这一步保证了搜索不会被巧合骗到（错组合过不了摘要校验），也意味着
**资源名每次构建随机变化都不影响**：

```
$ au3 unpack build.exe
unpacked: loader <name>, members <a>/<b>/<c> (17 resources considered)
  4144 entries, 347887 bytes
```

实现放在独立 crate `autoitv3-unpack`：它是一个**打包器**的格式，不是通用 AutoIt
能力，所以不塞进解释器。解码链复用 `autoitv3-platform` 的 CryptoAPI 仿真
（`CryptDeriveKey` 的 HMAC 式扩展）和 PE 解析。

### 当作字符串表的独立基准

同一份载荷往往**就是脚本自己的字符串表**：脚本里那张 `$table[n]` 是它解出来再按序号取的，
所以解包结果第 N 行 = `$table[N]`（`[0]` 是条数，不是表项）。`--table` / `--at` 就是把这件事
做得顺手一点：

```bash
au3 unpack build.exe --at 152,1263        # 表项 152、1263 的原始值
```

于是**怀疑解释器把表算错了**时有一条不依赖解释器的对照链：拿 exe 独立解出表项，
和断点里 `print $table[152]` 的值逐项比。两边一致 → 表没问题，往别处找；
不一致 → 就是求值/解密那一段的问题。这条链在排查"脚本自己解不动自己的数据"时特别有用
——能先把"我们算错了"这个可能性排除掉。

### 资源从哪里读

编译后的 AutoIt 脚本把加密的表放进 **PE 镜像的 `RT_RCDATA` 资源**，
`FindResourceW` / `LoadResource` 再从那里取。有两种来源，**先文件夹、后镜像**：

1. **已提取的资源文件**（优先）。`AutoIt3Wrapper_Res_File_Add` 会把每个内嵌资源
   摊在脚本旁边，文件名可以从资源名反推：
   `__NAME`、`__Res64/NAME`、`__ResImage/_NAME`，最后才试裸名 `NAME`。
   在脚本所在目录和当前工作目录里依次找，大小写不敏感（`FindResourceW` 本来就是）。
   于是**只要有从 exe 提取出来的资源，就不需要那个几 MB 的 exe**。
2. **PE 镜像**（回退）。`--resource-module <FILE>` → `AU3_RESOURCE_MODULE` →
   自动查找：脚本所在目录、再当前工作目录，优先选与脚本同名的镜像，否则选第一个
   真正带资源的 PE（按文件名排序，保证可复现）。自动选中时会往 stderr 打一行
   `# resource module: ...` 提示。

两边都找不到时，`GetModuleHandleW`/`FindResourceW` 落回"未列举"分支
（`@error = 1`、返回 `0`），脚本自己决定怎么办 —— 边界可见，不编造数据。
`AU3_WINEMU_TRACE=1` 会打印每次"资源来自文件"的命中，便于确认读的是哪一份。

### 执行配置（ExecutionProfile）——近似行为按用途区分

解释器有两类调用者，诉求相反：**反混淆**要快、可复现、无害（`Sleep(60000)` 不能真等
一分钟，`Random` 不能每次不同，评估样本不该动磁盘）；**正常运行**要按 AutoIt v3 语义
（真的延时、真的随机、真的产生副作用）。写死任一种都会对另一类造成错误行为，因此这些
行为是一个**值**，由调用方选择：

```rust
rt.set_profile(ExecutionProfile::deterministic());  // 分析：快、可复现、拒绝写入
rt.set_profile(ExecutionProfile::faithful());       // 运行：AutoIt 语义
```

| 维度 | `deterministic()`（反混淆/分析） | `faithful()`（正常运行） |
| ---- | -------------------------------- | ------------------------ |
| `Sleep` | **直接返回**（`SleepPolicy::Skip`） | 真实延时（`Real`），另有 `Capped(时长)` 上限模式 |
| `Random` | **固定种子**，多次运行结果一致 | **OS 熵**，每次不同（`RandomSeed` 显式覆盖两者） |
| 副作用（文件写入/删除/建目录/改环境） | **拒绝**：调用失败并置 `@error = 1`（`EffectPolicy::ReadOnly`） | 真实执行（`Allow`） |
| 读取（`FileRead`/`FileExists`/`EnvGet`…） | 照常工作 | 照常工作 |

**默认是 `faithful()`**——库不应悄悄改变脚本的行为；反混淆相关代码（`deobfuscate`
的常量折叠与函数表解析）**显式**切到 `deterministic()`，保证输出可复现、不触碰机器。

CLI 的 `au3 run` 面向"探查混淆样本"，因此默认用确定性配置；需要按 AutoIt 语义真跑时加
`--faithful`：

```bash
au3 run F sample.au3              # 快速、可复现、不改磁盘
au3 run F --faithful sample.au3   # 真的 Sleep、真的随机、真的写文件
```

其余仍属**有意为之的近似**（与执行配置无关，已在模块文档逐条标注）：

- `FileGetTime` 返回 **UTC**（本地时区需要时区数据库），`YYYY/MM/DD HH:MM:SS` 格式与 AutoIt 一致
- `FileGetAttrib` 返回 `D`（目录）/`A`（普通文件），只读时加 `R`；Windows 专有的 `S`/`H` 无对应概念，不设置
- `FileGetShortName` 无 8.3 短名概念，原样返回长名
- 文本按 UTF-8 读写；`FileOpen` 的 `$FO_UNICODE` 系列标志被接受但按 UTF-8 处理
- 控制台输出不算"修改状态"的副作用，两种配置下都会写出（可重定向）

> **不静默编造值**：没有仿真的那些 Windows 专有函数（COM、GUI、窗口/控件、以及未列举的
> `DllCall`）在非 Windows 上仍然**没有桩**，会如实报 `undefined function` 或置
> `@error = 1`；`winemu` 只回答它真正实现的部分，且每一处近似都写在模块文档里。
> 关掉仿真（`--no-win-emu`）即可回到"非 Windows 一律 undefined function"的行为。

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
AU3_SAMPLE=/path/to/obfuscated.au3 cargo test --release
```

涉及：`autoitv3-ast`（整份脚本冒烟解析）、`autoitv3-deobf`（全量函数表解析（上千项）；
用 `--evaluate` 跑完整脚本体、断言加密表被解出）、`autoitv3-runtime`（用解释器执行
函数表构建函数）。整份样本的解释执行在 debug 构建下要几分钟，所以配上 `--release`
（8 秒左右）。脚本旁边的 `.exe` 会被自动发现，不需要额外设环境变量。

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
