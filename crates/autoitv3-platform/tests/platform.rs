//! Tests for the OS platform layer.

use autoitv3_platform::{host_platform, runtime_with_platform, GenericPlatform};
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::Value;

fn autoit3_parse(src: &str) -> autoitv3_ast::Program {
    autoitv3_ast::parse(src).expect("parses")
}

#[test]
fn host_platform_matches_the_target_os() {
    let p = host_platform();
    let expected = if cfg!(windows) { "windows" } else { "linux-generic" };
    assert_eq!(p.name(), expected);
}

#[test]
fn runtime_helper_installs_the_platform() {
    let prog = autoit3_parse("Func F()\n    Return 1\nEndFunc\n");
    let mut rt = runtime_with_platform(&prog);
    assert_eq!(
        rt.platform_name(),
        if cfg!(windows) { "windows" } else { "linux-generic" }
    );
    assert!(matches!(rt.call_function("F", vec![]).unwrap(), Value::Int(1)));
}

#[test]
fn portable_platform_provides_nothing_os_specific() {
    // AutoIt is a Windows tool: off Windows the honest answer is "not
    // provided", which the interpreter turns into an undefined-function error.
    let p = GenericPlatform::new();
    assert!(!p.provides("RegRead"));
    assert!(!p.provides("DllCall"));
}

#[test]
fn a_platform_can_supply_an_os_builtin() {
    use autoitv3_runtime::error::RuntimeError;
    use autoitv3_runtime::host::HostContext;

    /// Stands in for the future Windows module.
    struct FakeWindows;
    impl Platform for FakeWindows {
        fn name(&self) -> &'static str {
            "fake-windows"
        }
        fn provides(&self, name: &str) -> bool {
            name.eq_ignore_ascii_case("RegRead")
        }
        fn call(
            &mut self,
            name: &str,
            _args: Vec<Value>,
            _ctx: &mut dyn HostContext,
        ) -> Result<Option<Value>, RuntimeError> {
            if name.eq_ignore_ascii_case("RegRead") {
                return Ok(Some(Value::str("installed")));
            }
            Ok(None)
        }
    }

    let prog = autoit3_parse(
        "Func F()\n    Return RegRead(\"HKEY_LOCAL_MACHINE\\\\X\", \"Y\")\nEndFunc\n",
    );
    let mut rt = autoitv3_runtime::Runtime::with_program(&prog);
    rt.set_platform(Box::new(FakeWindows));
    assert_eq!(rt.platform_name(), "fake-windows");
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        "installed"
    );
}