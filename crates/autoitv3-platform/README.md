# autoitv3-platform — 平台层（原生 Windows + 通用 + winemu 仿真）

**平台层**：解释器之外的"操作系统"。分层组合——Windows 原生层 + 通用层 + Windows 仿真层（winemu），由 [`CompositePlatform`](src/lib.rs) 按序应答。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-platform/       # 库 crate——平台层（分层：仿真 + 通用 + 系统）
      src/
        lib.rs        Platform 分层组合（CompositePlatform）、host_platform() /
                      host_platform_with() / host_platform_with_options()（PlatformOptions：
                      force_emulated 经 FilteredPlatform 把指定函数路由到仿真层）工厂、
                      runtime_with_platform() 便捷构造
        pathmap.rs    PathMap：仿真 `C:` ↔ 宿主目录的双向翻译（默认 C:\ = 宿主根）
        common/       通用层：文件/目录 I/O、INI、环境变量、数学、计时器、控制台
                      + 进程执行与网络 —— Linux 与 Windows 都安装
          mod.rs        CommonPlatform：直接分发 + 委托给下面两个服务；带
                        path_map/script_path 时翻译路径参数并在宏里报脚本自身
          proc.rs       Run 家族统一接口（std::process 机制）；平台差异的探测点
                        （进程表/存活/内存）在各系统模块实现，这里按平台调用
          net.rs        Inet*/TCP*/UDP*/Ping/代理设置（std::net）
        linux/        系统层（Linux）：/proc 进程查询、OS 标识宏
          mod.rs
          proc_support.rs  /proc 探针（进程表/存活/内存，供 common 进程服务使用）
        windows/      系统层（Windows）：原生 Win32 后端（DllCall/DllStruct/剪贴板/
                      进程/驱动器/系统宏）；注册表、COM、GUI 由仿真层兜底
          mod.rs        WindowsPlatform：分发 + obj_*（IDispatch 晚绑定转发）+ 宏
          dll.rs        DllCall/DllCallAddress（LoadLibraryW/GetProcAddress + 变参调用桥）
          registry.rs   Reg*（64 位视图 + AutoIt 类型码 + @error 阶梯 + @extended 类型）
          com.rs        手写 IDispatch vtable 晚绑定（VARIANT/BSTR 封送，MTA）
          clipboard.rs  ClipGet/Put（CF_UNICODETEXT）
          process.rs    Toolhelp 进程快照 + K32GetProcessMemoryInfo
          drive.rs      DriveGet*（真实卷信息）
          files.rs      真实文件属性（RASH）、8.3 短名、EnvUpdate 广播
          misc.rs       MemGetStats/IsAdmin/ShellExecute*/RunAs*/DriveMap*/Shutdown
        winemu/       Windows 仿真层（非 Windows 主机；见下文「Windows 仿真」）
          mod.rs        WindowsEmulation：状态 + builder + `Platform::call` 的宏表
                        与内建分发 + 注册表/剪贴板/驱动器/`FileInstall`
          com.rs        伪 COM 对象模型（Scripting.Dictionary、WScript.Shell、
                        Scripting.FileSystemObject）；`obj_get`/`obj_call`
                        两个 trait 方法仍在 mod.rs，转发到这里
          dll/          DllCall 仿真：mod.rs（`DllOutcome` + 函数名分派 + A/W
                        回退 + trace）、memory.rs（DllStruct/地址模型与缓冲辅助）、
                        crypto.rs（CryptoAPI 分支 + `RtlDecompressBuffer`）、
                        system.rs（版本/系统信息结构）
          version.rs    WindowsVersion：选定仿真系统版本（默认 win10）
          paths.rs      WindowsPaths：C:\ 目录布局（@WindowsDir、@AppDataDir…）
          registry.rs   RegistryStore 接口 + FileRegistry（默认，落盘 .au3_registry）
                        + MemoryRegistry（可选，不落盘）
          compress.rs   LZNT1 解压（自写：crates.io 的 `lznt1` 词法布局与 [MS-XCA]
                        相反，见下）
          crypto.rs     CryptoAPI 仿真：`CALG_*` 映射 + `CryptDeriveKey` 规则；
                        算法本身用 RustCrypto（md-5/sha1/sha2/aes/rc4）
          bcrypt.rs     bcrypt.dll（CNG）仿真：算法提供者、哈希/HMAC、对称密钥、
                        PBKDF2、GenRandom、RSA 私钥导入与解密（用 RustCrypto 实现）
          shell.rs      ShellExecute*/RunAs*
          gui/          GUI 无头语义：messages.rs（$EM_*/$LVM_* 默认）、
                        mod.rs（165 个 GUI 函数的 AutoIt 语义；控件模型与
                        GuiBackend 接缝在 autoitv3-gui-model crate）
        winfmt/       机制层（纯字节解析/布局，不碰 OS API；winemu 与 windows
                      共同复用）：dllstruct.rs（DllStruct 布局引擎）、pe.rs（PE
                      资源）、verinfo.rs（RT_VERSION）、shortcut.rs（.lnk）、
                      mod.rs（WindowsArch 指针宽度）
      tests/
        platform.rs   分层、选择、注入、通用函数与宏（56 项）
        windows_native.rs  原生 Win32 层 38 项（真实内核/注册表/COM 冒烟）+
                      扩展仿真 DllCall/回调/伪 COM（经 emu 栈，全宿主可跑）
        profile.rs    执行配置（忠实 / 确定性）（14 项）
        winemu.rs     Windows 仿真层：DllStruct / 注册表 / 快捷方式 / GUI …
        unit/         winfmt/winemu 各模块的单元测试（`#[path]` 回挂）
          winemu_bcrypt.rs    bcrypt.dll（CNG）仿真（12 项，公开向量）
          winemu_compress.rs  LZNT1 解压
          winemu_crypto.rs    CryptoAPI 仿真（8 项，公开向量）
          winemu_pe.rs        PE 资源读取
          winemu_verinfo.rs   RT_VERSION
```

## winemu 的拆分：DLL 与 COM 要不要单开

`winemu/mod.rs` 已经长到 3300 行，问"dll 和 com 该不该单开"时先量一下各段（行号为
撰写时，随改动漂移）：

| 段 | 位置 | 行数 |
| --- | --- | ---: |
| `DllCall` 分派（含 CryptoAPI/CNG 分支、A/W 回退、trace） | `dll_call` + `dll_call_inner` | ~600 |
| DllStruct/地址模型 + 缓冲辅助 | `struct_*`、`memory_*`、`read_buffer`/`dll_bytes`/`write_buffer`、`c_string_*` | ~250 |
| CryptoAPI 辅助 + `RtlDecompressBuffer` | `crypt_*`、`rtl_decompress_buffer` | ~160 |
| 版本/系统信息结构填充 | `fill_version_struct`、`fill_system_info` | ~80 |
| 注册表/剪贴板/驱动器/`FileInstall`（只从 DllCall 进） | `reg_*`、`clip_*`、`drive_*`、`file_install` | ~235 |
| **DLL 合计** | | **~1300** |
| 伪 COM：对象模型 + 4 个入口 | `PseudoObject`、`pseudo_com_*`、`obj_get`/`obj_call` | ~350 |
| `Platform::call`（winemu 自己的 AutoIt 内建） | 尾部 | ~835 |
| 状态 + builder + 访问器 | 头部 | ~445 |

**结论**

* **COM：单文件就够**（`winemu/com.rs`，~350 行）。它是一个自包含对象模型，对外只有
  `pseudo_com_create`/`pseudo_com_get`/`pseudo_com_call` 加 `obj_get`/`obj_call`，
  与既有的 `winemu/shell.rs`(85)、`winemu/registry.rs`(863) 同一量级；**不需要文件夹**。
  这样也和原生侧 `windows/com.rs` 对齐。
* **DLL：值得单开文件夹**（`winemu/dll/`，~1300 行）。它不是"一个东西"，而是四件彼此
  独立的事，而且都还在长（bcrypt 刚加进来、注册表/驱动器还在填）：
  * `dll/mod.rs` —— `DllOutcome`、`dll_call`、函数名分派、A/W 回退与 trace
  * `dll/memory.rs` —— DllStruct/地址模型与缓冲辅助（`read_buffer`/`dll_bytes`/
    `write_buffer`、`c_string_*`）
  * `dll/crypto.rs` —— CryptoAPI 分支 + `RtlDecompressBuffer`（bcrypt 本体已在
    `winemu/bcrypt.rs`）
  * `dll/system.rs` —— 版本/系统信息结构
  原生侧早就是这么分的（`windows/dll.rs` 877 行、`windows/com.rs` 414 行），winemu
  跟上即可。
* 更大的那块其实不是 DLL 也不是 COM，而是 **`Platform::call` 尾部那 835 行的
  winemu 版 AutoIt 内建**（`MemGetStats`/`IsAdmin`/`DriveMap*`/`ShellExecute*`/
  `FileInstall`…）。要拆的话按领域分（`winemu/sysinfo.rs`、`drive.rs`、`shell.rs`），
  与 DLL/COM 是两件事。

**已经这么做**（本次改动）：`mod.rs` 3327 → 2019 行，新增 `com.rs`(315)、
`dll/mod.rs`(590)、`dll/memory.rs`(241)、`dll/crypto.rs`(184)、`dll/system.rs`(89)。
搬运只动了三处可见性——搬出去的方法标 `pub(in crate::winemu)`、`DllOutcome` 与
`PseudoObject`/`PseudoKind` 同理（兄弟模块要互相看得见），以及子模块用
`use super::*` 取父模块的私有自由函数（子模块本来就能看祖先的私有项）。行为不变，
555 项测试全绿。

`obj_get`/`obj_call` 留在 `mod.rs`：它们是 `impl Platform` 的方法，一个 trait impl
不能跨模块写，于是它们只做一次转发，实现落在 `com.rs`。同理 `dll_call` 的入口
（`Platform::call` 要调它）也在 `dll/mod.rs` 里保持 `pub(in crate::winemu)`。

`DllCall` 那个 ~600 行的 `match` 本身不因为搬文件而变小，真要可读性得按命名空间切成
若干小函数，那是下一件事。

## 平台层

解释器核心、值模型与语言级内置函数都与平台无关。AutoIt 的函数库分成**通用**与
**系统相关**两部分，因此平台层是**分层**的（crate `autoitv3-platform`）：

| 层 | 模块 | 安装于 | 内容 |
| -- | ---- | ------ | ---- |
| 仿真 | `winemu/` | 非 Windows 时在**最前**；Windows 上在**最后兜底** | Windows 身份、路径、`DllStruct*`/`DllCall`、注册表、剪贴板、驱动器——让 Windows 目标脚本能在 Linux 上继续跑（见下文「Windows 仿真」）；`AU3_WIN_EMU=0` / `--no-win-emu` 可整体关闭 |
| 通用 | `common/`（`mod.rs` + `proc.rs` + `net.rs`） | **所有**平台 | 文件与目录 I/O（含 `FileFind*`、`FileGetPos`/`FileSetPos`/`FileSetEnd`、`FileGetEncoding`、`FileReadToArray`、`FileSetTime`）、INI（`Ini*` 7 个）、环境变量、数学、计时器、控制台，以及进程（`Run`/`ProcessWait*`/`StdoutRead`…）与网络（`Inet*`/`TCP*`/`UDP*`/`Ping`）——AutoIt 在各系统上行为一致的部分 |
| 系统 | `linux/` | 仅 Linux | `/proc` 进程查询（`ProcessList`/`ProcessExists`/`ProcessClose`）、OS 标识宏 |
| 系统 | `windows/` | 仅 Windows | **原生 Win32 后端**：`DllCall`/`DllCallAddress`/`DllOpen`/`DllClose`（`LoadLibraryW`/`GetProcAddress` + 变参调用桥）、`DllStruct*`（复用 winemu 布局引擎，但缓冲是**真实堆内存**，被调方直接写穿）、剪贴板（`ClipGet`/`ClipPut`，`CF_UNICODETEXT`）、进程（`ProcessList`/`ProcessExists`/`ProcessClose`，Toolhelp 快照）、驱动器（`DriveGet*`/`DriveMap*` 真实卷与网络映射）、注册表（`Reg*`，64 位视图 + AutoIt 类型码）、COM（`ObjCreate`/`IsObj`/`ObjName` 与 `.$member`/`.Method()` 经手写 `IDispatch` vtable 晚绑定）、系统/Shell（`MemGetStats`/`IsAdmin`/`ShellExecute*`/`RunAs*`/`Shutdown`）以及 Windows 身份宏（`@WindowsDir`/`@OSVersion`/`@ComputerName`…）；`files.rs` 以真实 Win32 语义覆盖 common 的近似实现（`FileGetAttrib`/`FileSetAttrib` 的 RASH 位、`FileGetShortName` 的真实 8.3 名、`EnvUpdate` 的 `WM_SETTINGCHANGE` 广播）。GUI 仍由仿真层兜底；`ObjGet`（文件名字对象）与 `ObjEvent`（事件接收器）报告 `@error = 1` |

`host_platform()` 按目标平台组装成 `CompositePlatform`：Windows 为
`windows+common+winemu`（原生层最前应答真实语义，通用层居中，仿真层最后只接住
原生未实现的 Windows 专有名），其余平台为 `winemu+common+linux`（仿真层在最前，
因此它的宏会**有意覆盖**通用层的同名宏）。逐层查找；通用层在 Windows 上同样生效。
`AU3_WIN_EMU=0`（或 `--no-win-emu`）去掉兜底后，原生未实现的名字回归"未定义函数"。
需要显式指定仿真配置时用 `host_platform_with(WindowsEmulation::new()...)`。

#### 被分析镜像的资源（两个宿主一致）

编译后的脚本把自己的载荷放进**自身 PE 镜像的 `RT_RCDATA`**，运行期用
`GetModuleHandleW(NULL)` → `FindResourceW` → `SizeofResource` → `LoadResource` →
`LockResource` → `RtlMoveMemory` 读出。分析脚本源码时真实的宿主是 `au3`，它的镜像里
没有这些资源，所以**无论在哪台主机上都会读失败**——除非把镜像指回被分析的 `.exe`。

`--resource-module`（或 `AU3_RESOURCE_MODULE`，或直接把编译产物当输入，
或脚本旁边自动发现的同名 `.exe`）因此**同时**喂给两个宿主：

- 非 Windows（`winemu`）由 `PeImage` 解析镜像，用仿真地址回答整条资源链；
- Windows（原生层）用 `LoadLibraryExW(path, NULL, LOAD_LIBRARY_AS_IMAGE_RESOURCE)`
  把镜像按**资源**映射进地址空间（不执行其中任何代码），让 `GetModuleHandleW(NULL)`
  返回它，于是 `FindResource*`/`LoadResource`/`LockResource`/`RtlMoveMemory` 全走真实
  Win32 语义、真实指针。

于是同一份 `au3 deobf script.au3 --evaluate` 在 Linux 与 Windows 上走到同一条边界。

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
| 宏 | `@TempDir`、`@AutoItPID`、`@AutoItEXE`、`@WorkingDir`/`@ScriptDir`/`@ScriptName`/`@ScriptFullPath`（后三个由平台栈按被分析的脚本填入，不给就退回工作目录）、`@UserName`、`@ComputerName`、`@HomePath`/`@UserProfileDir`、`@AppDataDir`/`@LocalAppDataDir`（XDG）、`@DesktopDir`、`@MyDocumentsDir` |

### 通用层的进程与网络（`common/proc.rs` + `common/net.rs`）

`CommonPlatform` 除了直接分发上面的文件/环境/数学函数，还把进程与网络委托给
`common/proc.rs`、`common/net.rs` 两个子服务，因此它们是**同一个通用层**的一部分。
其中 `proc.rs` 的语义是**统一接口**：`Run` 家族的接口与 `std::process` 机制在
common 定义；平台差异（进程表、存活探测、内存）在各平台模块实现——Linux 在
`linux/proc_support.rs` 走 `/proc`，Windows 在 `windows/process.rs` 走
Toolhelp/`K32GetProcessMemoryInfo`——由 common 的三个按平台分派的钩子调用。
新增宿主只需在其系统模块实现这三个钩子，不动家族接口：

| 类别 | 函数 | 说明 |
| ---- | ---- | ---- |
| 执行 | `Run`、`RunWait` | `std::process`；`$STDIO_*` 标志决定是否接管标准流 |
| 标准 IO | `StdoutRead`、`StderrRead`、`StdinWrite`、`StdioClose` | 每条流一个后台读取线程，读操作永不阻塞；进程结束后 join，缓冲完整 |
| 进程 | `ProcessWait`、`ProcessWaitClose`、`ProcessGetStats`、`ProcessSetPriority` | `ProcessWait*` 超时为**秒**、0 表示无限（与 AutoIt 一致）；`ProcessGetStats` 在 Linux 读 `/proc/<pid>/status`，在 Windows 走 `K32GetProcessMemoryInfo` |
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
| 路径 | 宏与文件参数是两个方向：`@ScriptDir`/`@ScriptName`/`@ScriptFullPath` 描述被分析的脚本，路径宏与文件函数之间按 `C:` ↔ 宿主根翻译（见下文「盘符映射」；`AU3_WIN_DRIVE_MAP`/`--win-drive-map`/`without_path_map()` 可关） |
| 目录 | `WindowsPaths` 给出传统 `C:` 布局：`@WindowsDir`、`@SystemDir`、`@ProgramFilesDir`、`@HomeDrive`、`@TempDir`、`@AppDataDir`、`@LocalAppDataDir`、`@UserProfileDir`、`@StartMenuDir`、`@StartupDir`…… |
| 原生结构 | `DllStructCreate`/`GetData`/`SetData`/`GetSize`/`GetPtr`/`IsDllStruct`——定义解析器支持 `struct;…;endstruct`、常见整型/浮点/指针、`char`/`wchar` 数组、无名段、`align N`；句柄指向一块本层持有的字节缓冲 |
| 原生调用 | `DllCall(dll, rettype, func, type, arg…)`：版本/系统信息（`GetVersionExW/A`、`RtlGetVersion`、`GetSystemInfo`）、资源链（`GetModuleHandle*`/`FindResource*`/`SizeofResource`/`LoadResource`/`LockResource`/`RtlMoveMemory`）、模块与内存（`LoadLibrary*`/`GetProcAddress`/`GetModuleFileName*`、`VirtualAlloc`/`HeapAlloc` 等）、**内存沙箱文件**（`CreateFile*`/`ReadFile`/`WriteFile`/`GetFileSize`/`CloseHandle`，`with_file()` 注入）、CRT 字符串（`lstrlen*`/`lstrcpy*`/`lstrcat*`）、脚本化 `EnumWindows` 家族、CryptoAPI、**bcrypt.dll（CNG）** 与 LZNT1 解压。返回 **AutoIt 风格的数组**（`[0]` = 返回值，其余为 by-ref 参数）——脚本普遍写 `$r = DllCall(...)` / `If @error Or Not $r[0]`，返回标量会让它们全部报类型错误；调用失败时按 AutoIt 语义返回 `0` 并置 `@error = 1` |
| 注册表 | `RegRead`/`RegWrite`/`RegDelete`/`RegEnumKey`/`RegEnumVal` 全部重定向到可插拔的 `RegistryStore` 接口。默认实现是 `FileRegistry`：注册表状态落在**工作目录的 `.au3_registry` 文本文件**里，读在加载时进入内存；写先在内存累积（dirty 标记），store drop 或显式 `flush()` 时一次落盘；`MemoryRegistry`（不落盘）用 `with_memory_registry()` 选回 |
| 剪贴板 | `ClipGet`/`ClipPut` 落到**工作目录下的文件**（默认 `.au3_clipboard`，可用 `with_clipboard_file()` 改名） |
| 驱动器 | `DriveGetDrive`/`DriveGetType`/`DriveGetFileSystem`/`DriveGetLabel`/`DriveGetSerial`/`DriveSpaceTotal`/`DriveSpaceFree`/`DriveStatus`，默认一台 `C:`（`DriveSpec` 可配）；网络映射 `DriveMapAdd`/`DriveMapDel`/`DriveMapGet` 与 `DriveSetLabel` 维护本层的映射/卷标状态 |
| Windows 文件 | `FileGetVersion`（解析 PE `RT_VERSION`）、`FileCreateShortcut`/`FileGetShortcut`（读写真实 `.lnk` Shell Link）、`FileCreateNTFSLink`、`FileRecycle`/`FileRecycleEmpty`（落到 `.au3_recycle`，可用 `with_recycle_dir()` 改名）、`FileInstall`（磁盘文件或已加载模块的 `RT_RCDATA` 资源） |
| 回调 | `DllCallbackRegister`/`DllCallbackGetPtr`/`DllCallbackFree` 发放合成指针；`EnumWindows`/`EnumChildWindows`/`EnumThreadWindows` 按 `with_scripted_windows()` 的句柄表把回调排入队列，运行时在 DllCall 返回后真实执行脚本函数（不重入解释器）；`DllCallAddress` 无加载器，按边界失败 |
| 系统信息 / Shell | `MemGetStats`（固定机器画像，可复现）、`IsAdmin`（`AU3_WIN_ADMIN`/`with_admin()`）；`ShellExecute`/`ShellExecuteWait`/`RunAs`/`RunAsWait` 委托宿主进程，`Shutdown` 只记录请求 |
| COM | **伪 COM**：`ObjCreate` 对内建 ProgID 表返回真实行为对象——`Scripting.Dictionary`（Add/Exists/Item/Count/Keys/Items/Remove/RemoveAll）、`WScript.Shell`（RegRead/RegWrite/RegDelete 桥接仿真注册表、ExpandEnvironmentStrings、Run）、`Scripting.FileSystemObject`（FileExists/DriveExists/路径运算/GetSpecialFolder）；表外 ProgID 与 `ObjCreateInterface`/`ObjEvent`/`ObjGet` 维持 `@error = 1`；`IsObj` 对前者返回 `1`、`ObjName` 回显 ProgID |
| GUI | `GUICreate`/`GUICtrlCreate*`/`GUICtrlSet*`/`GUIGetMsg`/`Win*`/`Control*`/对话框/托盘/输入/像素 共 165 项，全部在 `winemu/gui/` 的**内存控件树**上实现：控件=对象、句柄=整数、`GUIGetMsg` 无事件返回 `0`、`GUICtrlSendMsg` 对 `$EM_*`/`$LVM_*`/`$TVM_*` 给默认值（未知消息置 `@error`）。渲染与事件是 `GuiBackend` 接口（模型与接缝在 `autoitv3-gui-model`），默认 `HeadlessBackend` 不画任何东西；`autoitv3-gui-egui` 提供**离屏**（`EguiBackend`，可出 PNG）与**真窗口**（`LiveBackend`）两种渲染，29 种控件全部落地（见下节）；`with_gui_events`/`with_gui_auto_close`/`with_gui_answers` 提供**脚本化事件**，让消息循环可确定终止、对话框不阻塞；**答过的对话框会往 stderr 打一行** `[winemu] MsgBox(...) -> 1`（模拟框不显示文本，不报就等于把"脚本走了错误分支"藏起来了），重复的只报一次。默认后端不画任何东西，所以"GUI 函数有返回值但窗口没出现"是**默认行为**；要看真窗口用 `autoitv3-gui-egui` 的 `LiveBackend`——`au3 run --gui window`（构建 CLI 时加 `--features gui-window`）把窗口后端交给仿真层，窗口归主线程、脚本跑在工作线程上（见 `crates/au3-cli/README.md`） |

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

#### 盘符映射：`C:` → 宿主根（默认开启）

脚本不只是**打印**路径，它会把路径**拆开**：`@ScriptDir & "\data.dat"` 是最简单的
一种，更常见的是手写的规范化函数——按 `\` 切分、看头两个字符是不是盘符或 `\\server`、
折叠 `.`/`..`。给这种代码一个 POSIX 路径，它会算出垃圾（`/home/me/x` 没有盘符，
于是 `/h` 被当成盘符）。所以仿真层交给脚本的路径是 Windows 形状的，回到宿主文件系统
边界时再翻回去：

| 方向 | 规则 |
| --- | --- |
| 交给脚本 | `@ScriptDir`/`@WorkingDir`/`@TempDir` 等路径宏报 `C:\...`；`@ScriptName`/`@ScriptFullPath` 报脚本自身的文件名/全路径 |
| 进入宿主 | 文件/目录/INI 函数的路径参数里，`C:\x` → `<root>/x`；相对路径的 `\` 也按宿主分隔符归一 |
| 回到脚本 | `FileGetLongName`/`FileGetShortName` 之类返回路径的函数翻回 `C:\...` |

默认 `C:\` 就是宿主根：`C:\home\me\a.dat` 打开 `/home/me/a.dat`
（`PathMap::host_root()`；在 Windows 上这是恒等映射）。开关：

```rust
use autoitv3_platform::winemu::WindowsEmulation;

let emu = WindowsEmulation::new().with_drive_root("/srv/sandbox"); // C:\ → 该目录
let emu = WindowsEmulation::new().without_path_map();              // 完全关掉
let emu = WindowsEmulation::new().with_script_path("build/run.au3"); // @ScriptDir 等
```

CLI 是 `--win-drive-map <ROOT>`（`--win-drive-map ""` 或 `--no-win-drive-map` 关掉），
环境变量 `AU3_WIN_DRIVE_MAP`（`0`/`off` 关掉，其它值当根目录）。`au3 run`/`debug` 会
自动把被分析的脚本路径通过 `with_script_path()` 传进来，所以 `@ScriptDir`/`@ScriptName`/
`@ScriptFullPath` 描述的是脚本本身，而不是工作目录。

几点边界：**只有路径参数**会被翻译（`FileWrite` 的内容是数据，不动）；映射不覆盖的
目录（自定义根之外的路径）按宿主写法返回；`with_host_paths()`（保留可用宿主路径）
会一并关掉映射，因为把宿主路径渲染成 `C:\tmp` 正好和它相反。要模拟脚本里的
`C:\Windows\...` 仍需宿主上真有这个目录树——映射不凭空造文件。

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
- COM 只有一张内建 ProgID 表（`ObjCreate` 的三个 + `IsObj`/`ObjName`），表外的一律
  **可判定失败**（返回 `0`/`""` + `@error = 1`），不编造对象；
- GUI 已由 `winemu/gui/` 的**无头语义**回答（不再是 `undefined function`），默认后端
  **不渲染**——所以"GUI 函数有返回值、屏幕上却没有窗口"是默认行为，不是缺函数。
  要看真窗口：CLI 用 `au3 run --gui window` / `au3 debug --gui window`（构建时加
  `--features gui-window`），
  或直接用 `autoitv3-gui-egui` 的 `LiveBackend`（feature `window`：窗口归主线程、
  脚本跑在工作线程，点击/输入回灌 `GUIGetMsg`/`GUICtrlRead`）；只在离屏画布上出
  PNG 截图则用 feature `gui-egui`。两种后端共用同一套控件绘制；
- 写文件、回收站、驱动器映射、启动进程同样遵循 `ExecutionProfile`：确定性分析配置下被拒绝
  （`@error = 1`），也就不会生成 `.au3_registry` / `.au3_clipboard` / `.au3_recycle`；
- 用 `--no-win-emu` / `AU3_WIN_EMU=0` / `WindowsEmulation::new().disabled()` 可整体关闭，
  回到"停在第一个 Windows 调用"的诚实行为；`with_host_paths()` 则只让**目录**宏回落到
  主机路径（`@TempDir` 等仍可用于真实文件 I/O），Windows 专有宏照旧仿真。
