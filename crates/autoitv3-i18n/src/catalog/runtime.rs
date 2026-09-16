//! Messages from the interpreter and the earlier analysis stages
//! (`autoitv3-runtime`, `autoitv3-ast`, `autoitv3-preproc`, `autoitv3-deobf`,
//! `autoitv3-format`) — errors and warnings, not generated source.

pub static ENTRIES: &[(&str, &str)] = &[
    ("unsupported construct: {what}", "不支持的构造：{what}"),
    ("type error: expected {expected}, got {got}", "类型错误：应为 {expected}，实际为 {got}"),
    ("undefined variable: {name}", "未定义的变量：{name}"),
    ("undefined function: {name}", "未定义的函数：{name}"),
    ("index {index} out of bounds (len {len})", "索引 {index} 越界（长度为 {len}）"),
    (
        "array of {elements} elements is past the {limit} AutoIt allows (VAR_SUBSCRIPT_ELEMENTS)",
        "数组的 {elements} 个元素超过了 AutoIt 允许的 {limit} 个（VAR_SUBSCRIPT_ELEMENTS）",
    ),
    ("step limit exceeded ({limit})", "超出步数限制（{limit}）"),
    ("call depth exceeded ({limit})", "超出调用深度限制（{limit}）"),
    ("host function {name}: {message}", "宿主函数 {name}：{message}"),
    ("aborted by the debugger", "已由调试器中止"),
    ("{message} (at {line}:{col})", "{message}（位于 {line}：{col}）"),
    ("jump target line {line} is outside the current frame", "跳转目标行 {line} 超出当前帧"),
    (
        "loop or case control outside its block in {display}",
        "循环或 Case 控制流超出了 {display} 中的块",
    ),
    ("`With` subject outside a platform host", "`With` 主体不在平台宿主中"),
    (
        "member access `.{member}` on a non-object value",
        "对非对象值进行成员访问 `.{member}`",
    ),
    ("member access `.{member}` (no platform installed)", "成员访问 `.{member}`（未安装平台层）"),
    (
        "method call `.{member}()` on a non-object value",
        "对非对象值进行方法调用 `.{member}()`",
    ),
    ("method call `.{member}()` (no platform installed)", "方法调用 `.{member}()`（未安装平台层）"),
    ("assignment target {target}", "赋值目标 {target}"),
    ("not an expression: {error} ({source})", "不是表达式：{error}（{source}）"),
    ("not an expression: {source}", "不是表达式：{source}"),
    ("Execute() parse error: {error}", "Execute() 解析错误：{error}"),
    ("Execute() control flow {flow}", "Execute() 控制流 {flow}"),
    ("StringRegExp flag {other} (expected 0..4)", "StringRegExp 标志 {other}（应为 0..4）"),
    ("this host cannot jump", "此宿主无法跳转"),
    // A refused side effect: the profile said no, and the call failed like a
    // real permission problem would.
    (
        "note: a {kind} side effect was refused by the execution profile — use --allow {kind} to permit this kind, or --faithful to let the script do its side effects for real",
        "注意：执行配置拒绝了一次 {kind} 类副作用 — 用 --allow {kind} 只放行这一类，或用 --faithful 让脚本真的产生副作用",
    ),
];
