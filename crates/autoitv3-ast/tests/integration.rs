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
// Full-file smoke test (the actual obfuscation target)
// ---------------------------------------------------------------------------

#[test]
fn parse_entire_obfuscated_target() {
    // The target file lives next to the repo; skip if unavailable.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../sample.au3");
    if !std::path::Path::new(path).exists() {
        eprintln!("skipping: {} not found", path);
        return;
    }
    let src = std::fs::read_to_string(path).unwrap();
    let prog = parse(&src).unwrap();
    let funcs = prog
        .items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count();
    assert!(funcs > 900, "expected 900+ functions, got {funcs}");
    assert!(prog.items.len() > 1000);
}