//! The non-interactive `au3` commands (run/parse/pretty/deobfuscate/evaluate/
//! unpack), their shared output and progress helpers, and elevation notes.

pub static ENTRIES: &[(&str, &str)] = &[
    ("cannot write to stdout: {e}", "无法写入标准输出：{e}"),
    ("cannot write {path}: {e}", "无法写入 {path}：{e}"),
    ("note: ", "注意："),
    ("cannot find the running executable: {e}", "找不到正在运行的可执行文件：{e}"),
    ("#RequireAdmin: {e}", "#RequireAdmin：{e}"),
    (
        "#RequireAdmin: --no-elevate, running without administrator rights",
        "#RequireAdmin：--no-elevate，以非管理员权限运行",
    ),
    (
        "#RequireAdmin: --deny spawn, running without administrator rights",
        "#RequireAdmin：--deny spawn，以非管理员权限运行",
    ),
    (
        "#RequireAdmin: the elevated copy finished with exit code {code}",
        "#RequireAdmin：提升权限的副本已结束，退出码为 {code}",
    ),
    (
        "#RequireAdmin: the elevation prompt was dismissed, running without administrator rights",
        "#RequireAdmin：权限提升提示已被关闭，以非管理员权限运行",
    ),
    (
        "#RequireAdmin: elevation is a Windows mechanism and this host has none, running without administrator rights",
        "#RequireAdmin：权限提升是 Windows 机制，此主机不支持，以非管理员权限运行",
    ),
    ("error: {message}", "错误：{message}"),
    ("opening the GUI window failed: {e}", "打开 GUI 窗口失败：{e}"),
    (
        "note: #RequireAdmin: --gui window keeps this process, so the script runs without administrator rights",
        "注意：#RequireAdmin：--gui window 保留当前进程，因此脚本以非管理员权限运行",
    ),
    (
        "--gui window needs a build with the `gui-window` feature (cargo build --release -p au3-cli --features gui-window)",
        "--gui window 需要启用 `gui-window` 特性构建（cargo build --release -p au3-cli --features gui-window）",
    ),
    ("script body returned {value}", "脚本主体返回 {value}"),
    ("script body exited with code {code}", "脚本主体以退出码 {code} 退出"),
    ("script body ran to completion", "脚本主体运行完毕"),
    ("(stopped: {reason})", "（已停止：{reason}）"),
    ("error while running script body: {e}", "运行脚本主体时出错：{e}"),
    ("runtime error in {function}(): {e}", "运行时错误（{function}()）：{e}"),
    ("[trace] {pos} depth={depth}", "[trace] {pos} 深度={depth}"),
    (
        "[trace] ... (further statements suppressed)",
        "[trace] ...（后续语句已省略）",
    ),
    ("[trace] call {name}({args} args)", "[trace] 调用 {name}（{args} 个参数）"),
    ("[trace] stop: {reason}", "[trace] 停止：{reason}"),
    ("<unnamed>", "<未命名>"),
    ("  {bytes} bytes of source", "  {bytes} 字节源码"),
    (
        "{path} is a directory; --script reads a PE image or a compiled-script chunk",
        "{path} 是目录；--script 读取的是 PE 映像或编译脚本数据块",
    ),
    (
        "compiled script: {version} ({files} embedded file(s))",
        "编译脚本：{version}（{files} 个内嵌文件）",
    ),
    ("  {sub_type} {name} ({bytes} bytes)", "  {sub_type} {name}（{bytes} 字节）"),
    ("{path}: no resources to look at", "{path}：没有可查看的资源"),
    (
        "unpacked: loader {loader}, members {members0}/{members1}/{members2} ({resources} resources considered)",
        "已解包：加载器 {loader}，成员 {members0}/{members1}/{members2}（已考虑 {resources} 个资源）",
    ),
    ("  {entries} entries, {bytes} bytes", "  {entries} 个条目，{bytes} 字节"),
    (
        "evaluated: {globals} globals, {tables} tables, {inlined} values inlined, {calls} calls resolved",
        "已求值：{globals} 个全局变量，{tables} 个表，内联 {inlined} 个值，解析 {calls} 个调用",
    ),
    (
        "  {declarations} table declaration(s) rewritten as literal values",
        "  已将 {declarations} 个表声明改写为字面量值",
    ),
    ("script body did not finish: {why}", "脚本主体未运行完毕：{why}"),
    (
        "  (that is the platform boundary: this function is not implemented for the current OS)",
        "  （这是平台边界：当前操作系统未实现此函数）",
    ),
    ("  values produced before that point were still inlined", "  在此之前产生的值仍已内联"),
    ("script body stopped early (Exit)", "脚本主体提前停止（Exit）"),
    (
        "  {more} more values inlined in code the simplifier spliced",
        "  简化器拼接的代码中又内联了 {more} 个值",
    ),
    (" (renaming off; pass --rename)", "（未启用重命名；传入 --rename 可启用）"),
    (
        "deobfuscated: {folds} folds, {vars} vars, {funcs} funcs renamed{renamed}; table: {entries} entries, {calls} calls, {refs} refs; {simplified} indirect calls simplified",
        "已反混淆：{folds} 次折叠，重命名 {vars} 个变量、{funcs} 个函数{renamed}；表：{entries} 个条目，{calls} 个调用，{refs} 个引用；简化 {simplified} 个间接调用",
    ),
    (
        "note: {computed} global(s) are built by a function call at load time; re-run with --evaluate to run the script body and inline their values",
        "注意：有 {computed} 个全局变量在加载时由函数调用构建；请用 --evaluate 重新运行以执行脚本主体并内联其值",
    ),
    (
        "evaluating: {globals} globals, {tables} tables ({seconds}s)",
        "评估中：{globals} 个全局变量，{tables} 个表（{seconds}s）",
    ),
    (
        "parsed OK: {items} top-level items, {functions} functions",
        "解析成功：{items} 个顶层项，{functions} 个函数",
    ),
    // the elevated `#RequireAdmin` copy's console note (printed by `main.rs`)
    ("note: #RequireAdmin: elevated, sharing the console of process {pid}", "注意：#RequireAdmin：已提升权限，输出接在进程 {pid} 的控制台上"),
    ("note: #RequireAdmin: elevated, but process {pid} has no console to share,           so this output has a window of its own", "注意：#RequireAdmin：已提升权限，但进程 {pid} 没有可共用的控制台，          输出会单独开一个窗口"),
];
