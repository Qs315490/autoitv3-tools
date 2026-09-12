//! Syntax coverage corpus.
//!
//! A systematic sweep of AutoIt v3 language constructs, checked against the
//! parser. It exists because coverage used to be established sample-by-sample:
//! the tool parsed the one real script it was developed against, which said
//! nothing about constructs that script happened not to use.
//!
//! Two lists drive the sweep — [`VALID`] and [`INVALID`] — plus targeted
//! assertions for the constructs whose *meaning* matters (and not just whether
//! they lex).

use autoitv3_ast::ast::*;
use autoitv3_ast::parse;

/// Constructs that must parse. Keep entries small and self-contained.
const VALID: &[(&str, &str)] = &[
    // ---- preprocessor ----
    ("include", "#include <Array.au3>\n"),
    ("include-once", "#include-once\n#include <Misc.au3>\n"),
    ("pragma", "#pragma compile(Console, True)\n"),
    ("OnAutoItStartRegister", "#OnAutoItStartRegister \"Start\"\n"),
    ("forceref", "Func F($a)\n    #forceref $a\nEndFunc\n"),
    ("region", "#Region main\n$x = 1\n#EndRegion\n"),
    ("block comment #cs", "#cs\njunk !!! @@@\nEndFunc x\n#ce\n$x = 1\n"),
    ("block comment long", "#comments-start\njunk !!!\n#comments-end\n$x = 1\n"),
    ("block comment case", "#CS\njunk\n#CE\n$x = 1\n"),
    ("unterminated block comment", "$x = 1\n#cs\nrest of file\n"),

    // ---- line continuation ----
    ("continuation operand", "$x = 1 + _\n     2\n"),
    ("continuation argument", "MsgBox(0, \"t\", _\n    \"b\")\n"),
    ("continuation with comment", "Local $a[2] = [ _\n    1, _; c1\n    2]\n"),
    ("continuation in condition", "If $a = 1 And _\n   $b = 2 Then\nEndIf\n"),
    ("underscore-prefixed name is not a continuation", "Func _MyFunc()\nEndFunc\n"),
    ("trailing underscore in identifier", "Local $aArray_1_\nLocal $b = 1\n"),

    // ---- literals ----
    ("double-quoted string", "$x = \"abc\"\n"),
    ("single-quoted string", "$x = 'abc'\n"),
    ("double-quote escape", "$x = \"a\"\"b\"\n"),
    ("single-quote escape", "$x = 'a''b'\n"),
    ("quotes inside other quotes", "$x = 'he said \"hi\"'\n"),
    ("decimal", "$x = 123\n"),
    ("hex", "$x = 0xFF\n"),
    ("hex uppercase", "$x = 0XFF\n"),
    ("scientific", "$x = 1e5\n"),
    ("scientific negative exponent", "$x = 1.5e-3\n"),
    ("float", "$x = 1.5\n"),

    // ---- operators ----
    ("equals comparison in If", "If $a = 1 Then\nEndIf\n"),
    ("double equals", "If $a == 1 Then\n    $b = 2\nEndIf\n"),
    ("not equal", "If $a <> 1 Then\n    $b = 2\nEndIf\n"),
    ("And Or Not", "If Not $a And $b Or $c Then\n    $b = 1\nEndIf\n"),
    ("power", "$x = 2 ^ 3\n"),
    ("concat", "$x = \"a\" & \"b\"\n"),
    (
        "compound assignment",
        "$a += 1\n$b -= 1\n$c *= 2\n$d /= 2\n$e &= \"x\"\n$f ^= 2\n",
    ),
    ("ternary", "$x = $a ? 1 : 2\n"),

    // ---- statements ----
    ("single-line If", "If $a Then $b = 1\n"),
    ("While", "While $a\n    $b = 1\nWEnd\n"),
    ("Do Until", "Do\n    $b = 1\nUntil $a\n"),
    ("For To Step", "For $i = 1 To 10 Step 2\n    $b = 1\nNext\n"),
    ("For In", "For $v In $a\n    $b = 1\nNext\n"),
    (
        "Select",
        "Select\n    Case $a = 1\n        $b = 1\n    Case Else\n        $b = 2\nEndSelect\n",
    ),
    (
        "Switch",
        "Switch $a\n    Case 1, 2\n        $b = 1\n    Case Else\n        $b = 2\nEndSwitch\n",
    ),
    (
        "ContinueCase",
        "Switch $a\n    Case 1\n        ContinueCase\n    Case 2\n        $b = 1\nEndSwitch\n",
    ),
    ("ExitLoop and ContinueLoop", "For $i = 1 To 2\n    ExitLoop\n    ContinueLoop\nNext\n"),
    ("Exit", "Exit\n"),
    ("Exit with code", "Exit 0\n"),
    ("Return without value", "Func F()\n    Return\nEndFunc\n"),
    ("ReDim", "Local $a[3]\nReDim $a[5]\n"),
    ("ReDim multi-dimensional", "Local $a[2][2]\nReDim $a[3][3]\n"),
    ("colon-separated statements", "$x = 1 : $y = 2\n"),

    // ---- declarations ----
    (
        "scopes",
        "Local $a\nGlobal $b\nDim $c\nGlobal Const $D = 1\n",
    ),
    ("Static", "Func F()\n    Static $n = 0\nEndFunc\n"),
    ("Static Local", "Func F()\n    Static Local $n = 0\nEndFunc\n"),
    ("Enum with values", "Global Enum $A = 1, $B, $C\n"),
    ("Enum without values", "Global Enum $A, $B, $C\n"),
    ("bare Enum", "Enum $A, $B\n"),
    ("Enum Step", "Global Enum Step 2 $A, $B\n"),
    ("multiple declarators", "Local $a = 1, $b = 2\n"),
    ("multi-dimensional array", "Local $a[2][3]\n"),
    ("array literal", "Local $a[] = [1, 2, 3]\n"),
    ("nested array literal", "Local $a[][] = [[1, 2], [3, 4]]\n"),
    ("empty brackets declare a Map", "Local $m[]\n"),

    // ---- functions ----
    ("basic function", "Func F()\nEndFunc\n"),
    ("function without parentheses", "Func F\nEndFunc\n"),
    ("default parameter", "Func F($a = 1)\nEndFunc\n"),
    ("ByRef parameter", "Func F(ByRef $a)\nEndFunc\n"),
    ("Const ByRef parameter", "Func F(Const ByRef $a)\nEndFunc\n"),
    ("ByRef Const parameter", "Func F(ByRef Const $a)\nEndFunc\n"),
    ("underscore-prefixed name", "Func _MyFunc()\nEndFunc\n"),
    ("Volatile function", "Volatile Func F()\nEndFunc\n"),

    // ---- objects / COM ----
    ("method call", "$obj = ObjCreate(\"X\")\n$obj.Method()\n"),
    ("method call with args", "$obj.Method(1, 2)\n"),
    ("property read", "$x = $obj.Property\n"),
    ("property write", "$obj.Property = 1\n"),
    ("member chain", "$x = $obj.Sub.Prop\n"),
    ("method chain", "$obj.A().B()\n"),
    (
        "With block members",
        "With $obj\n    .Value = 1\n    .Method(2)\nEndWith\n",
    ),
    ("member in condition", "If $obj.Ready Then\n    $x = 1\nEndIf\n"),
    ("member as argument", "F($obj.Prop, $obj.M())\n"),
    ("With inline subject", "With ObjCreate(\"X\")\n    .Prop = 1\nEndWith\n"),

    // ---- macros & keywords ----
    ("macro concat", "$x = \"a\" & @CRLF\n"),
    ("macro read", "$x = @error\n"),
    ("booleans", "$a = True\n$b = False\n"),
    ("Null", "$a = Null\n"),
    ("Default parameter", "Func F($a = Default)\nEndFunc\n"),
    ("Call()", "Call(\"MyFunc\")\n"),
    ("variable function call", "$f = \"MyFunc\"\n$f()\n"),
    ("indexed call", "$a[0]()\n"),
    ("multi-dimensional index", "$a[1][2] = 3\n"),

    // ---- misc ----
    ("empty file", "\n"),
    ("comment only", "; just a comment\n"),
    ("trailing comment", "$x = 1 ; c\n"),
    ("CRLF line endings", "$x = 1\r\n$y = 2\r\n"),
];

/// Constructs AutoIt does *not* allow; the parser must reject them.
const INVALID: &[(&str, &str)] = &[
    // AutoIt has no nested functions.
    ("nested Func", "Func A()\n    Func B()\n    EndFunc\nEndFunc\n"),
    // The single-line `If` form is `If <expr> Then <statement>` — no Else.
    ("single-line If/Else", "If $a Then $b = 1 Else $c = 2\n"),
    // A `.` needs a member name.
    ("lone dot", "$x = .\n"),
    ("unterminated string", "$x = \"abc\n"),
    ("unterminated single-quoted string", "$x = 'abc\n"),
];

#[test]
fn valid_constructs_parse() {
    let mut failures = Vec::new();
    for (name, src) in VALID {
        if let Err(e) = parse(src) {
            failures.push(format!("  {name}: {e}\n    source: {src:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} constructs failed to parse:\n{}",
        failures.len(),
        VALID.len(),
        failures.join("\n")
    );
}

#[test]
fn invalid_constructs_are_rejected() {
    for (name, src) in INVALID {
        assert!(
            parse(src).is_err(),
            "{name} should not parse, but did: {src:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Constructs whose *meaning* matters, not just whether they lex.
// ---------------------------------------------------------------------------

/// The first expression-statement of a single-statement program.
fn first_stmt_expr(src: &str) -> Expr {
    let prog = parse(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    for item in &prog.items {
        if let ItemKind::Stmt(s) = &item.kind {
            match &s.kind {
                StmtKind::Expr(e) => return e.clone(),
                StmtKind::VarDecl(v) => {
                    if let Some(init) = v.vars.first().and_then(|i| i.init.clone()) {
                        return init;
                    }
                }
                _ => {}
            }
        }
    }
    panic!("no expression statement in {src:?}");
}

#[test]
fn equals_is_assignment_at_statement_level() {
    match first_stmt_expr("$x = 1\n").kind {
        ExprKind::Binary(BinaryOp::Assign, _, _) => {}
        other => panic!("expected Assign, got {other:?}"),
    }
}

#[test]
fn equals_is_comparison_inside_expressions() {
    // `If $a = 1 Then` compares; only `==` is the case-sensitive spelling, so
    // `=` must not be modelled as an assignment here.
    let prog = parse("If $a = 1 Then\nEndIf\n").unwrap();
    let ItemKind::Stmt(st) = &prog.items[0].kind else { panic!() };
    let StmtKind::If(if_) = &st.kind else { panic!() };
    match &if_.cond.kind {
        ExprKind::Binary(BinaryOp::EqLoose, _, _) => {}
        other => panic!("expected EqLoose, got {other:?}"),
    }
}

#[test]
fn double_equals_stays_case_sensitive_equality() {
    let prog = parse("If $a == 1 Then\nEndIf\n").unwrap();
    let ItemKind::Stmt(st) = &prog.items[0].kind else { panic!() };
    let StmtKind::If(if_) = &st.kind else { panic!() };
    match &if_.cond.kind {
        ExprKind::Binary(BinaryOp::Eq, _, _) => {}
        other => panic!("expected Eq, got {other:?}"),
    }
}

#[test]
fn continuation_joins_lines_into_one_statement() {
    let prog = parse("$x = 1 + _\n     2\n").unwrap();
    // Exactly one statement: the second line is part of the first.
    let stmts: Vec<_> = prog
        .items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Stmt(_)))
        .collect();
    assert_eq!(stmts.len(), 1, "continuation should not split the statement");
    // ...and no stray `_` identifier survives.
    assert!(
        !format!("{prog:?}").contains("\"_\"") || !format!("{prog:?}").contains("Ident { name: \"_\""),
        "the underscore leaked into the AST"
    );
}

#[test]
fn continue_loop_is_distinct_from_exit_loop() {
    let prog = parse("For $i = 1 To 2\n    ExitLoop\n    ContinueLoop\nNext\n").unwrap();
    let ItemKind::Stmt(st) = &prog.items[0].kind else { panic!() };
    let StmtKind::For(f) = &st.kind else { panic!() };
    assert!(matches!(f.body[0].kind, StmtKind::ExitLoop(_)));
    assert!(matches!(f.body[1].kind, StmtKind::ContinueLoop(_)));
}

#[test]
fn redim_is_distinct_from_dim_const() {
    let prog = parse("ReDim $a[5]\nDim Const $b = 1\n").unwrap();
    let kinds: Vec<_> = prog
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Stmt(s) => match &s.kind {
                StmtKind::VarDecl(v) => Some((v.is_redim, v.is_const, v.is_enum)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![(true, false, false), (false, true, false)]);
}

#[test]
fn enum_step_and_members_are_captured() {
    let prog = parse("Global Enum Step 2 $A, $B\n").unwrap();
    let ItemKind::Stmt(st) = &prog.items[0].kind else { panic!() };
    let StmtKind::VarDecl(v) = &st.kind else { panic!() };
    assert!(v.is_enum);
    assert!(v.enum_step.is_some(), "Step 2 should be captured");
    assert_eq!(v.vars.len(), 2);
}

#[test]
fn block_comment_is_preserved_as_one_comment() {
    let prog = parse("#cs\njunk !!!\n#ce\n$x = 1\n").unwrap();
    assert_eq!(prog.comments.len(), 1);
    assert!(prog.comments[0].block);
    assert!(prog.comments[0].text.starts_with("#cs"));
}

#[test]
fn volatile_function_is_flagged() {
    let prog = parse("Volatile Func F()\nEndFunc\n").unwrap();
    let ItemKind::Func(f) = &prog.items[0].kind else { panic!() };
    assert!(f.is_volatile);
}

#[test]
fn continuecase_is_a_control_statement_not_an_identifier() {
    // Parsing it as a bare identifier would silently turn a jump into a no-op
    // expression, which is exactly the kind of thing this corpus exists to
    // catch.
    let prog = parse("Switch $a\n    Case 1\n        ContinueCase\n    Case 2\n        $b = 1\nEndSwitch\n")
        .unwrap();
    let ItemKind::Stmt(outer) = &prog.items[0].kind else { panic!() };
    let StmtKind::Switch(sw) = &outer.kind else { panic!("{:?}", outer.kind) };
    assert!(
        matches!(sw.cases[0].body[0].kind, StmtKind::ContinueCase),
        "{:?}",
        sw.cases[0].body[0].kind
    );
}

#[test]
fn with_block_member_uses_the_implicit_subject() {
    let prog = parse("With $obj\n    .Value = 1\nEndWith\n").unwrap();
    let ItemKind::Stmt(st) = &prog.items[0].kind else { panic!() };
    let StmtKind::With(w) = &st.kind else { panic!() };
    let StmtKind::Expr(e) = &w.body[0].kind else { panic!("{:?}", w.body[0].kind) };
    match &e.kind {
        // `.Value = 1` is an assignment whose target is a member of the
        // implicit `With` subject.
        ExprKind::Binary(BinaryOp::Assign, lhs, _) => match &lhs.kind {
            ExprKind::Member(recv, name) => {
                assert!(matches!(recv.kind, ExprKind::WithSubject));
                assert_eq!(name.name, "Value");
            }
            other => panic!("expected Member target, got {other:?}"),
        },
        other => panic!("expected Assign, got {other:?}"),
    }
}

#[test]
fn member_and_method_shapes_are_distinct() {
    match first_stmt_expr("$x = $obj.Prop\n").kind {
        ExprKind::Binary(BinaryOp::Assign, _, rhs) => match &rhs.kind {
            ExprKind::Member(recv, name) => {
                assert!(matches!(recv.kind, ExprKind::Var(_)));
                assert_eq!(name.name, "Prop");
            }
            other => panic!("expected Member, got {other:?}"),
        },
        other => panic!("expected Assign, got {other:?}"),
    }

    match first_stmt_expr("$obj.Method(1)\n").kind {
        ExprKind::MethodCall(recv, name, args) => {
            assert!(matches!(recv.kind, ExprKind::Var(_)));
            assert_eq!(name.name, "Method");
            assert_eq!(args.len(), 1);
        }
        other => panic!("expected MethodCall, got {other:?}"),
    }
}

#[test]
fn single_quoted_strings_decode_like_double_quoted() {
    let a = parse("$x = 'it''s'\n").unwrap();
    let b = parse("$x = \"it's\"\n").unwrap();
    let text = |p: &Program| match &p.items[0].kind {
        ItemKind::Stmt(s) => match &s.kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Binary(_, _, rhs) => match &rhs.kind {
                    ExprKind::Lit(Lit { kind: LitKind::Str(s), .. }) => s.clone(),
                    other => panic!("{other:?}"),
                },
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert_eq!(text(&a), "it's");
    assert_eq!(text(&b), "it's");
}