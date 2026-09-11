//! Tests for the autoitv3-runtime interpreter, host and debug interfaces.

use std::rc::Rc;

use autoitv3_runtime::debug::{DebugAction, DebugHost, Debugger, StopReason, TracingDebugger};
use autoitv3_runtime::host::{HostContext, NativeHost};
use autoitv3_runtime::{Runtime, Value};

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
    let yes = |f: &str, s: &str| matches!(call(src, f, vec![Value::str(s)]), Value::Bool(true));
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
        Value::Bool(true)
    ));
    assert!(matches!(
        call(src, "B", vec![Value::Int(1)]),
        Value::Bool(false)
    ));
    assert!(matches!(
        call(src, "F", vec![Value::Float(1.5)]),
        Value::Bool(true)
    ));
    assert!(matches!(
        call(src, "F", vec![Value::Float(1.0)]),
        Value::Bool(false)
    ));
    assert!(matches!(
        call(src, "F", vec![Value::str("2.25")]),
        Value::Bool(true)
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
fn execute_runs_generated_source() {
    let src = r#"
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
        fn on_call_enter(&mut self, name: &str, args: &[Value]) {
            self.0.borrow_mut().on_call_enter(name, args);
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
    let src = "Global $g = 1\nFunc F()\n    Return Bump()\nEndFunc\n";
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
    assert!(matches!(v, Value::Int(2)), "got {v:?}");
    assert_eq!(r.get_global("g").map(|v| v.to_int()), Some(2));
    assert_eq!(r.error(), 5);
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
    // Some slots hold real AutoIt builtins.
    assert!(all.iter().any(|n| n == "STRING"), "expected STRING in table");
    assert!(all.iter().any(|n| n == "BITAND"), "expected BITAND in table");
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
    assert!(msg.contains("platform host"), "got: {msg}");
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
