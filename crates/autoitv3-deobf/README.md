# autoitv3-deobf — 反混淆 pass 库

**反混淆 pass 库**：常量折叠、函数表解析、间接调用简化、确定性重命名，以及唯一能解开字符串表的运行时求值（`evaluate`）。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
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
        deobf.rs      反混淆 pass 单元测试（35 项）
        table_test.rs 函数表解析测试（最小 + 全量样本，2 项）
        evaluate_test.rs 运行时求值测试（25 项，含可选 AU3_SAMPLE 集成测试）
        unit/rename.rs `#forceref` 重写的单元测试（`#[path]` 回挂）
```

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
- **带下标读取只在"没有任何地方给这个名字赋值"时内联**。混淆器的表建成后确实不再变，
  但同一份脚本也把**运行时句柄**放在普通全局里（比如 `DllOpen` 的句柄、CryptoAPI 的
  provider 句柄，各自通过 `Func H() Return $g_state[1] EndFunc` 读回来）。把求值那一轮
  看到的句柄冻成常量，产物就只能在求值时的那个环境里跑：模拟层恰好返回同一个句柄所以
  看不出问题，原生 Windows 返回的真实句柄不同，后续调用立刻失败。判定按函数作用域做
  （参数、`Dim`/`Local`/`Static`、`For` 计数器算局部，不遮蔽全局），所以
  `Global $t = Build()` 这种"只被声明写过一次"的表照旧内联
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
deobfuscated: <F> folds, ... ; table: <T> entries, <C> calls, <R> refs; 0 indirect calls simplified
note: <N> global(s) are built by a function call at load time; re-run with --evaluate to run the script body and inline their values
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
