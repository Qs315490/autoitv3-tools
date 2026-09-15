//! Tests for execution profiles: the difference between *analysing* a script
//! (fast, reproducible, harmless) and *running* it (AutoIt semantics).
//!
//! These are the behaviours that used to be hard-coded for both callers at
//! once, which meant a normal run silently got deobfuscation's shortcuts.

use std::time::{Duration, Instant};

use autoitv3_platform::runtime_with_platform;
use autoitv3_runtime::profile::SleepPolicy;
use autoitv3_runtime::{EffectPolicy, ExecutionProfile, RandomPolicy, Runtime, Value};

fn parse(src: &str) -> autoitv3_ast::Program {
    autoitv3_ast::parse(src).expect("parses")
}

/// Run `Func F()` under `profile`.
fn run_with(profile: ExecutionProfile, body: &str) -> Value {
    let src = format!("Func F()\n{body}\nEndFunc\n");
    let prog = parse(&src);
    let mut rt = runtime_with_platform(&prog);
    rt.set_profile(profile);
    rt.call_function("F", vec![]).expect("no runtime error")
}

fn text_with(profile: ExecutionProfile, body: &str) -> String {
    run_with(profile, body).to_autoit_string()
}

/// A scratch path unique to one test.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-profile-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

// ---------------------------------------------------------------------------
// The presets themselves
// ---------------------------------------------------------------------------

#[test]
fn default_profile_is_faithful() {
    // A library must not silently change what a script does.
    assert_eq!(ExecutionProfile::default(), ExecutionProfile::faithful());
    assert_eq!(ExecutionProfile::default().sleep, SleepPolicy::Real);
    assert_eq!(ExecutionProfile::default().random, RandomPolicy::Entropy);
    assert_eq!(ExecutionProfile::default().effects, EffectPolicy::Allow);
    assert!(!ExecutionProfile::default().is_deterministic());

    let rt = Runtime::new();
    assert_eq!(*rt.profile(), ExecutionProfile::faithful());
}

#[test]
fn deterministic_profile_is_the_analysis_one() {
    let p = ExecutionProfile::deterministic();
    assert_eq!(p.sleep, SleepPolicy::Skip);
    assert!(matches!(p.random, RandomPolicy::Deterministic(_)));
    assert_eq!(p.effects, EffectPolicy::ReadOnly);
    assert!(p.is_deterministic());
}

// ---------------------------------------------------------------------------
// Sleep
// ---------------------------------------------------------------------------

#[test]
fn faithful_sleep_actually_waits() {
    let started = Instant::now();
    text_with(ExecutionProfile::faithful(), "Sleep(120)\n    Return 0");
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "faithful Sleep should wait, took {:?}",
        started.elapsed()
    );
}

#[test]
fn deterministic_sleep_returns_immediately() {
    let started = Instant::now();
    text_with(ExecutionProfile::deterministic(), "Sleep(5000)\n    Return 0");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "deterministic Sleep should not wait, took {:?}",
        started.elapsed()
    );
}

#[test]
fn capped_sleep_waits_no_longer_than_the_cap() {
    let started = Instant::now();
    text_with(
        ExecutionProfile::capped(Duration::from_millis(50)),
        "Sleep(5000)\n    Return 0",
    );
    let took = started.elapsed();
    assert!(took < Duration::from_millis(400), "capped Sleep took {took:?}");
}

#[test]
fn sleep_does_not_change_the_result_only_the_clock() {
    // The point of skipping Sleep: the script's answer is identical.
    let body = "Local $t = TimerInit()\n    Sleep(300)\n    Return Round(TimerDiff($t)) = 0";
    assert_eq!(text_with(ExecutionProfile::deterministic(), body), "True");
}

// ---------------------------------------------------------------------------
// Random
// ---------------------------------------------------------------------------

#[test]
fn deterministic_random_repeats_across_runs() {
    let body = "Local $a = Random(1, 1000000)\n    Local $b = Random(1, 1000000)\n    Return $a & \",\" & $b";
    let first = text_with(ExecutionProfile::deterministic(), body);
    let second = text_with(ExecutionProfile::deterministic(), body);
    assert_eq!(first, second, "deterministic runs must agree");
}

#[test]
fn entropy_random_differs_between_runs() {
    let body = "Return Random(1, 1000000000)";
    let a = text_with(ExecutionProfile::faithful(), body);
    // A collision is possible in principle but not in practice; try a few
    // times before declaring the seed fixed.
    let differs = (0..5).any(|_| text_with(ExecutionProfile::faithful(), body) != a);
    assert!(differs, "entropy seeding should vary between runs");
}

#[test]
fn explicit_randomseed_overrides_the_profile() {
    // An explicit seed wins in both profiles, so a script can pin its own
    // sequence regardless of how the interpreter was configured.
    let body = "RandomSeed(12345)\n    Return Random(1, 1000000000)";
    let determin = text_with(ExecutionProfile::deterministic(), body);
    let faithful = text_with(ExecutionProfile::faithful(), body);
    assert_eq!(determin, faithful);
    assert_eq!(
        determin,
        text_with(ExecutionProfile::deterministic(), body),
        "an explicit seed must be reproducible"
    );
}

// ---------------------------------------------------------------------------
// External effects
// ---------------------------------------------------------------------------

#[test]
fn deterministic_profile_refuses_to_write() {
    let dir = scratch("readonly");
    let path = dir.join("out.txt");
    let body = format!(
        r#"Local $h = FileOpen("{p}", 2)
    Local $w = FileWrite($h, "data")
    Local $e = @error
    FileClose($h)
    Return $h & ":" & $w & ":" & $e & ":" & FileExists("{p}")"#,
        p = path.display()
    );
    // The open itself is refused — the *refusal* is what carries `@error = 1`
    // — so nothing is created; the `FileWrite` on the resulting bad handle is
    // a plain 0 with `@error` 0, the way the help page has it.
    assert_eq!(text_with(ExecutionProfile::deterministic(), &body), "-1:0:0:0");
    assert!(!path.exists(), "deterministic profile must not create files");
}

#[test]
fn faithful_profile_writes_for_real() {
    let dir = scratch("allow");
    let path = dir.join("out.txt");
    let body = format!(
        r#"Local $h = FileOpen("{p}", 2)
    Local $w = FileWrite($h, "data")
    FileClose($h)
    Return ($w > 0) & ":" & (FileExists("{p}") = 1)"#,
        p = path.display()
    );
    assert_eq!(text_with(ExecutionProfile::faithful(), &body), "True:True");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
}

#[test]
fn deterministic_profile_refuses_deletes_creates_and_env_changes() {
    let dir = scratch("mutations");
    let file = dir.join("keep.txt");
    std::fs::write(&file, "keep me").unwrap();
    let newdir = dir.join("created");

    let body = format!(
        r#"Local $del = FileDelete("{f}")
    Local $mk = DirCreate("{d}")
    Local $env = EnvSet("AU3_PROFILE_TEST", "x")
    Return ($del & $mk & $env) & ":" & (FileExists("{f}") = 1) & ":" & DirGetSize("{d}")"#,
        f = file.display(),
        d = newdir.display()
    );
    // All three calls are refused (0), the file survives, and the directory was
    // never created — `DirGetSize` answers the documented `-1` for a path that
    // is not there.
    assert_eq!(text_with(ExecutionProfile::deterministic(), &body), "000:True:-1");
    assert!(file.exists(), "the file must not have been deleted");
    assert!(!newdir.exists(), "the directory must not have been created");
}

#[test]
fn reading_is_allowed_in_both_profiles() {
    // Deobfuscation has to follow the code, so reads must keep working.
    let dir = scratch("reads");
    let path = dir.join("in.txt");
    std::fs::write(&path, "hello").unwrap();
    let body = format!(
        r#"Local $h = FileOpen("{p}", 0)
    Local $s = FileRead($h)
    FileClose($h)
    Return $s & ":" & FileGetSize("{p}")"#,
        p = path.display()
    );
    assert_eq!(text_with(ExecutionProfile::deterministic(), &body), "hello:5");
    assert_eq!(text_with(ExecutionProfile::faithful(), &body), "hello:5");
}

// ---------------------------------------------------------------------------
// The deobfuscation pipeline states its profile explicitly
// ---------------------------------------------------------------------------

#[test]
fn deobfuscation_is_reproducible() {
    // `deobfuscate` must not depend on the clock or on entropy.
    let src = r#"
Func MergeArrays(ByRef $t, Const ByRef $s)
    ReDim $t[$t[0] + $s[0] + 1]
    Local $i
    For $i = 1 To $s[0]
        $t[$t[0] + $i] = $s[$i]
    Next
    $t[0] += $s[0]
EndFunc
Func BuildFunctionTable()
    Local $x[] = [0x2, Foo, Bar]
    MergeArrays($x, [0x1, Baz][0])
    Return $x
EndFunc
Func F()
    Sleep(1000)
    Return Random(1, 1000000)
EndFunc
"#;
    let mut a = parse(src);
    let mut b = parse(src);
    let ra = autoitv3_deobf::deobfuscate(&mut a);
    let rb = autoitv3_deobf::deobfuscate(&mut b);
    assert_eq!(ra.folds, rb.folds);
    assert_eq!(ra.table.entries, rb.table.entries);
}