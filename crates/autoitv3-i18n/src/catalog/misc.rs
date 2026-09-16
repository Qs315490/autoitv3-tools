//! Messages from the earlier analysis stages that are not part of
//! `autoitv3-runtime`: `autoitv3-ast`, `autoitv3-preproc`, `autoitv3-deobf`
//! and `autoitv3-format`. Errors and warnings only — generated AutoIt source
//! is output, not a message, and stays as it is.

pub static ENTRIES: &[(&str, &str)] = &[
    // autoitv3-ast (lexer)
    ("unexpected character '{c}'", "意外的字符 '{c}'"),
    ("unterminated string literal", "字符串字面量未终止"),
    // autoitv3-ast (parser)
    ("expected {what}", "应为 {what}"),
    ("unexpected EOF: missing EndFunc", "意外的文件结尾：缺少 EndFunc"),
    ("unexpected EOF: missing EndIf", "意外的文件结尾：缺少 EndIf"),
    ("unexpected EOF: missing WEnd", "意外的文件结尾：缺少 WEnd"),
    ("unexpected EOF: missing Until", "意外的文件结尾：缺少 Until"),
    ("unexpected EOF: missing Next", "意外的文件结尾：缺少 Next"),
    ("unexpected EOF: missing EndSelect", "意外的文件结尾：缺少 EndSelect"),
    ("expected Case or EndSelect", "应为 Case 或 EndSelect"),
    ("unexpected EOF: missing EndSwitch", "意外的文件结尾：缺少 EndSwitch"),
    ("expected Case or EndSwitch", "应为 Case 或 EndSwitch"),
    ("unexpected EOF: missing EndWith", "意外的文件结尾：缺少 EndWith"),
    ("cannot call non-variable expression", "无法调用非变量表达式"),
    ("expected a member name after '.'", "'.' 之后应为成员名"),
    ("expected expression", "应为表达式"),
    ("expected identifier or variable name", "应为标识符或变量名"),
    // autoitv3-preproc
    (
        "#include without a file name ({argument}) — the directive was ignored",
        "#include 缺少文件名（{argument}）——该指令已被忽略",
    ),
    (
        "#include {argument} not found (searched {searched}) — put the AutoIt Include directory in AU3_INCLUDE_PATH or pass -I DIR",
        "#include {argument} 未找到（已搜索 {searched}）——请将 AutoIt Include 目录加入 AU3_INCLUDE_PATH 或使用 -I DIR",
    ),
    (
        "#include {path} is already being expanded (cyclic include) — it was skipped",
        "#include {path} 已在展开中（循环包含）——已跳过",
    ),
    (
        "#include {path} nests more than {max_depth} levels deep",
        "#include {path} 的嵌套层级超过 {max_depth} 层",
    ),
    // The next two keys are also used by the CLI and already translated in
    // `cli_help`; they are repeated here with the same text so the analysis
    // stage does not depend on that table.
    ("cannot read {path}: {e}", "无法读取 {path}：{e}"),
    (
        "#include {path} is a compiled file (.a3x), which is not expanded",
        "#include {path} 是编译文件（.a3x），不进行展开",
    ),
    ("parse error in {path}: {e}", "解析 {path} 时出错：{e}"),
];
