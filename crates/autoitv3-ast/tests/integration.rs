//! Unit tests for the AutoIt v3 AST library.
//!
//! These tests exercise the lexer, parser, AST structure and the
//! pretty-printer round-trip. They use the public library API so they also
//! serve as usage examples for downstream callers.

use autoitv3_ast::ast::{BinaryOp, ExprKind, ItemKind, LitKind, StmtKind, VarKind};
use autoitv3_ast::{parse, lexer};

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[test]
fn lexer_simple_tokens() {
    let toks = lexer::lex("Local $x = 0x1F + 2").unwrap();
    let kinds: Vec<String> = toks.iter().map(|t| format!("{:?}", t.kind)).collect();
    // Local, $x, =, 0x1F, +, 2, Eof
    assert_eq!(kinds.len(), 7);
    assert_eq!(kinds[0], "Local");
    assert!(kinds[1].starts_with("Var(\"$x\")"));
    assert_eq!(kinds[2], "Assign");
    assert_eq!(kinds[3], "Number(\"0x1F\")");
    assert_eq!(kinds[4], "Plus");
    assert_eq!(kinds[5], "Number(\"2\")");
    assert_eq!(kinds[6], "Eof");
}

#[test]
fn lexer_string_escape() {
    // AutoIt doubles quotes to escape: "say ""hi"""
    let toks = lexer::lex("MsgBox(0, \"say \"\"hi\"\"\")").unwrap();
    let s = toks
        .iter()
        .find(|t| matches!(t.kind, autoitv3_ast::token::TokenKind::Str(_)))
        .unwrap();
    match &s.kind {
        autoitv3_ast::token::TokenKind::Str(v) => assert_eq!(v, "say \"hi\""),
        other => panic!("expected Str, got {other:?}"),
    }
}

#[test]
fn lexer_preproc_keeps_rest_of_line() {
    let toks = lexer::lex("#include <Constants.au3>\n").unwrap();
    match &toks[0].kind {
        autoitv3_ast::token::TokenKind::Preproc(s) => {
            assert_eq!(s, "include <Constants.au3>");
        }
        other => panic!("expected Preproc, got {other:?}"),
    }
}

#[test]
fn lexer_compound_assign_and_ternary() {
    let toks = lexer::lex("$a += 1\n$b -= 2\n$x = $c ? 1 : 2").unwrap();
    let kinds: Vec<String> = toks.iter().map(|t| format!("{:?}", t.kind)).collect();
    assert!(kinds.contains(&"PlusAssign".to_string()));
    assert!(kinds.contains(&"MinusAssign".to_string()));
    assert!(kinds.contains(&"Question".to_string()));
    assert!(kinds.contains(&"Colon".to_string()));
}

#[test]
fn lexer_handles_crlf() {
    let toks = lexer::lex("#NoTrayIcon\r\nMsgBox(0, \"hi\")\r\n").unwrap();
    // Preproc must not retain the trailing \r.
    match &toks[0].kind {
        autoitv3_ast::token::TokenKind::Preproc(s) => assert_eq!(s, "NoTrayIcon"),
        other => panic!("expected Preproc, got {other:?}"),
    }
    let eof = toks.last().unwrap();
    assert_eq!(eof.kind, autoitv3_ast::token::TokenKind::Eof);
}

// ---------------------------------------------------------------------------
// Parser / AST structure
// ---------------------------------------------------------------------------

#[test]
fn parse_top_level_items() {
    let prog = parse("#NoTrayIcon\nFunc A()\nEndFunc\nB()\n").unwrap();
    assert_eq!(prog.items.len(), 3);
    assert!(matches!(prog.items[0].kind, ItemKind::Directive(_)));
    assert!(matches!(prog.items[1].kind, ItemKind::Func(_)));
    assert!(matches!(prog.items[2].kind, ItemKind::Stmt(_)));
}

#[test]
fn parse_global_consts_and_vars() {
    let src = "Global Const $A = 1\nGlobal $b = $A + 2\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::VarDecl(v) = &s.kind else {
        panic!("expected var decl");
    };
    assert_eq!(v.kind, VarKind::Global);
    assert!(v.is_const);
    assert_eq!(v.vars.len(), 1);
    assert_eq!(v.vars[0].name.name, "$A");
}

#[test]
fn parse_assign_expr_is_binary_assign() {
    let prog = parse("$x = 5\n").unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::Expr(e) = &s.kind else {
        panic!("expected expr stmt");
    };
    let ExprKind::Binary(op, lhs, rhs) = &e.kind else {
        panic!("expected binary");
    };
    assert_eq!(*op, BinaryOp::Assign);
    assert!(matches!(lhs.kind, ExprKind::Var(_)));
    match &rhs.kind {
        ExprKind::Lit(l) => assert!(matches!(l.kind, LitKind::Int(5))),
        _ => panic!("expected int literal"),
    }
}

#[test]
fn a_hex_literal_is_a_32_bit_pattern() {
    // AutoIt reads a hex literal that fits in 32 bits into an *Int32*: on
    // 3.3.16 `VarGetType(0xffffffff)` is Int32 and its value is -1, while
    // `0x100000000` is an ordinary Int64. Reading it as an unsigned 64-bit
    // number makes `$n + 0xffffffff` ("minus one") ask for four billion.
    let int_of = |src: &str| match parse(src) {
        Ok(prog) => match &prog.items[0].kind {
            ItemKind::Stmt(s) => match &s.kind {
                StmtKind::Expr(e) => match &e.kind {
                    ExprKind::Lit(l) => match l.kind {
                        LitKind::Int(v) => v,
                        ref other => panic!("expected an int literal, got {other:?}"),
                    },
                    other => panic!("expected a literal, got {other:?}"),
                },
                other => panic!("expected an expr stmt, got {other:?}"),
            },
            other => panic!("expected a stmt, got {other:?}"),
        },
        Err(e) => panic!("{src:?} did not parse: {e}"),
    };
    assert_eq!(int_of("0x7fffffff\n"), 2_147_483_647);
    assert_eq!(int_of("0x80000000\n"), -2_147_483_648);
    assert_eq!(int_of("0xffffffff\n"), -1);
    assert_eq!(int_of("0x00ffffffff\n"), -1, "leading zeros do not widen it");
    assert_eq!(int_of("0x100000000\n"), 4_294_967_296);
    assert_eq!(int_of("4294967295\n"), 4_294_967_295, "decimal stays Int64");
}

#[test]
fn parse_compound_assign_op() {
    let prog = parse("$counter += 3\n").unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::Expr(e) = &s.kind else {
        panic!("expected expr stmt");
    };
    match &e.kind {
        ExprKind::Binary(op, _, _) => assert_eq!(*op, BinaryOp::PlusAssign),
        other => panic!("expected compound assign, got {other:?}"),
    }
}

#[test]
fn parse_ternary() {
    let prog = parse("$r = $a ? 1 : 2\n").unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::Expr(e) = &s.kind else {
        panic!("expected expr stmt");
    };
    let ExprKind::Binary(BinaryOp::Assign, _, rhs) = &e.kind else {
        panic!("expected assign");
    };
    assert!(matches!(rhs.kind, ExprKind::Ternary(_, _, _)));
}

#[test]
fn parse_func_def_with_params() {
    let src = "Func Add(ByRef $a, Const $b = 10, $c = Default)\n    Return $a\nEndFunc\n";
    let prog = parse(src).unwrap();
    let ItemKind::Func(f) = &prog.items[0].kind else {
        panic!("expected func");
    };
    assert_eq!(f.name.name, "Add");
    assert_eq!(f.params.len(), 3);
    assert!(f.params[0].by_ref);
    assert!(f.params[1].by_ref == false);
    assert!(f.params[1].default.is_some());
    assert!(f.params[2].default.is_some());
}

#[test]
fn parse_if_single_line_no_endif_needed() {
    // Single-line If requires no EndIf.
    let prog = parse("If $a = 1 Then $b = 2\n$x = 3\n").unwrap();
    assert_eq!(prog.items.len(), 2);
}

#[test]
fn parse_if_multiline_with_elseif_else() {
    let src = "If $a Then\n    $x = 1\nElseIf $b Then\n    $x = 2\nElse\n    $x = 3\nEndIf\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::If(if_) = &s.kind else {
        panic!("expected if");
    };
    assert_eq!(if_.then_block.len(), 1);
    assert_eq!(if_.else_ifs.len(), 1);
    assert_eq!(if_.else_block.len(), 1);
}

#[test]
fn parse_for_in() {
    let src = "For $x In $arr\n    $x = 1\nNext\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::For(f) = &s.kind else {
        panic!("expected for");
    };
    assert!(f.iter.is_some());
    assert_eq!(f.body.len(), 1);
}

#[test]
fn parse_for_to_step() {
    let src = "For $i = 1 To 10 Step 2\nNext\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::For(f) = &s.kind else {
        panic!("expected for");
    };
    assert!(f.iter.is_none());
    assert!(f.step.is_some());
}

#[test]
fn parse_select_switch_with() {
    let src = "Select\nCase 1\n    $x = 1\nCase Else\n    $x = 2\nEndSelect\nSwitch $a\nCase 1\n    $b = 1\nEndSwitch\nWith $obj\n    $c = 1\nEndWith\n";
    let prog = parse(src).unwrap();
    assert_eq!(prog.items.len(), 3);
}

#[test]
fn parse_array_literal_init() {
    let src = "Local $a[] = [1, 2, 3]\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::VarDecl(v) = &s.kind else {
        panic!("expected var decl");
    };
    assert_eq!(v.vars[0].dims.len(), 1); // the [] dim
    let ExprKind::ArrayLit(items) = &v.vars[0].init.as_ref().unwrap().kind else {
        panic!("expected array literal init");
    };
    assert_eq!(items.len(), 3);
}

#[test]
fn parse_enum() {
    let src = "Global Enum $a, $b, $c\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::VarDecl(v) = &s.kind else {
        panic!("expected var decl");
    };
    assert!(v.is_enum);
    assert_eq!(v.vars.len(), 3);
}

#[test]
fn parse_stacked_scope_keywords() {
    let src = "Static Local $x = 1\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::VarDecl(v) = &s.kind else {
        panic!("expected var decl");
    };
    assert_eq!(v.kind, VarKind::Static);
}

#[test]
fn parse_indexed_call() {
    // $arr[0](args) — calling a function ref stored in an array element.
    let src = "$fn_table[0x10b]($a, $b)\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::Expr(e) = &s.kind else {
        panic!("expected expr stmt");
    };
    match &e.kind {
        ExprKind::IndexCall(v, args) => {
            assert_eq!(v.name.name, "$fn_table");
            assert_eq!(v.indices.len(), 1);
            assert_eq!(args.len(), 2);
        }
        other => panic!("expected IndexCall, got {other:?}"),
    }
}

#[test]
fn parse_error_reports_span() {
    let err = parse("Func\n").unwrap_err();
    assert!(err.msg.contains("expected identifier"));
}

// ---------------------------------------------------------------------------
// Pretty-printer round-trip
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Full-file smoke test (an obfuscated script, when one is supplied)
// ---------------------------------------------------------------------------

#[test]
fn parse_entire_obfuscated_target() {
    let Some(src) = sample_script() else { return };
    let prog = parse(&src).unwrap();
    let funcs = prog
        .items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count();
    // The file parsing at all is the real check; how large a given sample is
    // belongs to that sample, so only require that functions came out.
    assert!(funcs > 0, "no functions parsed from the sample");
}
// ---------------------------------------------------------------------------
// Regression: parser must accept `;` comments trailing after EndFunc
// (marker comments like `;==>MARKER` in real obfuscated targets).
// A stale release binary previously failed here with "expected expression";
// the root cause was parser/lexer comment handling, so this lives in the
// AST crate.
// ---------------------------------------------------------------------------

#[test]
fn parse_comment_after_endfunc_collects_comment() {
    let src = "Func F($x)\n    Return $x\nEndFunc   ;==>MARKER\r\n";
    let prog = parse(src).expect("must parse");
    // The comment is collected into the AST, not dropped.
    assert!(
        prog.comments.iter().any(|c| c.text.contains("MARKER")),
        "comment not collected: {:?}",
        prog.comments
    );
}

#[test]
fn parse_comment_after_endfunc_at_top_level() {
    let src = "Local $x = 1 ; trailing\n;==>MARKER\n";
    let prog = parse(src).expect("must parse");
    assert!(prog.comments.len() >= 2);
}

#[test]
fn parse_standalone_comment_lines_between_funcs() {
    let src = "Func A()\nEndFunc\n; between\nFunc B()\nEndFunc\n";
    let prog = parse(src).expect("must parse");
    assert!(prog.comments.iter().any(|c| c.text.contains("between")));
    assert_eq!(prog.items.len(), 2);
}

/// Read the optional obfuscated sample script used by the integration checks.
///
/// Point the `AU3_SAMPLE` environment variable at a real obfuscated AutoIt
/// script to enable them; they are skipped when it is unset or unreadable, so
/// `cargo test` stays green without any external fixture.
fn sample_script() -> Option<String> {
    let path = std::env::var("AU3_SAMPLE").ok().filter(|p| !p.is_empty())?;
    match std::fs::read_to_string(&path) {
        Ok(src) => Some(src),
        Err(e) => {
            eprintln!("skipping: cannot read {path}: {e}");
            None
        }
    }
}

#[test]
fn subscript_on_a_call_result_stays_part_of_the_expression() {
    // `DllCall(...)[0]` indexes what the call returned; it is not a statement
    // of its own, and it must not be turned into a concatenation.
    let src = "$x = $fn_table[0x273]($a, $b)[0x0]\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::Expr(e) = &s.kind else {
        panic!("expected expr stmt");
    };
    let ExprKind::Binary(_, _, rhs) = &e.kind else {
        panic!("expected assignment, got {:?}", e.kind);
    };
    let ExprKind::Subscript(base, indices) = &rhs.kind else {
        panic!("expected Subscript, got {:?}", rhs.kind);
    };
    assert!(matches!(base.kind, ExprKind::IndexCall(..)));
    assert_eq!(indices.len(), 1);
}

#[test]
fn chained_subscripts_on_a_call_accumulate() {
    let src = "$a = DllCall(\"x\", \"int\", \"y\")[0][1]\n";
    let prog = parse(src).unwrap();
    let ItemKind::Stmt(s) = &prog.items[0].kind else {
        panic!("expected stmt");
    };
    let StmtKind::Expr(e) = &s.kind else {
        panic!("expected expr stmt");
    };
    let ExprKind::Binary(_, _, rhs) = &e.kind else {
        panic!("expected assignment");
    };
    let ExprKind::Subscript(base, indices) = &rhs.kind else {
        panic!("expected Subscript, got {:?}", rhs.kind);
    };
    assert!(matches!(base.kind, ExprKind::Call(..)));
    assert_eq!(indices.len(), 2);
}

#[test]
fn non_ascii_text_survives_every_token_that_carries_it() {
    // The lexer walks bytes, so anything it hands back as text has to be
    // decoded as UTF-8. Casting a byte to `char` turned `示` (`e7 a4 ba`) into
    // mojibake and re-encoded that, which showed up in the output.
    let src = "#AutoIt3Wrapper_Res_Field=CompanyName|示例软件有限公司\n\
               ;行注释：也是中文\n\
               Func F()\n\
               \x20   Return \"字符串里的中文：你好\"\n\
               EndFunc\n";
    let prog = parse(src).expect("parses");

    let ItemKind::Directive(directive) = &prog.items[0].kind else {
        panic!("expected a directive, got {:?}", prog.items[0].kind);
    };
    assert!(directive.ends_with("示例软件有限公司"), "{directive}");

    let body = prog
        .items
        .iter()
        .find_map(|i| match &i.kind {
            ItemKind::Func(f) => Some(&f.body),
            _ => None,
        })
        .expect("function");
    let StmtKind::Return(Some(expr)) = &body[0].kind else {
        panic!("expected return");
    };
    let ExprKind::Lit(lit) = &expr.kind else {
        panic!("expected string literal, got {:?}", expr.kind);
    };
    let LitKind::Str(text) = &lit.kind else {
        panic!("expected a string, got {:?}", lit.kind);
    };
    assert_eq!(text, "字符串里的中文：你好");
}

#[test]
fn non_ascii_in_comments_is_kept_whole() {
    // Both comment kinds carry text through to the pretty-printer, so both
    // have to survive the byte walk.
    let src = ";中文行注释，不应乱码\n#cs\n    中文块注释：测试\n#ce\n$x = 1\n";
    let prog = parse(src).expect("parses");
    assert_eq!(prog.comments.len(), 2);
    assert_eq!(prog.comments[0].text.trim(), "中文行注释，不应乱码");
    assert!(prog.comments[1].text.contains("中文块注释：测试"), "{:?}", prog.comments[1]);
}

#[test]
fn an_identifier_caches_its_lookup_key() {
    use autoitv3_ast::ast::Ident;
    use autoitv3_ast::span::Span;

    // The key drops the `$` and folds case, so one key serves the variable and
    // function lookups the interpreter does.
    let id = Ident::new("$MixedCase", Span::default());
    assert_eq!(id.key(), "mixedcase");
    assert_eq!(id.key(), "mixedcase", "cached, and stable");

    // Renaming has to drop the cache, or a later lookup would use the old
    // spelling's key.
    let mut id = id;
    id.set_name("OtherName");
    assert_eq!(id.name, "OtherName");
    assert_eq!(id.key(), "othername");
}
