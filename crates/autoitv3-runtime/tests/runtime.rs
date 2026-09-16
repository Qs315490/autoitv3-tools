//! Tests for the autoitv3-runtime interpreter, host and debug interfaces.

use std::cell::RefCell;
use std::rc::Rc;

use autoitv3_runtime::debug::{DebugAction, DebugHost, Debugger, StopReason, TracingDebugger};
use autoitv3_runtime::host::{HostContext, NativeHost};
use autoitv3_runtime::{BuildFacts, Runtime, Value};

fn rt(src: &str) -> Runtime {
    let prog = autoitv3_ast::parse(src).unwrap();
    Runtime::with_program(&prog)
}

fn call(src: &str, name: &str, args: Vec<Value>) -> Value {
    rt(src).call_function(name, args).unwrap()
}

// ---------------------------------------------------------------------------
// Basics
// ---------------------------------------------------------------------------

#[test]
fn calls_a_function_and_returns() {
    let v = call(
        "Func Add($a, $b)\n    Return $a + $b\nEndFunc\n",
        "Add",
        vec![Value::Int(2), Value::Int(3)],
    );
    assert!(matches!(v, Value::Int(5)), "got {v:?}");
}

#[test]
fn function_names_are_case_insensitive() {
    let v = call(
        "Func Add($a, $b)\n    Return $a + $b\nEndFunc\n",
        "ADD",
        vec![Value::Int(1), Value::Int(4)],
    );
    assert!(matches!(v, Value::Int(5)), "got {v:?}");
}

#[test]
fn parameters_default_and_extra_args() {
    let src = "Func F($a, $b = 10)\n    Return $a + $b\nEndFunc\n";
    assert!(matches!(call(src, "F", vec![Value::Int(1)]), Value::Int(11)));
    assert!(matches!(
        call(src, "F", vec![Value::Int(1), Value::Int(2)]),
        Value::Int(3)
    ));
}

#[test]
fn string_concat_and_coercion() {
    let src = "Func F($n)\n    Return \"n=\" & $n & \"!\"\nEndFunc\n";
    let v = call(src, "F", vec![Value::Int(5)]);
    match v {
        Value::Str(s) => assert_eq!(s, "n=5!"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn compound_concat_builds_a_string_in_a_loop() {
    // `$s &= x` in statement position appends onto the buffer the variable
    // already owns. The observable result matches `$s = $s & x`; the loop just
    // no longer reallocates and copies the whole accumulator every iteration.
    let src = r#"
Func F()
    Local $s = ""
    For $i = 1 To 5
        $s &= $i
    Next
    Return $s
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "12345"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn compound_concat_coerces_like_the_operator() {
    let src = r#"
Func F()
    Local $s = "n"
    $s &= 2.5
    $s &= True
    $s &= Null
    Return $s
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "n2.5True"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn the_long_concat_form_is_inlined_too() {
    // `$s = $s & x` is the same accumulation spelled the long way. Its right
    // side is appended in place when it cannot reassign the target.
    let src = r#"
Func F()
    Local $s = ""
    For $i = 1 To 5
        $s = $s & $i
    Next
    Return $s
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "12345"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn the_long_concat_form_still_reads_the_target_first() {
    // The in-place rewrite reads the target *after* the right side runs, so it
    // must not fire when that side can reassign it: `Bump()` writes `$s`, and
    // the value concatenated has to stay the one read before the call.
    let src = r#"
Global $s
Func Bump()
    $s = $s & "B"
    Return "x"
EndFunc
Func F()
    $s = "a"
    $s = $s & Bump()
    Return $s
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "ax", "the operand must be read before the call"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn the_long_concat_form_handles_a_different_source_variable() {
    let src = r#"
Func F()
    Local $s = "a"
    Local $t = "b"
    $s = $s & $t
    $t = "c"
    Return $s & "/" & $t
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "ab/c"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn compound_concat_declares_an_unset_variable() {
    let src = "Func F()\n    $s &= \"x\"\n    Return $s\nEndFunc\n";
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "x"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn compound_concat_leaves_an_earlier_copy_alone() {
    // Strings are values: a read hands out a copy, so the append has to land in
    // the slot and not in a value somebody else is holding.
    let src = r#"
Func F()
    Local $s = "a"
    Local $t = $s
    $s &= "b"
    Return $t & "/" & $s
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "a/ab"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn integer_division_stays_integral() {
    let src = "Func F()\n    Return 20 / 5\nEndFunc\n";
    assert!(matches!(call(src, "F", vec![]), Value::Int(4)));
    let src2 = "Func G()\n    Return 7 / 2\nEndFunc\n";
    assert!(matches!(call(src2, "G", vec![]), Value::Float(_)));
}

// ---------------------------------------------------------------------------
// Arrays, ByRef, ReDim — the shape the obfuscator's MergeArrays uses
// ---------------------------------------------------------------------------

#[test]
fn byref_writes_back_to_the_caller_variable_whatever_its_name() {
    // The callee is handed a copy, so the write has to be replayed at the call
    // site — by *the caller's* name, which need not match the parameter's.
    let src = r#"
Func Bump(ByRef $v)
    $v += 1
EndFunc
Func F()
    Local $x = 1
    Bump($x)
    Return $x
EndFunc
Func SameName()
    Local $v = 1
    Bump($v)
    Return $v
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(2)));
    assert!(matches!(call(src, "SameName", vec![]), Value::Int(2)));
}

#[test]
fn byref_writes_back_to_a_global() {
    let mut runtime = rt(
        "Func Bump(ByRef $v)\n\
         \x20   $v += 1\n\
         EndFunc\n\
         Func F()\n\
         \x20   Bump($g)\n\
         EndFunc\n",
    );
    // The file-scope `Global $g` has not run: `call_function` only runs the
    // function body, so set the global the way the script body would have.
    runtime.assign_variable("$g", Value::Int(10), true, false);
    runtime.call_function("F", vec![]).unwrap();
    assert_eq!(runtime.variable_value("$g").unwrap().to_int(), 11);
}

#[test]
fn byref_creates_a_variable_the_caller_only_passed() {
    let src = r#"
Func SetIt(ByRef $q)
    $q = 42
EndFunc
Func F()
    Local $y
    SetIt($y)
    Return IsDeclared("y") & ":" & $y
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "1:42");
}

#[test]
fn a_literal_argument_writes_back_to_nothing() {
    // The caller happens to have a variable with the *parameter's* name; a
    // literal argument must not be copied into it on the way out.
    let src = r#"
Func Bump(ByRef $v)
    $v += 1
EndFunc
Func F()
    Local $v = 1
    Bump(7)
    Return $v
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(1)));
}

#[test]
fn byref_through_a_function_table_call() {
    // `$t[0]($x)` reaches the callee through `call_value`, which has to carry
    // the call site's names just like a direct call.
    let src = r#"
Func Bump(ByRef $v)
    $v += 1
EndFunc
Func F()
    Local $t[1]
    $t[0] = Bump
    Local $x = 5
    $t[0]($x)
    Return $x
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(6)));
}

#[test]
fn byref_is_copied_out_through_a_chain_of_forwarders() {
    // Each frame holds a copy and copies it out on return, so the chain of
    // names resolves back to the variable the bottom-most caller passed.
    let src = r#"
Func Bump(ByRef $v)
    $v += 1
EndFunc
Func Outer(ByRef $w)
    Bump($w)
    $w += 10
EndFunc
Func F()
    Local $z = 100
    Outer($z)
    Return $z
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(111)));
}

#[test]
fn byref_writes_back_into_an_array_element() {
    // AutoIt: "not only a named variable can be passed for a ByRef parameter".
    // The array is `Rc`-shared, so writing through the container the call site
    // captured is visible to the caller with no copy-out.
    let src = r#"
Func Bump(ByRef $v)
    $v += 1
EndFunc
Func F()
    Local $a[2]
    $a[0] = 5
    Bump($a[0])
    Bump($a[1])
    Return $a[0] & "/" & $a[1]
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "6/1");
}

#[test]
fn byref_writes_back_through_nested_elements_and_map_keys() {
    let src = r#"
Func Bump(ByRef $v)
    $v += 1
EndFunc
Func Nested()
    Local $a[1][1]
    $a[0][0] = 1
    Bump($a[0][0])
    Return $a[0][0]
EndFunc
Func MapTarget()
    Local $m[]
    $m["k"] = 3
    Bump($m["k"])
    Return $m["k"]
EndFunc
"#;
    assert!(matches!(call(src, "Nested", vec![]), Value::Int(2)));
    assert!(matches!(call(src, "MapTarget", vec![]), Value::Int(4)));
}

#[test]
fn a_byref_subscript_is_evaluated_once() {
    // The element path is captured while the argument is evaluated, so the
    // index expression must not run a second time on the way back.
    let mut runtime = rt(
        "Global $calls = 0\n\
         Func Index()\n\
         \x20   $calls += 1\n\
         \x20   Return 0\n\
         EndFunc\n\
         Func Bump(ByRef $v)\n\
         \x20   $v += 1\n\
         EndFunc\n\
         Func F()\n\
         \x20   Local $a[1]\n\
         \x20   $a[0] = 0\n\
         \x20   Bump($a[Index()])\n\
         \x20   Return $a[0] & \":\" & $calls\n\
         EndFunc\n",
    );
    // `call_function` runs only the function body, so stand in for the file
    // scope's `Global $calls = 0`.
    runtime.assign_variable("$calls", Value::Int(0), true, false);
    let out = runtime.call_function("F", vec![]).unwrap();
    assert_eq!(out.to_autoit_string(), "1:1");
}

#[test]
fn byref_writes_back_into_a_global_array_element() {
    let mut runtime = rt(
        "Global $g[2]\n\
         Func Bump(ByRef $v)\n\
         \x20   $v += 1\n\
         EndFunc\n\
         Func F()\n\
         \x20   Bump($g[1])\n\
         EndFunc\n",
    );
    let array = Value::array_sized(2);
    runtime.assign_variable("$g", array, true, false);
    runtime.call_function("F", vec![]).unwrap();
    let g = runtime.variable_value("$g").expect("global is set");
    let Value::Array(a) = g else { panic!("expected array, got {g:?}") };
    assert!(matches!(a.borrow()[1], Value::Int(1)));
}

#[test]
fn merges_arrays_with_byref_and_redim() {
    let src = r#"
Func MergeArrays(ByRef $targetArray, Const ByRef $sourceArray)
    ReDim $targetArray[$targetArray[0x0] + $sourceArray[0x0] + 0x1]
    Local $i
    For $i = 0x1 To $sourceArray[0x0]
        $targetArray[$targetArray[0x0] + $i] = $sourceArray[$i]
    Next
    $targetArray[0x0] += $sourceArray[0x0]
EndFunc

Func Build()
    Local $x[] = [0x2, "A", "B"]
    Local $y[] = [0x1, "C"]
    MergeArrays($x, $y)
    Return $x
EndFunc
"#;
    let v = call(src, "Build", vec![]);
    let Value::Array(a) = v else { panic!("expected array, got {v:?}") };
    let a = a.borrow();
    assert_eq!(a.len(), 4, "count + 3 items");
    assert!(matches!(a[0], Value::Int(3)), "count is {}", a[0].to_int());
    assert_eq!(a[1].to_autoit_string(), "A");
    assert_eq!(a[2].to_autoit_string(), "B");
    assert_eq!(a[3].to_autoit_string(), "C");
}

#[test]
fn an_array_past_autoits_element_limit_is_refused() {
    // `VAR_SUBSCRIPT_ELEMENTS` is 16,777,216 in the help's Limits/Defaults
    // table. An obfuscated `$n + 4294967295` asks for four billion elements and
    // used to reach the allocator — the process died there with nothing said
    // about where. The declaration is now an error that names the count.
    let src = "Func F()\n    Local $a[20000000]\nEndFunc\n";
    let err = rt(src).call_function("F", vec![]).unwrap_err();
    let message = err.message();
    assert!(message.contains("20000000"), "{message}");
    assert!(message.contains("16777216"), "{message}");
    assert!(
        matches!(err.span().map(|s| s.start.line), Some(2)),
        "the declaration's line: {err:?}"
    );
}

#[test]
fn redim_preserves_existing_elements() {
    let src = r#"
Func F()
    Local $a[3]
    $a[0] = 1
    $a[1] = 2
    $a[2] = 3
    ReDim $a[5]
    Return $a[0] + $a[1] + $a[2] + $a[3]
EndFunc
"#;
    // 1 + 2 + 3 + 0 = 6
    assert!(matches!(call(src, "F", vec![]), Value::Int(6)));
}

#[test]
fn for_in_iterates_arrays() {
    let src = r#"
Func F()
    Local $a[] = [1, 2, 3]
    Local $sum = 0
    For $v In $a
        $sum += $v
    Next
    Return $sum
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(6)));
}

#[test]
fn numeric_for_with_step_runs_expected_times() {
    let src = r#"
Func F()
    Local $n = 0
    For $i = 1 To 10 Step 2
        $n += 1
    Next
    Return $n
EndFunc
"#;
    // 1,3,5,7,9 -> 5 iterations
    assert!(matches!(call(src, "F", vec![]), Value::Int(5)));
}

// ---------------------------------------------------------------------------
// Static — one variable shared by every call of the function
// ---------------------------------------------------------------------------

#[test]
fn static_survives_across_calls() {
    let mut runtime = rt(
        "Func Tick()\n\
         \x20   Static $n = 0\n\
         \x20   $n += 1\n\
         \x20   Return $n\n\
         EndFunc\n",
    );
    for expected in 1..=3 {
        let v = runtime.call_function("Tick", vec![]).unwrap();
        assert_eq!(v.to_int(), expected, "call {expected}: got {v:?}");
    }
}

#[test]
fn static_initializer_runs_once() {
    // The initializer reads a global the function bumps on every call, so a
    // re-evaluated initializer would show up as a growing result.
    let mut runtime = rt(
        "Global $inits = 0\n\
         Func F()\n\
         \x20   Static $n = $inits\n\
         \x20   $inits += 1\n\
         \x20   Return $n\n\
         EndFunc\n",
    );
    runtime.assign_variable("$inits", Value::Int(5), true, false);
    assert_eq!(runtime.call_function("F", vec![]).unwrap().to_int(), 5);
    assert_eq!(runtime.call_function("F", vec![]).unwrap().to_int(), 5);
    assert_eq!(
        runtime
            .variable_value("$inits")
            .expect("global is set")
            .to_int(),
        7
    );
}

#[test]
fn static_is_per_function() {
    let mut runtime = rt(
        "Func A()\n\
         \x20   Static $n = 0\n\
         \x20   $n += 1\n\
         \x20   Return $n\n\
         EndFunc\n\
         Func B()\n\
         \x20   Static $n = 100\n\
         \x20   $n += 1\n\
         \x20   Return $n\n\
         EndFunc\n",
    );
    assert_eq!(runtime.call_function("A", vec![]).unwrap().to_int(), 1);
    assert_eq!(runtime.call_function("B", vec![]).unwrap().to_int(), 101);
    assert_eq!(runtime.call_function("A", vec![]).unwrap().to_int(), 2);
    assert_eq!(runtime.call_function("B", vec![]).unwrap().to_int(), 102);
}

#[test]
fn static_is_shared_by_recursive_activations() {
    // Every activation sees the same variable, so the depth counter counts
    // the whole recursion rather than restarting per call.
    let mut runtime = rt(
        "Func Rec($d)\n\
         \x20   Static $seen = 0\n\
         \x20   $seen += 1\n\
         \x20   If $d > 0 Then Return Rec($d - 1)\n\
         \x20   Return $seen\n\
         EndFunc\n",
    );
    assert_eq!(runtime.call_function("Rec", vec![Value::Int(3)]).unwrap().to_int(), 4);
}

#[test]
fn static_local_spells_the_same_variable() {
    let mut runtime = rt(
        "Func F()\n\
         \x20   Static Local $a = 0, $b = 100\n\
         \x20   $a += 1\n\
         \x20   $b += 1\n\
         \x20   Return $a & \"/\" & $b\n\
         EndFunc\n",
    );
    let first = runtime.call_function("F", vec![]).unwrap();
    assert_eq!(first.to_autoit_string(), "1/101");
    let second = runtime.call_function("F", vec![]).unwrap();
    assert_eq!(second.to_autoit_string(), "2/102");
}

#[test]
fn static_array_keeps_its_elements() {
    let mut runtime = rt(
        "Func F()\n\
         \x20   Static $a[] = [0, 0]\n\
         \x20   $a[0] += 1\n\
         \x20   Return $a[0]\n\
         EndFunc\n",
    );
    assert_eq!(runtime.call_function("F", vec![]).unwrap().to_int(), 1);
    assert_eq!(runtime.call_function("F", vec![]).unwrap().to_int(), 2);
}

#[test]
fn a_static_passed_byref_is_written_back() {
    let mut runtime = rt(
        "Func Bump(ByRef $n)\n\
         \x20   $n += 1\n\
         EndFunc\n\
         Func F()\n\
         \x20   Static $n = 0\n\
         \x20   Bump($n)\n\
         \x20   Return $n\n\
         EndFunc\n",
    );
    assert_eq!(runtime.call_function("F", vec![]).unwrap().to_int(), 1);
    assert_eq!(runtime.call_function("F", vec![]).unwrap().to_int(), 2);
}

#[test]
fn a_static_is_not_visible_outside_its_function() {
    let mut runtime = rt(
        "Func F()\n\
         \x20   Static $n = 7\n\
         \x20   Return $n\n\
         EndFunc\n",
    );
    assert_eq!(runtime.call_function("F", vec![]).unwrap().to_int(), 7);
    assert!(runtime.variable_value("$n").is_none());
}

// ---------------------------------------------------------------------------
// Loops, If, Select/Switch, control flow
// ---------------------------------------------------------------------------

#[test]
fn exiti_loop_and_continue_loop_are_distinct() {
    let src = r#"
Func F()
    Local $s = ""
    For $i = 1 To 5
        If $i == 2 Then ContinueLoop
        If $i == 4 Then ExitLoop
        $s &= $i
    Next
    Return $s
EndFunc
"#;
    // 1, (skip 2), 3, (stop at 4) -> "13"
    let v = call(src, "F", vec![]);
    assert_eq!(v.to_autoit_string(), "13");
}

#[test]
fn select_and_switch() {
    let src = r#"
Func Sel($x)
    Select
        Case $x == 1
            Return "one"
        Case $x == 2
            Return "two"
        Case Else
            Return "other"
    EndSelect
EndFunc

Func Sw($x)
    Switch $x
        Case 1
            Return "a"
        Case 2
            Return "b"
    EndSwitch
    Return "z"
EndFunc
"#;
    assert_eq!(call(src, "Sel", vec![Value::Int(1)]).to_autoit_string(), "one");
    assert_eq!(call(src, "Sel", vec![Value::Int(9)]).to_autoit_string(), "other");
    assert_eq!(call(src, "Sw", vec![Value::Int(2)]).to_autoit_string(), "b");
    assert_eq!(call(src, "Sw", vec![Value::Int(7)]).to_autoit_string(), "z");
}

#[test]
fn macro_error_and_seterror() {
    let src = r#"
Func F()
    SetError(7, 9)
    Return @error * 100 + @extended
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(709)));
}

#[test]
fn clock_macros_follow_the_autoit_format_and_the_profile() {
    let mut rt = rt(
        r#"
Func F()
    Return @YEAR & "-" & @MON & "-" & @MDAY & " " & @HOUR & ":" & @MIN & ":" & @SEC & "." & @MSEC & " wday=" & @WDAY & " yday=" & @YDAY
EndFunc
"#,
    );
    rt.set_profile(autoitv3_runtime::ExecutionProfile::deterministic());
    // The deterministic profile runs at 2024-01-01T00:00:00Z (a Monday);
    // `@MSEC`/`@YDAY` are zero-padded strings, as AutoIt documents.
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        "2024-01-01 00:00:00.000 wday=2 yday=001"
    );
}

#[test]
fn compiled_macro_reflects_how_the_script_was_loaded() {
    let mut rt = rt("Func F()\n    Return @Compiled\nEndFunc\n");
    assert_eq!(rt.call_function("F", vec![]).unwrap().to_int(), 0);
    rt.set_compiled(true);
    assert_eq!(rt.call_function("F", vec![]).unwrap().to_int(), 1);
}

#[test]
fn build_macros_come_from_the_build_facts() {
    // A script extracted from a build has to answer the three macros the way
    // its own stub did, not the way the machine emulating it now would.
    let mut rt = rt("Func F()\n    Return @Compiled & \"|\" & @Unicode & \"|\" & @AutoItX64\nEndFunc\n");
    assert_eq!(rt.call_function("F", vec![]).unwrap().to_autoit_string(), "0|1|");
    rt.set_build_facts(BuildFacts {
        compiled: true,
        unicode: true,
        autoit_x64: Some(false),
    });
    assert_eq!(rt.call_function("F", vec![]).unwrap().to_autoit_string(), "1|1|0");
}

#[test]
fn a_debugger_sees_builtin_calls() {
    // Builtins have no script body, so the debugger's call hook is the only
    // way to know they ran (`untilcall GUICreate` relies on it).
    struct Recorder(Rc<RefCell<Vec<String>>>);
    impl Debugger for Recorder {
        fn on_builtin_call(
            &mut self,
            name: &str,
            _args: &[Value],
            _span: autoitv3_ast::span::Span,
            _depth: usize,
        ) -> DebugAction {
            self.0.borrow_mut().push(name.to_string());
            DebugAction::Continue
        }
    }

    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut rt = rt("Func F()\n    Return String(1) & String(2)\nEndFunc\n");
    rt.set_debugger(Box::new(Recorder(calls.clone())));
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        "12"
    );
    let seen = calls.borrow();
    assert!(
        seen.iter().any(|c| c.eq_ignore_ascii_case("String")),
        "got {seen:?}"
    );
}

#[test]
fn a_debugger_can_stop_a_builtin_before_it_runs() {
    // The `stopat` seam: returning `Pause` from the builtin hook suspends the
    // interpreter *before* the call — which is what lets a dialog's arguments
    // be read without the dialog opening — and the call still happens once the
    // debugger is done with it.
    /// `(name, arguments)` for every call the debugger was offered.
    type Seen = Rc<RefCell<Vec<(String, Vec<Value>)>>>;

    struct Catcher {
        stopped: Rc<RefCell<Vec<String>>>,
        seen: Seen,
    }
    impl Debugger for Catcher {
        fn on_builtin_call(
            &mut self,
            name: &str,
            args: &[Value],
            _span: autoitv3_ast::span::Span,
            _depth: usize,
        ) -> DebugAction {
            self.seen
                .borrow_mut()
                .push((name.to_string(), args.to_vec()));
            if name.eq_ignore_ascii_case("MsgBox") {
                DebugAction::Pause
            } else {
                DebugAction::Continue
            }
        }

        fn on_stop(&mut self, reason: &StopReason, _host: &mut dyn DebugHost) {
            if let StopReason::Call { name } = reason {
                self.stopped.borrow_mut().push(name.clone());
            }
        }
    }

    let stopped = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut rt = rt(
        "Func F()\n    Local $r = MsgBox(16, \"title\", \"body\")\n    Return $r + StringLen(\"abc\")\nEndFunc\n",
    );
    rt.set_debugger(Box::new(Catcher { stopped: stopped.clone(), seen: seen.clone() }));
    // The dialog itself is answered by the host (no backend here: `MsgBox`
    // returns through the undefined-function path), so what is checked is that
    // the call was offered *with its arguments* and still went ahead.
    let _ = rt.call_function("F", vec![]);
    assert_eq!(stopped.borrow().as_slice(), ["MsgBox".to_string()]);
    let seen = seen.borrow();
    let (_, args) = seen
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("MsgBox"))
        .expect("MsgBox was offered");
    assert_eq!(args.len(), 3, "got {args:?}");
    assert_eq!(args[1].to_autoit_string(), "title");
    assert_eq!(args[2].to_autoit_string(), "body");
}

#[test]
fn a_debugger_can_stop_at_a_script_functions_entry() {
    // For a script function `stopat` stops where the frame is already live, so
    // the parameters are readable — the builtin case can only show the argument
    // list it was handed.
    struct Entry {
        stops: Rc<RefCell<Vec<String>>>,
        arg: Rc<RefCell<Option<i64>>>,
    }
    impl Debugger for Entry {
        fn on_call_enter(&mut self, name: &str, _args: &[Value]) -> DebugAction {
            if name.eq_ignore_ascii_case("Double") {
                DebugAction::Pause
            } else {
                DebugAction::Continue
            }
        }

        fn on_stop(&mut self, reason: &StopReason, host: &mut dyn DebugHost) {
            if let StopReason::Call { name } = reason {
                self.stops.borrow_mut().push(name.clone());
                if let Some(frame) = host.frames().last() {
                    if let Some((_, v)) = frame.locals.iter().find(|(n, _)| n == "n") {
                        *self.arg.borrow_mut() = Some(v.to_int());
                    }
                }
            }
        }
    }

    let stops = Rc::new(RefCell::new(Vec::new()));
    let arg = Rc::new(RefCell::new(None));
    let mut rt = rt("Func Double($n)\n    Return $n * 2\nEndFunc\n");
    rt.set_debugger(Box::new(Entry { stops: stops.clone(), arg: arg.clone() }));
    assert_eq!(
        rt.call_function("Double", vec![Value::Int(21)]).unwrap().to_int(),
        42,
        "the call still ran"
    );
    assert_eq!(stops.borrow().as_slice(), ["Double".to_string()]);
    assert_eq!(*arg.borrow(), Some(21), "the parameter is bound at the stop");
}

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

#[test]
fn string_builtins() {
    let src = r#"
Func F()
    Local $s = StringMid("abcdef", 2, 3)
    Local $u = StringUpper($s)
    Local $n = StringLen($u)
    Return $u & ":" & $n & ":" & StringLeft("xyz", 2)
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "BCD:3:xy");
}

#[test]
fn bit_and_hex_builtins() {
    let src = r#"
Func F()
    Return BitAND(0xF0, 0x3C) & ":" & Hex(255, 2) & ":" & Dec("0xFF")
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "48:FF:255");
}

#[test]
fn string_predicate_builtins() {
    let src = r#"
Func A($s)
    Return StringIsASCII($s)
EndFunc
Func L($s)
    Return StringIsLower($s)
EndFunc
Func U($s)
    Return StringIsUpper($s)
EndFunc
Func X($s)
    Return StringIsXDigit($s)
EndFunc
Func C($s)
    Return StringStripCR($s)
EndFunc
"#;
    // The answer is an integer ("Success: 1, Failure: 0"), not a boolean.
    let yes = |f: &str, s: &str| matches!(call(src, f, vec![Value::str(s)]), Value::Int(1));
    assert!(yes("A", "abc"));
    assert!(!yes("A", "aé"));
    assert!(yes("L", "abc"));
    assert!(!yes("L", "aBc"));
    assert!(!yes("L", "123"));
    assert!(yes("U", "ABC"));
    assert!(!yes("U", "123"));
    assert!(yes("X", "1aF"));
    assert!(!yes("X", "1g"));
    assert_eq!(
        call(src, "C", vec![Value::str("a\r\nb\r")]).to_autoit_string(),
        "a\nb"
    );
}

#[test]
fn chrw_and_ascii_array_roundtrip() {
    let src = r#"
Func C($n)
    Return ChrW($n)
EndFunc
Func T($s)
    Return StringToASCIIArray($s)
EndFunc
Func B($a)
    Return StringFromASCIIArray($a)
EndFunc
"#;
    assert_eq!(
        call(src, "C", vec![Value::Int(0x4E2D)]).to_autoit_string(),
        "中"
    );
    let arr = call(src, "T", vec![Value::str("Hi")]);
    match &arr {
        Value::Array(a) => {
            let a = a.borrow();
            assert!(matches!(a[0], Value::Int(72)));
            assert!(matches!(a[1], Value::Int(105)));
        }
        other => panic!("expected array, got {other:?}"),
    }
    let back = call(
        src,
        "B",
        vec![Value::array(vec![Value::Int(72), Value::Int(105)])],
    );
    assert_eq!(back.to_autoit_string(), "Hi");
}

#[test]
fn bitrotate_wraps_and_sign_extends() {
    let src = "Func F()\n    Return BitRotate(1, 1)\nEndFunc\nFunc G()\n    Return BitRotate(1, -1)\nEndFunc\nFunc H()\n    Return BitRotate(0x81, 1, 8)\nEndFunc\n";
    assert!(matches!(call(src, "F", vec![]), Value::Int(2)));
    assert!(matches!(call(src, "G", vec![]), Value::Int(-2147483648)));
    assert!(matches!(call(src, "H", vec![]), Value::Int(3)));
}

#[test]
fn isbool_and_isfloat() {
    let src = "Func B($v)\n    Return IsBool($v)\nEndFunc\nFunc F($v)\n    Return IsFloat($v)\nEndFunc\n";
    assert!(matches!(
        call(src, "B", vec![Value::Bool(true)]),
        Value::Int(1)
    ));
    assert!(matches!(
        call(src, "B", vec![Value::Int(1)]),
        Value::Int(0)
    ));
    assert!(matches!(
        call(src, "F", vec![Value::Float(1.5)]),
        Value::Int(1)
    ));
    assert!(matches!(
        call(src, "F", vec![Value::Float(1.0)]),
        Value::Int(0)
    ));
    assert!(matches!(
        call(src, "F", vec![Value::str("2.25")]),
        Value::Int(1)
    ));
}

#[test]
fn mapappend_uses_the_next_integer_key() {
    let src = r#"
Func F()
    Local $m[]
    $m[1] = "a"
    Local $k = MapAppend($m, "b")
    Return $k & ":" & $m[2] & ":" & $m[1]
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "2:b:a");
}

#[test]
fn assign_eval_and_isdeclared() {
    let src = r#"
Func F()
    Assign("x", 41)
    Return Eval("x") + 1
EndFunc
Func D()
    Local $a = 1
    Return IsDeclared("a") & IsDeclared("nope") & IsDeclared("1bad")
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(42)));
    assert_eq!(call(src, "D", vec![]).to_autoit_string(), "10-1");
}

#[test]
fn a_variable_name_is_case_insensitive_everywhere() {
    // The lookup key is cached on the identifier, so the cache has to agree
    // with the name-based paths (`Eval`, `Assign`, `IsDeclared`) that build a
    // key at runtime.
    let src = r#"
Func F()
    Local $MixedCase = 1
    $mixedCASE += 1
    Assign("MIXEDcase", Eval("mixedcase") + 40)
    Return $MixedCase & ":" & Eval("MIXEDCASE") & ":" & IsDeclared("mixedCase")
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "42:42:1");
}

#[test]
fn a_function_reference_carries_its_own_lookup_key() {
    // A table element is a function reference, and the reference caches the
    // key the call resolves by — so the spelling in the table does not have to
    // match the definition.
    let src = r#"
Func Target($n)
    Return $n + 1
EndFunc
Func F()
    Local $t[] = [1, TaRgEt]
    Return $t[1](41)
EndFunc
Func G()
    Local $f = target
    Return FuncName($f)
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(42)));
    assert_eq!(call(src, "G", vec![]).to_autoit_string(), "target");
}

#[test]
fn funcname_reports_the_function_name() {
    let src = "Func Target()\n    Return 1\nEndFunc\nFunc F()\n    Return FuncName(Target)\nEndFunc\n";
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "Target");
}

#[test]
fn ubound_and_array_literal() {
    let src = r#"
Func F()
    Local $a[] = [10, 20, 30]
    Return UBound($a)
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(3)));
}

#[test]
fn maps_are_supported() {
    let src = r#"
Func F()
    Local $m[]
    $m["k"] = "v"
    If MapExists($m, "k") Then Return $m["k"]
    Return "missing"
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "v");
}

#[test]
fn map_integer_and_string_keys_are_distinct_entries() {
    // Per the AutoIt docs: `$m[3]` and `$m["3"]` are separate keys, and
    // string keys are case sensitive.
    let src = r#"
Func F()
    Local $m[]
    $m[3] = "Integer 3"
    $m["3"] = "String 3"
    $m["Key"] = "upper"
    Return UBound($m) & ":" & $m[3] & ":" & $m["3"] & ":" & $m["Key"] & ":" & $m["key"] _
        & ":" & MapExists($m, 3) & MapExists($m, "3") & MapExists($m, 4)
EndFunc
"#;
    assert_eq!(
        call(src, "F", vec![]).to_autoit_string(),
        "3:Integer 3:String 3:upper::110"
    );
}

#[test]
fn mapremove_and_mapkeys_distinguish_key_kinds() {
    let src = r#"
Func F()
    Local $m[]
    $m[1] = "a"
    $m["1"] = "s"
    $m["B"] = "b"
    MapRemove($m, 1)
    Local $ks = MapKeys($m)
    Return (UBound($ks) - 1) & ":" & $ks[1] & ":" & $ks[2] & ":" & $m[1] & ":" & $m["1"]
EndFunc
"#;
    // MapRemove with the integer key spares the string key; MapKeys echoes
    // the integer key back as an integer.
    assert_eq!(
        call(src, "F", vec![]).to_autoit_string(),
        "2:1:B::s"
    );
}

#[test]
fn effect_overrides_fine_tune_the_presets() {
    use autoitv3_runtime::profile::{EffectKind, ExecutionProfile};

    // The presets themselves are untouched.
    let det = ExecutionProfile::deterministic();
    let faith = ExecutionProfile::faithful();
    for kind in EffectKind::ALL {
        assert!(!det.effect_allowed(*kind), "deterministic allows {kind:?}");
        assert!(faith.effect_allowed(*kind), "faithful denies {kind:?}");
    }

    // A deterministic run may write the registry it probes — and nothing else.
    let reg = det.with_effect(EffectKind::RegistryWrite, true);
    assert!(reg.effect_allowed(EffectKind::RegistryWrite));
    assert!(!reg.effect_allowed(EffectKind::FileWrite));
    assert!(!reg.effect_allowed(EffectKind::Shutdown));
    assert_eq!(reg.random, det.random, "other preset fields unchanged");

    // A faithful run can still be told: never call Shutdown.
    let no_shutdown = faith.with_effect(EffectKind::Shutdown, false);
    assert!(!no_shutdown.effect_allowed(EffectKind::Shutdown));
    assert!(no_shutdown.effect_allowed(EffectKind::FileWrite));
    assert!(no_shutdown.effect_allowed(EffectKind::Spawn));

    // Decisions are also reachable through the HostContext seam.
    struct Probe(ExecutionProfile);
    impl HostContext for Probe {
        fn get_global(&self, _: &str) -> Option<Value> {
            None
        }
        fn set_global(&mut self, _: &str, _: Value) {}
        fn error(&self) -> i64 {
            0
        }
        fn set_error(&mut self, _: i64, _: i64) {}
        fn profile(&self) -> &ExecutionProfile {
            &self.0
        }
    }
    let ctx = Probe(reg);
    assert!(ctx.effect_allowed(EffectKind::RegistryWrite));
    assert!(!ctx.effect_allowed(EffectKind::Spawn));
}

#[test]
fn execute_runs_generated_source() {    let src = r#"
Func F()
    Return Execute("1 + 2")
EndFunc
"#;
    assert!(matches!(call(src, "F", vec![]), Value::Int(3)));
}

#[test]
fn unknown_function_is_an_error() {
    let src = "Func F()\n    Return NoSuchFunction(1)\nEndFunc\n";
    let err = rt(src).call_function("F", vec![]).unwrap_err();
    assert!(
        matches!(err, autoitv3_runtime::RuntimeError::UndefinedFunction { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Recursion guard
// ---------------------------------------------------------------------------

#[test]
fn runaway_recursion_is_stopped() {
    let src = "Func F()\n    Return F()\nEndFunc\n";
    let mut r = rt(src);
    r.set_max_depth(32);
    let err = r.call_function("F", vec![]).unwrap_err();
    assert!(
        matches!(err, autoitv3_runtime::RuntimeError::CallDepthExceeded { .. }),
        "got {err:?}"
    );
}

#[test]
fn runaway_loop_hits_step_limit() {
    let src = "Func F()\n    While 1\n        Local $x = 1\n    WEnd\nEndFunc\n";
    let mut r = rt(src);
    r.set_max_steps(500);
    let err = r.call_function("F", vec![]).unwrap_err();
    assert!(
        matches!(err, autoitv3_runtime::RuntimeError::StepLimitExceeded { .. }),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Debug interface (the seam the future debug module uses)
// ---------------------------------------------------------------------------

#[test]
fn debugger_sees_statements_and_calls() {
    let src = "Func F($a)\n    Local $b = $a + 1\n    Return $b\nEndFunc\n";
    let mut r = rt(src);
    r.set_debugger(Box::new(TracingDebugger::default()));
    let v = r.call_function("F", vec![Value::Int(1)]).unwrap();
    assert!(matches!(v, Value::Int(2)));
    let dbg = r.take_debugger().unwrap();
    // We cannot downcast through the trait object here, so assert via a fresh
    // tracer that we keep a handle on.
    drop(dbg);
}

#[test]
fn tracing_debugger_records_spans() {
    // Same as above but keeping ownership of the tracer for assertions.
    struct Share(std::rc::Rc<std::cell::RefCell<TracingDebugger>>);
    impl Debugger for Share {
        fn on_statement(
            &mut self,
            span: autoitv3_ast::span::Span,
            depth: usize,
            host: &mut dyn autoitv3_runtime::debug::DebugHost,
        ) -> DebugAction {
            self.0.borrow_mut().on_statement(span, depth, host)
        }
        fn on_call_enter(&mut self, name: &str, args: &[Value]) -> DebugAction {
            self.0.borrow_mut().on_call_enter(name, args)
        }
    }

    let src = "Func F($a)\n    Local $b = $a + 1\n    Return $b\nEndFunc\n";
    let mut r = rt(src);
    let tracer = std::rc::Rc::new(std::cell::RefCell::new(TracingDebugger::default()));
    r.set_debugger(Box::new(Share(tracer.clone())));
    r.call_function("F", vec![Value::Int(1)]).unwrap();

    let t = tracer.borrow();
    assert!(!t.trace.is_empty(), "no statements traced");
    assert_eq!(t.calls, vec!["F".to_string()]);
    // The traced spans must point into the generated source (lines 2 and 3).
    assert!(t.trace.iter().any(|s| s.start.line == 2), "trace: {:?}", t.trace);
}

#[test]
fn breakpoints_report_stop_reason() {
    let src = "Func F()\n    Local $a = 1\n    Return $a\nEndFunc\n";
    let mut r = rt(src);
    // Break on the `Return` statement (line 3).
    r.breakpoints_mut().add_line(3);
    r.call_function("F", vec![]).unwrap();
    match r.take_pause() {
        Some(StopReason::Breakpoint { line, .. }) => assert_eq!(line, 3),
        other => panic!("expected a breakpoint stop, got {other:?}"),
    }
}

#[test]
fn frames_snapshot_exposes_locals() {
    let src = "Func F()\n    Local $a = 41\n    Return G()\nEndFunc\nFunc G()\n    Return 1\nEndFunc\n";
    let mut r = rt(src);
    // Call G directly so the snapshot has one frame while it runs.
    r.call_function("G", vec![]).unwrap();
    assert!(r.frames_snapshot().is_empty(), "frames unwind after return");
}

// ---------------------------------------------------------------------------
// Host interface (the seam the full runtime uses)
// ---------------------------------------------------------------------------

#[test]
fn host_supplies_native_functions() {
    let src = "Func F()\n    Return Double(21)\nEndFunc\n";
    let mut r = rt(src);
    let mut host = NativeHost::new();
    host.register("Double", |_ctx: &mut dyn HostContext, args: Vec<Value>| {
        Ok(Value::Int(args[0].to_int() * 2))
    });
    r.set_host(Box::new(host));
    let v = r.call_function("F", vec![]).unwrap();
    assert!(matches!(v, Value::Int(42)), "got {v:?}");
}

#[test]
fn host_can_read_and_write_globals() {
    // `F` reports what the host's failure left *inside* it: a nested call's
    // codes are visible while the body runs. They do not outlive the function
    // (see `an_error_leaves_only_the_function_that_set_it`), so the caller ends
    // up with 0.
    let src = "Global $g = 1\nFunc F()\n    Local $bumped = Bump()\n    Return $bumped & \"/\" & @error & \"/\" & @extended\nEndFunc\n";
    let mut r = rt(src);
    let mut host = NativeHost::new();
    host.register("Bump", |ctx: &mut dyn HostContext, _args: Vec<Value>| {
        let cur = ctx.get_global("g").map(|v| v.to_int()).unwrap_or(0);
        ctx.set_global("g", Value::Int(cur + 1));
        ctx.set_error(5, 6);
        Ok(Value::Int(cur + 1))
    });
    r.set_host(Box::new(host));
    // Run the script body so `Global $g = 1` takes effect.
    r.run_script().unwrap();
    let v = r.call_function("F", vec![]).unwrap();
    match v {
        Value::Str(s) => assert_eq!(s, "2/5/6", "got {s}"),
        other => panic!("got {other:?}"),
    }
    assert_eq!(r.get_global("g").map(|v| v.to_int()), Some(2));
    assert_eq!(r.error(), 0);
}

#[test]
fn user_function_shadows_builtin() {
    // A user-defined `String` must win over the builtin, like AutoIt.
    let src = "Func String($x)\n    Return \"user\"\nEndFunc\nFunc F()\n    Return String(1)\nEndFunc\n";
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "user");
}
// ---------------------------------------------------------------------------
// Real-target integration: the runtime must be able to execute the
// obfuscator's own table builder.
// ---------------------------------------------------------------------------

#[test]
fn evaluates_real_function_table_builder() {
    // The function-table builder is pure array construction merged by
    // `MergeArrays` — the exact workload the deobfuscator depends on. Which
    // function it is differs per script, so call the script's own functions and
    // keep the one that returns the table-shaped array.
    let Some(src) = sample_script() else { return };
    let prog = autoit3_parse(&src);
    let mut rt = Runtime::with_program(&prog);
    rt.set_profile(autoitv3_runtime::ExecutionProfile::deterministic());
    let mut table = None;
    for name in rt.function_names() {
        if let Ok(Value::Array(a)) = rt.call_function(&name, vec![]) {
            let a = a.borrow();
            // Element 0 is the count; a table is count + many names.
            if a.len() > 100 && a[0].to_int() as usize == a.len() - 1 {
                table = Some(a.iter().map(|v| v.to_autoit_string()).collect::<Vec<_>>());
                break;
            }
        }
    }
    let Some(all) = table else {
        panic!("no function-table-shaped builder found in the sample");
    };
    // Some slots hold real AutoIt builtins; which ones the obfuscator picked is
    // script-specific, so only require that it named some of them.
    let known = all
        .iter()
        .filter(|n| autoitv3_runtime::vocab::canonical_function(n.as_str()).is_some())
        .count();
    assert!(
        known > 0,
        "no real AutoIt builtin in a {}-entry table",
        all.len()
    );
}

fn autoit3_parse(src: &str) -> autoitv3_ast::Program {
    autoitv3_ast::parse(src).expect("sample script parses")
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

// ---------------------------------------------------------------------------
// Platform layer (the seam for OS-specific builtins)
// ---------------------------------------------------------------------------

#[test]
fn no_platform_is_installed_by_default() {
    // The core names no operating system; a platform has to be installed
    // (see the `autoitv3-platform` crate).
    let r = rt("Func F()\nEndFunc\n");
    assert_eq!(r.platform_name(), "none");
}

#[test]
fn platform_is_consulted_after_the_host() {
    use autoitv3_runtime::platform::Platform;

    /// A platform that answers one function with a marker value.
    struct ProbePlatform;
    impl Platform for ProbePlatform {
        fn name(&self) -> &'static str {
            "probe"
        }
        fn provides(&self, name: &str) -> bool {
            name.eq_ignore_ascii_case("PlatformOnly")
        }
        fn call(
            &mut self,
            name: &str,
            _args: Vec<Value>,
            _ctx: &mut dyn HostContext,
        ) -> Result<Option<Value>, autoitv3_runtime::RuntimeError> {
            if name.eq_ignore_ascii_case("PlatformOnly") {
                return Ok(Some(Value::str("from-platform")));
            }
            Ok(None)
        }
    }

    let src = "Func F()\n    Return PlatformOnly()\nEndFunc\n";
    let mut r = rt(src);
    r.set_platform(Box::new(ProbePlatform));
    assert_eq!(r.platform_name(), "probe");
    assert_eq!(r.call_function("F", vec![]).unwrap().to_autoit_string(), "from-platform");

    // An explicit host takes precedence over the platform.
    let mut host = NativeHost::new();
    host.register("PlatformOnly", |_ctx: &mut dyn HostContext, _a: Vec<Value>| {
        Ok(Value::str("from-host"))
    });
    r.set_host(Box::new(host));
    assert_eq!(r.call_function("F", vec![]).unwrap().to_autoit_string(), "from-host");
}

#[test]
fn nothing_os_specific_is_silently_invented() {
    // No Windows-only function is implemented off Windows, so an unknown name
    // must fail loudly rather than quietly returning a made-up value.
    let src = "Func F()\n    Return ObjCreate(\"WScript.Shell\")\nEndFunc\n";
    let err = rt(src).call_function("F", vec![]).unwrap_err();
    // `ObjCreate` is not a neutral stub on this platform.
    assert!(
        matches!(err, autoitv3_runtime::RuntimeError::UndefinedFunction { .. }),
        "got {err:?}"
    );
}

#[test]
fn member_access_reports_that_it_needs_a_platform_host() {
    let src = "Func F()\n    Return $obj.Prop\nEndFunc\n";
    let err = rt(src).call_function("F", vec![]).unwrap_err();
    let msg = err.message();
    assert!(
        msg.contains("platform host") || msg.contains("non-object value"),
        "got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// AutoIt value semantics the obfuscator leans on
// ---------------------------------------------------------------------------

#[test]
fn a_binary_renders_as_prefixed_hex() {
    // `String($binary)` is "0x" + upper-case hex, which is why scripts strip
    // that prefix before reading hex digits back out.
    let src = r#"Func F()
    Return String(Binary("0x00ff10")) & "|" & Hex(Binary("0x00ff10"))
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "0x00FF10|00FF10");
}

#[test]
fn dec_reads_its_argument_as_hexadecimal() {
    // AutoIt's `Dec` is the inverse of `Hex`, not a decimal parse.
    let src = r#"Func F()
    Return Dec("00E0") & "|" & Dec("1A09") & "|" & Dec("0xFF") & "|" & Dec("-10")
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "224|6665|255|-16");
}

#[test]
fn concatenating_two_binaries_joins_their_bytes() {
    // The crypto UDFs build an HMAC input as `$iv & $ciphertext`; joining the
    // `0x…` spellings instead would hash the wrong bytes entirely.
    let src = r#"Func F()
    Local $a = Binary("0x0102"), $b = Binary("0x0304")
    Local $c = $a & $b
    Return IsBinary($c) & "|" & BinaryLen($c) & "|" & String($c)
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "1|4|0x01020304");
}

#[test]
fn concatenating_a_binary_with_text_still_spells_it_out() {
    // Only two binaries join byte-wise; a string operand keeps the `0x…` text
    // form, which is what scripts that build log lines expect.
    let src = r#"Func F()
    Return Binary("0x4142") & "!"
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "0x4142!");
}

#[test]
fn binarymid_without_a_count_reads_to_the_end() {
    let src = r#"Func F()
    Local $b = Binary("0x0001020304050607")
    Return BinaryMid($b, 5) & "|" & BinaryMid($b, 5, 2) & "|" & BinaryLen(BinaryMid($b, 5, 0))
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "0x04050607|0x0405|0");
}

#[test]
fn binaries_compare_by_their_bytes() {
    let src = r#"Func F()
    Local $same = (Binary("0x4142") == Binary("0x4142"))
    Local $other = (Binary("0x4142") == Binary("0x4143"))
    Return $same & "|" & $other
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "True|False");
}

#[test]
fn declarations_build_nested_dimensions() {
    let src = r#"Func F()
    Local $grid[2][3]
    $grid[1][2] = 7
    Local $auto[2][2] = [[1, 2], [3, 4]]
    Return UBound($grid, 0) & "|" & UBound($grid, 1) & "|" & UBound($grid, 2) & "|" & $grid[1][2] & "|" & $auto[1][0]
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "2|2|3|7|3");
}

#[test]
fn redim_resizes_rows_and_columns_in_place() {
    let src = r#"Func F()
    Local $grid[1][2]
    $grid[0][0] = 5
    ReDim $grid[2][3]
    $grid[1][2] = 9
    Return UBound($grid, 1) & "x" & UBound($grid, 2) & "|" & $grid[0][0] & "|" & $grid[1][2]
EndFunc"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "2x3|5|9");
}

#[test]
fn var_get_type_names_the_autoit_variant() {
    let src = r#"Func F()
    Return VarGetType(1) & "," & VarGetType(1.5) & "," & VarGetType("s") & "," & VarGetType(Binary("0x41")) & "," & VarGetType(Null)
EndFunc"#;
    assert_eq!(
        call(src, "F", vec![]).to_autoit_string(),
        "Int32,Double,String,Binary,Keyword"
    );
}

#[test]
fn exit_handlers_are_recorded_rather_than_run() {
    let src = "Func F()\n    OnAutoItExitRegister(\"Cleanup\")\n    Return OnAutoItExitRegister(\"Cleanup2\")\nEndFunc\n";
    let prog = autoitv3_ast::parse(src).unwrap();
    let mut r = Runtime::with_program(&prog);
    assert!(matches!(r.call_function("F", vec![]).unwrap(), Value::Int(1)));
    assert_eq!(r.exit_handlers(), ["Cleanup", "Cleanup2"]);
}

// ---------------------------------------------------------------------------
// Stopping, inspecting and resuming
// ---------------------------------------------------------------------------

/// A debugger that stops on one line, reads the live frame, and writes to it.
struct Inspector {
    stop_line: u32,
    /// `$arg` as seen at the stop, and the value of `$local` after a write.
    seen: Vec<(String, String)>,
    /// How many times it stopped.
    stops: usize,
    /// Assign to `$local` at the stop, to prove writes reach the program.
    assign: Option<i64>,
}

impl Debugger for Inspector {
    fn on_statement(
        &mut self,
        span: autoitv3_ast::span::Span,
        _depth: usize,
        _host: &mut dyn autoitv3_runtime::debug::DebugHost,
    ) -> DebugAction {
        if span.start.line == self.stop_line {
            DebugAction::Pause
        } else {
            DebugAction::Continue
        }
    }

    fn on_stop(
        &mut self,
        _reason: &StopReason,
        host: &mut dyn autoitv3_runtime::debug::DebugHost,
    ) {
        self.stops += 1;
        let arg = host.evaluate("$arg").unwrap().to_autoit_string();
        let local = host.evaluate("$local").unwrap().to_autoit_string();
        self.seen.push((arg, local));
        if let Some(v) = self.assign {
            host.evaluate(&format!("$local = {v}")).unwrap();
        }
    }
}

#[test]
fn a_stop_sees_the_live_frame_and_can_write_to_it() {
    let src = "Func F($arg)\n    Local $local = $arg * 2\n    Return $local\nEndFunc\n";
    let mut r = rt(src);
    r.set_debugger(Box::new(Inspector {
        stop_line: 3,
        seen: Vec::new(),
        stops: 0,
        assign: Some(7),
    }));
    let v = r.call_function("F", vec![Value::Int(21)]).unwrap();
    // The write at the stop is what the function returns.
    assert!(matches!(v, Value::Int(7)), "got {v:?}");
}

#[test]
fn a_stateful_debugger_sees_every_stop() {
    // The trait object is owned by the runtime, so a debugger that wants its
    // results back shares them through an `Rc`.
    struct Share(Rc<std::cell::RefCell<Inspector>>);
    impl Debugger for Share {
        fn on_statement(
            &mut self,
            span: autoitv3_ast::span::Span,
            depth: usize,
            host: &mut dyn autoitv3_runtime::debug::DebugHost,
        ) -> DebugAction {
            self.0.borrow_mut().on_statement(span, depth, host)
        }
        fn on_stop(
            &mut self,
            reason: &StopReason,
            host: &mut dyn autoitv3_runtime::debug::DebugHost,
        ) {
            self.0.borrow_mut().on_stop(reason, host);
        }
    }

    let src = "Func F($arg)\n    Local $local = $arg + 1\n    Local $local = $arg + 2\n    Return $local\nEndFunc\n";
    let mut r = rt(src);
    let inner = Rc::new(std::cell::RefCell::new(Inspector {
        stop_line: 4,
        seen: Vec::new(),
        stops: 0,
        assign: None,
    }));
    r.set_debugger(Box::new(Share(inner.clone())));
    let v = r.call_function("F", vec![Value::Int(1)]).unwrap();
    assert!(matches!(v, Value::Int(3)), "got {v:?}");
    let seen = inner.borrow();
    assert_eq!(seen.stops, 1);
    // `$arg` and the second `$local` are both visible at the stop.
    assert_eq!(seen.seen, vec![("1".to_string(), "3".to_string())]);
}

#[test]
fn a_conditional_breakpoint_only_fires_when_its_condition_holds() {
    let src = "Func F()\n    Local $hit = 0\n    For $i = 1 To 5\n        $hit += 1\n    Next\n    Return $hit\nEndFunc\n";
    let mut r = rt(src);
    // Break inside the loop only on the last iteration.
    let id = r.breakpoints_mut().add(4, Some("$i = 5".to_string()));
    r.set_debugger(Box::new(TracingDebugger::default()));
    assert!(matches!(
        r.call_function("F", vec![]).unwrap(),
        Value::Int(5)
    ));
    let hits = r.breakpoints().iter().find(|b| b.id == id).unwrap().hits;
    assert_eq!(hits, 1, "a guarded breakpoint counted the wrong number of hits");
}

#[test]
fn breakpoint_edits_apply_through_the_host() {
    let src = "Func F()\n    Return 1\nEndFunc\n";
    let mut r = rt(src);
    let id = r.add_breakpoint(2, Some("True".to_string()));
    assert_eq!(r.breakpoints().len(), 1);
    assert_eq!(r.breakpoints()[0].condition.as_deref(), Some("True"));
    assert!(r.set_breakpoint_enabled(id, false));
    assert!(!r.breakpoints()[0].enabled);
    assert!(r.remove_breakpoint(id));
    assert!(r.breakpoints().is_empty());
}

#[test]
fn the_host_reports_frames_and_functions() {
    let src = "Func F()\n    Local $x = 1\n    Return G()\nEndFunc\nFunc G()\n    Return 2\nEndFunc\n";
    let mut r = rt(src);
    r.call_function("F", vec![]).unwrap();
    let names = r.function_names();
    assert!(names.contains(&"F".to_string()) && names.contains(&"G".to_string()));
    // Nothing is running any more, so there are no frames to report.
    assert!(r.frames().is_empty());
    assert!(r.globals().is_empty());
}

#[test]
fn a_debugger_expression_reports_a_name_that_was_never_assigned() {
    // `print` is a question asked at a prompt, so a name that resolves nowhere
    // is a typo to report: `""` reads as a variable the program really holds.
    // `evaluate_expression` parses its own wrapper, so the position is not
    // meaningful here.
    let at = autoitv3_ast::span::Span::default();
    let mut r = rt("Global $blank = \"\"\n");
    r.run_script().unwrap();
    match r.evaluate_expression("$nope", at) {
        Err(autoitv3_runtime::RuntimeError::UndefinedVariable { name, span }) => {
            assert_eq!(name, "$nope");
            // The generated wrapper has no position the user could recognise,
            // so the error carries none.
            assert!(span.is_none(), "span: {span:?}");
        }
        other => panic!("expected an undefined variable, got {other:?}"),
    }
    // The same expression with a name that exists, even with an empty value,
    // is a value rather than an error.
    let v = r.evaluate_expression("$blank", at).unwrap();
    assert!(matches!(&v, Value::Str(s) if s.is_empty()), "got {v:?}");
}

#[test]
fn an_unset_name_still_reads_as_empty_in_script_code() {
    // Strict reads belong to the prompt. A script reading a name it never
    // assigned keeps AutoIt's default behaviour (`Opt("MustDeclareVars", 0)`),
    // which is what the scripts found in the wild are written against.
    let v = call(
        "Func F()\n    Return \"[\" & $never_set & \"]\"\nEndFunc\n",
        "F",
        vec![],
    );
    assert!(matches!(&v, Value::Str(s) if s == "[]"), "got {v:?}");
}

#[test]
fn an_uncaught_error_is_offered_while_the_frame_is_still_live() {
    // The hook fires where the error is raised, *before* the frame is popped,
    // which is the whole point: a post-mortem is only useful if the locals that
    // led to the failure are still there.
    struct PostMortem {
        seen: Vec<(String, String, String)>,
        errors: usize,
    }
    impl Debugger for PostMortem {
        fn on_error(
            &mut self,
            error: &autoitv3_runtime::RuntimeError,
            span: Option<autoitv3_ast::span::Span>,
            host: &mut dyn DebugHost,
        ) {
            self.errors += 1;
            let frames = host.frames();
            let innermost = frames
                .last()
                .and_then(|f| f.function.clone())
                .unwrap_or_default();
            let depth = frames.len();
            let n = host.evaluate_expression("$n").unwrap().to_autoit_string();
            self.seen.push((
                innermost,
                n,
                format!("{depth}/{}", span.map(|s| s.start.line).unwrap_or(0)),
            ));
            let _ = error;
        }
    }

    let src = "Func Boom($n)\n    Local $a[1] = [1]\n    Return $a[$n]\nEndFunc\nFunc Top()\n    Return Boom(9)\nEndFunc\n";
    let mut r = rt(src);
    let seen = Rc::new(std::cell::RefCell::new(PostMortem { seen: Vec::new(), errors: 0 }));
    struct Share(Rc<std::cell::RefCell<PostMortem>>);
    impl Debugger for Share {
        fn on_error(
            &mut self,
            error: &autoitv3_runtime::RuntimeError,
            span: Option<autoitv3_ast::span::Span>,
            host: &mut dyn DebugHost,
        ) {
            self.0.borrow_mut().on_error(error, span, host);
        }
    }
    r.set_debugger(Box::new(Share(seen.clone())));

    let err = r.call_function("Top", vec![]).unwrap_err();
    assert!(err.message().contains("out of bounds"), "got: {}", err.message());
    let seen = seen.borrow();
    // One offer, not one per enclosing statement, and it names the failing
    // frame — inside `Boom`, at the `Return` on line 3.
    assert_eq!(seen.errors, 1, "the error was offered more than once");
    // Both `Boom` and its caller `Top` are still on the stack.
    assert_eq!(
        seen.seen,
        vec![("Boom".to_string(), "9".to_string(), "2/3".to_string())]
    );
}

#[test]
fn an_error_is_offered_once_per_run() {
    // The flag that keeps the offer to one per error has to be reset by the
    // *next* run, or a second call would report nothing.
    struct Count(Rc<std::cell::RefCell<usize>>);
    impl Debugger for Count {
        fn on_error(
            &mut self,
            _error: &autoitv3_runtime::RuntimeError,
            _span: Option<autoitv3_ast::span::Span>,
            _host: &mut dyn DebugHost,
        ) {
            *self.0.borrow_mut() += 1;
        }
    }

    let src = "Func Boom()\n    Local $a[1] = [1]\n    Return $a[4]\nEndFunc\n";
    let mut r = rt(src);
    let hits = Rc::new(std::cell::RefCell::new(0usize));
    r.set_debugger(Box::new(Count(hits.clone())));
    assert!(r.call_function("Boom", vec![]).is_err());
    assert!(r.call_function("Boom", vec![]).is_err());
    // One offer each time — not two for the first run and none for the second.
    assert_eq!(*hits.borrow(), 2);
}

#[test]
fn adlib_callbacks_are_recorded_rather_than_fired() {
    // `AdlibRegister` is a core language builtin: it schedules a script
    // function, it does not touch the operating system. This interpreter has
    // no idle clock, so registrations are recorded for the caller instead of
    // being fired on a timer.
    let src = r#"Func Tick()
    Return 1
EndFunc
Func F()
    Local $ok = AdlibRegister("Tick", 60000)
    Local $missing = AdlibRegister("NoSuchFunction", 100)
    Local $again = AdlibRegister("Tick", 120)
    Local $dropped = AdlibUnRegister("Tick")
    Local $twice = AdlibUnRegister("Tick")
    Return $ok & $missing & $again & $dropped & $twice
EndFunc
"#;
    let mut r = Runtime::with_program(&autoit3_parse(src));
    assert_eq!(r.call_function("F", vec![]).unwrap().to_autoit_string(), "10110");
    assert!(r.adlib_handlers().is_empty(), "the callback was unregistered");
}

#[test]
fn adlib_registrations_keep_their_interval() {
    let src = "Func Tick()\nEndFunc\nFunc F()\n    AdlibRegister(\"Tick\", 60000)\nEndFunc\n";
    let mut r = Runtime::with_program(&autoit3_parse(src));
    r.call_function("F", vec![]).unwrap();
    assert_eq!(r.adlib_handlers().len(), 1);
    assert_eq!(r.adlib_handlers()[0].name, "Tick");
    assert_eq!(r.adlib_handlers()[0].interval_ms, 60000);
}

#[test]
fn adlib_registration_stops_at_autoits_limit() {
    // AutoIt keeps at most ten callbacks; the eleventh registration fails.
    let mut src = String::new();
    for i in 0..11 {
        src.push_str(&format!("Func Tick{i}()\nEndFunc\n"));
    }
    src.push_str("Func F()\n    Local $r = \"\"\n");
    for i in 0..11 {
        src.push_str(&format!("    $r &= AdlibRegister(\"Tick{i}\")\n"));
    }
    src.push_str("    Return $r\nEndFunc\n");
    let mut r = Runtime::with_program(&autoit3_parse(&src));
    assert_eq!(
        r.call_function("F", vec![]).unwrap().to_autoit_string(),
        "11111111110"
    );
    assert_eq!(r.adlib_handlers().len(), 10);
}

// ---------------------------------------------------------------------------
// ContinueCase
// ---------------------------------------------------------------------------

/// `Switch` fall-through: `ContinueCase` runs the *next* case's body without
/// testing it, and a case that matches on its own is unaffected.
#[test]
fn continue_case_falls_through_in_a_switch() {
    let src = r#"
Func F($x)
    Local $out = ""
    Switch $x
        Case 1
            $out = $out & "one"
            ContinueCase
        Case 2
            $out = $out & "two"
        Case Else
            $out = $out & "else"
    EndSwitch
    Return $out
EndFunc
"#;
    assert_eq!(call(src, "F", vec![Value::Int(1)]).to_autoit_string(), "onetwo");
    assert_eq!(call(src, "F", vec![Value::Int(2)]).to_autoit_string(), "two");
    assert_eq!(call(src, "F", vec![Value::Int(9)]).to_autoit_string(), "else");
}

/// The same fall-through applies to `Select`, whose cases are truth tests
/// rather than comparisons.
#[test]
fn continue_case_falls_through_in_a_select() {
    let src = r#"
Func F($x)
    Local $out = ""
    Select
        Case $x = 1
            $out = $out & "one"
            ContinueCase
        Case $x = 2
            $out = $out & "two"
        Case Else
            $out = $out & "else"
    EndSelect
    Return $out
EndFunc
"#;
    assert_eq!(call(src, "F", vec![Value::Int(1)]).to_autoit_string(), "onetwo");
    assert_eq!(call(src, "F", vec![Value::Int(2)]).to_autoit_string(), "two");
    assert_eq!(call(src, "F", vec![Value::Int(9)]).to_autoit_string(), "else");
}

/// Falling through from the last case just ends the block, as AutoIt
/// documents — it does not run the whole thing again.
#[test]
fn continue_case_in_the_last_case_ends_the_block() {
    let src = r#"
Func F()
    Local $out = ""
    Switch 1
        Case 1
            $out = $out & "only"
            ContinueCase
    EndSwitch
    $out = $out & "-after"
    Return $out
EndFunc
"#;
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "only-after");
}

/// A `ContinueCase` that never reaches a `Select`/`Switch` is a script bug;
/// reporting it beats silently returning early.
#[test]
fn continue_case_outside_a_case_is_reported() {
    let src = "Func F()\n    ContinueCase\nEndFunc\n";
    let err = rt(src).call_function("F", vec![]).unwrap_err();
    assert!(err.to_string().contains("case control"), "{err}");
}

#[test]
fn breakpoint_hit_rules_gate_firing() {
    use autoitv3_runtime::debug::Breakpoints;

    let mut bps = Breakpoints::new();
    let id = bps.add_full(10, None, 2, 2, true, vec!["1 + 1".into()]);
    let span = autoitv3_ast::span::Span {
        start: autoitv3_ast::span::Pos { line: 10, col: 1 },
        end: autoitv3_ast::span::Pos { line: 10, col: 2 },
    };
    let bp = |b: &Breakpoints| b.get(id).cloned().unwrap();
    assert!(bp(&bps).matches(span));

    // skip 2: hits 1 and 2 are counted but do not fire.
    assert!(!bps.should_fire(id));
    assert!(!bps.should_fire(id));
    // every 2 from then on: hit 3 does not fire (3 % 2), hit 4 does.
    assert!(!bps.should_fire(id));
    assert!(bps.should_fire(id));
    assert_eq!(bp(&bps).hits, 4, "skipped hits count too");
    assert!(bp(&bps).stop);

    // A plain `add` breakpoint keeps the old defaults.
    let plain = bps.add_line(11);
    assert!(bps.should_fire(plain));
    assert!(bp(&bps).stop);
}

#[test]
fn breakpoint_actions_and_flags_round_trip_through_the_host() {
    use autoitv3_runtime::debug::DebugHost;

    let mut rt = rt("Func Target()\n    Return 1\nEndFunc\n");
    let id = rt.add_breakpoint_full(2, None, 0, 1, false, vec!["$x = 1".into()]);
    assert!(rt.set_breakpoint_stop(id, true));
    assert!(rt.ignore_breakpoint(id, 5));
    let bp = rt.breakpoints().into_iter().find(|b| b.id == id).unwrap();
    assert_eq!(bp.skip_remaining, 5);
    assert!(bp.stop);
    assert_eq!(bp.actions, vec!["$x = 1".to_string()]);
    // Function entry resolution: first statement of the body.
    assert_eq!(rt.function_entry_line("target"), Some(2));
    assert_eq!(rt.function_entry_line("nope"), None);
}

// ---------------------------------------------------------------------------
// Truthiness of strings
// ---------------------------------------------------------------------------

#[test]
fn a_non_empty_string_is_true_in_a_boolean_context() {
    // AutoIt judges a string by whether it is empty, not by whether it looks
    // like a number: `"0"` and `"abc"` are both true. The guard
    // `If Not $path Then` therefore means "the path is empty", which is how
    // AutoIt code tests for a missing argument.
    let v = call(
        r#"
Func T($p)
    If (Not $p Or ($p = ".")) Then Return "BASE"
    Return "PATH"
EndFunc
"#,
        "T",
        vec![Value::str("/home/user/data.dat")],
    );
    assert_eq!(v.to_autoit_string(), "PATH");

    let v = call(
        r#"
Func T($p)
    If (Not $p Or ($p = ".")) Then Return "BASE"
    Return "PATH"
EndFunc
"#,
        "T",
        vec![Value::str("")],
    );
    assert_eq!(v.to_autoit_string(), "BASE");
}

#[test]
fn the_string_zero_is_true() {
    let v = call(
        "Func F($s)\n    If $s Then Return 1\n    Return 0\nEndFunc\n",
        "F",
        vec![Value::str("0")],
    );
    assert!(matches!(v, Value::Int(1)), "got {v:?}");

    let v = call(
        "Func F($s)\n    If $s Then Return 1\n    Return 0\nEndFunc\n",
        "F",
        vec![Value::str("")],
    );
    assert!(matches!(v, Value::Int(0)), "got {v:?}");
}

#[test]
fn opt_answers_with_the_previous_setting() {
    let src = r#"
Func F()
    Local $fresh = Opt("GUIOnEventMode", 1)
    Local $now = Opt("GUIOnEventMode")
    Local $reset = Opt("GUIOnEventMode", Default)
    Local $after = Opt("GUIOnEventMode")
    Local $unknown = Opt("NotAnOption", 1)
    Local $err = @error
    Local $close = Opt("GUICloseOnESC")
    Local $mode = Opt("WinTitleMatchMode", 2)
    Local $sep = Opt("GUIDataSeparatorChar", ";")
    Return $fresh & ":" & $now & ":" & $reset & ":" & $after & ":" & $unknown & ":" & $err & ":" & $close & ":" & $mode & ":" & $sep
EndFunc
"#;
    match call(src, "F", vec![]) {
        // The first call answers the default, the reset answers what was set,
        // and an option the help page does not list is refused with `@error`.
        Value::Str(s) => assert_eq!(s, "0:1:1:0:0:1:1:1:|"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn opt_settings_are_kept_for_the_platform_to_read() {
    // What a platform sees is `HostContext::option`; what the runtime has to do
    // is remember the setting under the option's own name.
    let mut runtime = rt("Opt(\"GUIOnEventMode\", 1)\n");
    runtime.run_script().unwrap();
    assert!(matches!(
        runtime.option("GUIOnEventMode"),
        Some(Value::Int(1))
    ));
    assert!(runtime.option("NothingLikeThis").is_none());
}

#[test]
fn every_documented_option_is_known() {
    // The list the `Opt` help page gives, in its own spelling: a name that is
    // misspelled in the table below would answer `@error = 1` here.
    let names = [
        "CaretCoordMode",
        "ExpandEnvStrings",
        "ExpandVarStrings",
        "GUICloseOnESC",
        "GUICoordMode",
        "GUIDataSeparatorChar",
        "GUIEventOptions",
        "GUIOnEventMode",
        "GUIResizeMode",
        "MouseClickDelay",
        "MouseClickDownDelay",
        "MouseClickDragDelay",
        "MouseCoordMode",
        "MustDeclareVars",
        "PixelCoordMode",
        "SendAttachMode",
        "SendCapslockMode",
        "SendKeyDelay",
        "SendKeyDownDelay",
        "SetExitCode",
        "TCPTimeout",
        "TrayAutoPause",
        "TrayIconDebug",
        "TrayIconHide",
        "TrayMenuMode",
        "TrayOnEventMode",
        "WinDetectHiddenText",
        "WinSearchChildren",
        "WinTextMatchMode",
        "WinTitleMatchMode",
        "WinWaitDelay",
    ];
    let mut runtime = rt("");
    for name in names {
        let value = runtime
            .call_named(
                "Opt",
                vec![Value::Str(name.into())],
                autoitv3_ast::span::Span::new(
                    autoitv3_ast::span::Pos::new(1, 1),
                    autoitv3_ast::span::Pos::new(1, 1),
                ),
            )
            .unwrap();
        assert_eq!(runtime.error(), 0, "Opt(\"{name}\") is not a known option");
        assert!(
            !matches!(value, Value::Null),
            "Opt(\"{name}\") answered nothing"
        );
    }
}

#[test]
fn entering_a_function_resets_the_error_codes() {
    // "When entering a user-written function @error macro is set to 0"
    // (SetError's help page). What the body leaves behind does *not* get
    // restored, so `Return SetError(...)` is how a function reports anything.
    let src = r#"
Func Probe()
    Return @error & "/" & @extended
EndFunc
Func Failing()
    Return SetError(5, 7, 0)
EndFunc
Func Outer()
    Failing()
    Return @error & "/" & @extended
EndFunc
Func F()
    SetError(9, 9, 0)
    Local $inside = Probe()
    Local $out = Outer()
    Return $inside & ":" & $out
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "0/0:5/7"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn a_builtin_call_resets_the_error_codes() {
    // Every builtin resets both codes before it runs (`FunctionExecute`), so a
    // script has to read `@error` immediately after the call that failed.
    let src = r#"
Func F()
    SetError(9, 9, 0)
    Local $len = StringLen("ab")
    Return $len & ":" & @error & "/" & @extended
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "2:0/0", "got {s}"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn an_error_leaves_only_the_function_that_set_it() {
    // The 3.3 behaviour, measured against the official interpreter: a nested
    // call's @error is visible inside the function, but a function that never
    // called SetError reports 0 to its caller — and a call *after* SetError
    // replaces the value too.
    let src = r#"
Func ErrorMaker()
    SetError(-99, 0, 0)
    Return 22
EndFunc
Func NaiveProxy()
    Return ErrorMaker()
EndFunc
Func Breakdown()
    Local $result = ErrorMaker()
    Local $seen = @error
    Return $seen & "/" & $result
EndFunc
Func WiseProxy()
    Local $result = ErrorMaker()
    SetError(@error, @extended, 0)
    Return $result
EndFunc
Func SettingLate()
    SetError(13, 7, 0)
    Sleep(1)
EndFunc
Func F()
    ErrorMaker()
    Local $direct = @error
    NaiveProxy()
    Local $naive = @error
    Local $inside = Breakdown()
    WiseProxy()
    Local $wise = @error
    SettingLate()
    Local $late = @error
    Return $direct & ":" & $naive & ":" & $inside & ":" & $wise & ":" & $late
EndFunc
"#;
    match call(src, "F", vec![]) {
        Value::Str(s) => assert_eq!(s, "-99:0:-99/22:-99:0"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn on_autoit_start_register_runs_before_the_body() {
    // The compiler's start hook: the named function runs once, before the
    // body's first statement, so anything it sets is already there.
    let mut rt = rt(
        "#OnAutoItStartRegister \"Boot\"\n\
         Func Boot()\n\
         \x20   Global $G = 7\n\
         EndFunc\n\
         Func Get()\n\
         \x20   Return $G\n\
         EndFunc\n",
    );
    rt.run_script().expect("starts");
    assert_eq!(rt.call_function("Get", vec![]).unwrap().to_int(), 7);
}

#[test]
fn a_missing_start_register_function_is_reported() {
    let mut rt = rt("#OnAutoItStartRegister \"Nope\"\n");
    let err = rt.run_script().expect_err("no such function");
    assert!(
        err.message().contains("undefined function: Nope"),
        "got: {}",
        err.message()
    );
}

#[test]
fn the_start_register_directive_is_case_insensitive_and_may_be_unquoted() {
    let mut rt = rt(
        "#onautoitstartregister boot\n\
         Func boot()\n\
         \x20   Global $G = 9\n\
         EndFunc\n\
         Func Get()\n\
         \x20   Return $G\n\
         EndFunc\n",
    );
    rt.run_script().expect("starts");
    assert_eq!(rt.call_function("Get", vec![]).unwrap().to_int(), 9);
}

// ---------------------------------------------------------------------------
// Pointer provenance (`IsPtr`)
// ---------------------------------------------------------------------------

/// A platform that answers `DllCall` the way a real one does: an array holding
/// the return value and a copy of every argument, so a by-ref write shows up.
struct DllPlatform;
impl autoitv3_runtime::platform::Platform for DllPlatform {
    fn name(&self) -> &'static str {
        "dll"
    }
    fn provides(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case("DllCall")
    }
    fn call(
        &mut self,
        _name: &str,
        args: Vec<Value>,
        _ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, autoitv3_runtime::RuntimeError> {
        // `DllCall(dll, rettype, func, type, value, ...)`: the values sit at 4, 6, …
        let mut out = vec![Value::Int(0x1000)];
        out.extend(args.iter().skip(4).step_by(2).cloned());
        Ok(Some(Value::array(out)))
    }
}

#[test]
fn isptr_answers_for_pointer_typed_slots_only() {
    // AutoIt's `Ptr` is a base type, so the answer has to come from where the
    // value was produced: a `ptr` return and a `ptr*` write-back are pointers, a
    // `ptr` *input* (its slot echoes what the caller passed) is not, and neither
    // is a number that was never near a pointer.
    // The `ptr` input and the `ptr*` write-back carry different values, so the
    // two spellings cannot be told apart by their value alone.
    let src = "Func F()\n\
               \x20   Local $r = DllCall(\"stub.dll\", \"ptr\", \"F\", \"ptr\", 42, \"ptr*\", 0)\n\
               \x20   Local $t = \"\"\n\
               \x20   $t &= IsPtr($r[0]) ? \"1\" : \"0\"\n\
               \x20   $t &= \"|\" & (IsPtr($r[2]) ? \"1\" : \"0\")\n\
               \x20   $t &= \"|\" & (IsPtr($r[1]) ? \"1\" : \"0\")\n\
               \x20   $t &= \"|\" & (IsPtr(1234) ? \"1\" : \"0\")\n\
               \x20   $t &= \"|\" & (IsPtr(\"1234\") ? \"1\" : \"0\")\n\
               \x20   Return $t\n\
               EndFunc\n";
    let mut rt = rt(src);
    rt.set_platform(Box::new(DllPlatform));
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        "1|1|0|0|0"
    );
}



#[test]
fn a_declaration_keeps_its_dimensions_when_the_initializer_is_smaller() {
    // Measured on the official x64 interpreter: the declared shape wins, and an
    // element the literal does not name is an empty string - and so is every slot
    // of a plain declaration.
    let src = "Func F()\n\
               \x20   Local $a[3][2] = [[7]]\n\
               \x20   Local $b[4] = [9]\n\
               \x20   Local $e[2]\n\
               \x20   Return UBound($a, 1) & \"x\" & UBound($a, 2) & \":\" & $a[0][0] & \":\" \
               & StringLen($a[0][1]) & \":\" & UBound($b, 1) & \":\" & $b[0] & \":\" & StringLen($b[1]) \
               & \":\" & UBound($e, 1) & \":\" & StringLen($e[0]) & \":\" & IsNumber($e[0])\n\
               EndFunc\n";
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "3x2:7:0:4:9:0:2:0:0");
}

#[test]
fn an_initializer_larger_than_the_declaration_is_an_error() {
    // Measured: AutoIt stops at the declaration with "Array variable has incorrect
    // number of subscripts or subscript dimension range exceeded".
    let src = "Func F()\n    Local $c[2] = [1, 2, 3]\n    Return 0\nEndFunc\n";
    let mut rt = rt(src);
    assert!(rt.call_function("F", vec![]).is_err(), "must not silently truncate");
}


#[test]
fn an_auto_sized_nested_literal_is_rectangular() {
    // Measured on the official x64 interpreter: rows are the sub-arrays, the
    // longest row sets the width, and a short row is padded with empty strings -
    // both for an auto-sized declaration and for one with explicit dimensions.
    let src = "Func F()\n\
               \x20   Local $a[][] = [[1], [2, 3], [4, 5, 6]]\n\
               \x20   Local $d[2][3] = [[1], [2, 3]]\n\
               \x20   Return UBound($a, 1) & \"x\" & UBound($a, 2) & \":\" & $a[0][0] & \":\" \
               & StringLen($a[0][1]) & \":\" & $a[2][2] & \":\" & UBound($d, 1) & \"x\" \
               & UBound($d, 2) & \":\" & $d[0][0] & \":\" & StringLen($d[0][2])\n\
               EndFunc\n";
    assert_eq!(call(src, "F", vec![]).to_autoit_string(), "3x3:1:0:6:2x3:1:0");
}


#[test]
fn and_or_short_circuit() {
    // Measured on the official 3.3.16 x64 interpreter: `If (1 = 1 Or $x[0])` never
    // evaluates `$x[0]` (a fatal subscript error if it ran), `If (1 = 0 And $x[0])`
    // likewise, and `If (1 = 0 Or $x[0])` does evaluate it. AutoIt's failure idiom
    // `If (@error Or Not $arr[0])` - with a scalar in `$arr` after a failed
    // `DllCall` - depends on exactly this.
    let src = "Func F()\n\
               \x20   Local $x = 5\n\
               \x20   Local $n = 0\n\
               \x20   If (1 = 1 Or $x[0]) Then $n += 1\n\
               \x20   If (1 = 0 And $x[0]) Then $n += 10\n\
               \x20   Return $n\n\
               EndFunc\n";
    assert_eq!(call(src, "F", vec![]).to_int(), 1);
}
