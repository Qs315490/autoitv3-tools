# autoitv3-runtime — AutoIt v3 运行时

**AutoIt v3 运行时**：值模型、解释器、内置函数子集与宿主扩展接口（平台层 / Host / 调试器接缝）。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-runtime/        # 库 crate——AutoIt v3 运行时（值模型 + 解释器 + 扩展接口）
      src/
        value.rs      运行时值模型（Int/Float/Str/Array/Map/Binary/FuncRef）与 AutoIt 强制转换规则
        interp.rs     Runtime 解释器：加载程序、调用函数、求值表达式、执行语句、停机交还控制权
        builtins.rs   已实现的内置函数子集（字符串/数值/位运算/数组/Map/Execute/Call...）
        vocab.rs      内置函数 / 宏词表（AutoIt 的编译器编号顺序）+ 大小写还原；
                      解码编译脚本、校验平台层的 FUNCTIONS 都以它为准
        host.rs       Host trait——嵌入方接入原生函数的接口（优先级高于平台层）
        platform/     Platform trait（仅接口；实现见 autoitv3-platform）
        profile.rs    执行配置：忠实语义 vs 确定性分析语义（见下文「执行配置」）
        regexp.rs     StringRegExp* ——基于纯 Rust fancy-regex 引擎（含回溯特性），平台无关
        debug.rs      Debugger / DebugHost / Breakpoint / FrameInfo——调试接口（`au3 debug` 的实现端）
        error.rs      RuntimeError 与控制流信号 Flow
        lib.rs        公共 API
      tests/
        runtime.rs    解释器/host/debug 接口 + 可选样本集成测试（63 项）
        regexp.rs     StringRegExp / StringRegExpReplace（29 项）
        unit/vocab.rs 内置函数 / 宏词表的单元测试（3 项，`#[path]` 回挂）
```

## 定位

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

两个预设之上还可以**按效果类型**做细粒度开关：`EffectKind` 分 file / env / registry /
clipboard / spawn / shutdown / net / process 八类，`ExecutionProfile::with_effect(kind, allowed)`
在预设上叠加（如确定性运行放行注册表探测写入、忠实运行单独禁用 `Shutdown`）；
平台层的每个副作用门控点都经 `HostContext::effect_allowed(kind)` 查询该决策。
CLI 对应 `au3 run --allow <KIND>` / `--deny <KIND>`。

CLI 的 `au3 run` 面向"探查混淆样本"，因此默认用确定性配置；需要按 AutoIt 语义真跑时加
`--faithful`：

```bash
au3 run sample.au3                # 执行整个脚本体
au3 run sample.au3 F              # 调用函数 F（快速、可复现、不改磁盘）
au3 run sample.au3 F --faithful   # 真的 Sleep、真的随机、真的写文件
```

`--cmdline`（以及没写函数时的 `--arg`）由 `Runtime::set_cmdline` 变成脚本的
`$CmdLine`/`$CmdLineRaw`；写了函数时 `--arg` 是该函数的入参，`--cmdline` 仍供
`--init` 跑的脚本体读取。

`@Compiled` 由 `Runtime::set_compiled` 决定：CLI 以 `.exe`/`.a3x` 为输入时是 1、
`.au3` 是 0，脚本据此选择"重开 x64 进程 / 剥离自身命令行"的分支时与真实产物一致。
解出来的 `.au3` 想按产物那一侧跑（或反过来）时，`run`/`debug`/`evaluate`/`deobf`
都接受 `--compiled` / `--no-compiled` 覆盖这个默认值。
时钟宏 `@YEAR`/`@MON`/`@MDAY`/`@HOUR`/`@MIN`/`@SEC`/`@MSEC`/`@WDAY`/`@YDAY`
（解释器直接提供，不再落回平台的 `Null`）按 AutoIt 的零填充字符串格式返回；
确定性配置下是固定时刻，否则取宿主时钟。

其余仍属**有意为之的近似**（与执行配置无关，已在模块文档逐条标注）：

- `FileGetTime` 返回 **UTC**（本地时区需要时区数据库），`YYYY/MM/DD HH:MM:SS` 格式与 AutoIt 一致
- `@YEAR`…`@YDAY` 同样按 **UTC** 分解；格式（零填充、`@WDAY` 1=周日、`@YDAY`
  001-366）与 AutoIt 一致
- `FileGetAttrib` 返回 `D`（目录）/`A`（普通文件），只读时加 `R`；Windows 专有的 `S`/`H` 无对应概念，不设置
- `FileGetShortName` 无 8.3 短名概念，原样返回长名
- 文本按 UTF-8 读写；`FileOpen` 的 `$FO_UNICODE` 系列标志被接受但按 UTF-8 处理
- 控制台输出不算"修改状态"的副作用，两种配置下都会写出（可重定向）

> **不静默编造值**：没有仿真的那些 Windows 专有函数（COM、GUI、窗口/控件、以及未列举的
> `DllCall`）在非 Windows 上仍然**没有桩**，会如实报 `undefined function` 或置
> `@error = 1`；`winemu` 只回答它真正实现的部分，且每一处近似都写在模块文档里。
> 关掉仿真（`--no-win-emu`）即可回到"非 Windows 一律 undefined function"的行为。
