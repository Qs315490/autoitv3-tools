//! Tests for the autoitv3-runtime interpreter, host and debug interfaces.

use autoitv3_runtime::debug::{DebugAction, Debugger, StopReason, TracingDebugger};
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
        ) -> DebugAction {
            self.0.borrow_mut().on_statement(span, depth)
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
    // `BuildFunctionTable()` builds the script's function table from array
    // literals merged by `MergeArrays` — the exact workload the deobfuscator
    // depends on.
    let Some(src) = sample_script() else { return };
    let prog = autoit3_parse(&src);
    let mut rt = Runtime::with_program(&prog);
    let v = rt
        .call_function("BuildFunctionTable", vec![])
        .expect("builder should evaluate");
    let Value::Array(a) = v else { panic!("builder did not return an array") };
    let a = a.borrow();
    // Element 0 is the count; the real table has 1108 function names.
    assert_eq!(a[0].to_int(), 1108, "table count");
    assert_eq!(a.len(), 1109, "count + entries");
    // A couple of slots resolve to real AutoIt builtins.
    let all: Vec<String> = a.iter().map(|v| v.to_autoit_string()).collect();
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
