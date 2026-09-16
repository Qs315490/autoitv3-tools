//! Unit tests for the file-backed resource chain.
//!
//! Kept out of the module so it reads as implementation; `#[path]` pulls the
//! file back in as a unit-test module, which is what lets it reach the layer's
//! private state and its sentinel handles.

use std::cell::Cell;
use std::rc::Rc;

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::Platform;
use autoitv3_runtime::profile::ExecutionProfile;
use autoitv3_runtime::value::Value;

use super::file_layer::{FileResourceLayer, FILE_MODULE};
use super::*;

/// A scratch directory unique to one test.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-resources-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// A `HostContext` with nothing but `@error`, which is all the layer sets.
struct Ctx {
    profile: ExecutionProfile,
    error: i64,
}

impl HostContext for Ctx {
    fn get_global(&self, _name: &str) -> Option<Value> {
        None
    }
    fn set_global(&mut self, _name: &str, _value: Value) {}
    fn error(&self) -> i64 {
        self.error
    }
    fn set_error(&mut self, error: i64, _extended: i64) {
        self.error = error;
    }
    fn profile(&self) -> &ExecutionProfile {
        &self.profile
    }
}

/// Drive one `DllCall` through the layer and return its return value.
fn dll(layer: &mut FileResourceLayer, function: &str, args: &[Value]) -> Value {
    let mut call = vec![
        Value::str("kernel32.dll"),
        Value::str("handle"),
        Value::str(function),
    ];
    for value in args {
        call.push(Value::str("ptr"));
        call.push(value.clone());
    }
    let mut ctx = Ctx {
        profile: ExecutionProfile::faithful(),
        error: 0,
    };
    let answer = layer
        .call("DllCall", call, &mut ctx)
        .expect("no runtime error")
        .expect("the layer answers its own chain");
    // `[return value, arg1, ...]`, the shape every DllCall has.
    let Value::Array(items) = answer else {
        panic!("DllCall returns an array");
    };
    let items = items.borrow();
    items[0].clone()
}

#[test]
fn a_name_finds_the_file_the_script_says_it_is() {
    // The build script's own `_Res_File_Add` line: the resource name is the
    // third field, the file the first.
    let dir = scratch("alias");
    std::fs::write(dir.join("payload.bin"), b"named payload").unwrap();
    std::fs::create_dir_all(dir.join("__Res64")).unwrap();
    std::fs::write(dir.join("__Res64").join("STAGED"), b"staged payload").unwrap();

    let mut files = ResourceFiles::default();
    files.with_dirs([dir.clone()]);
    files.with_aliases([("CFGDATA".to_string(), "payload.bin".to_string())]);

    assert_eq!(
        files.find(&Selector::name("cfgdata")).as_deref(),
        Some(&b"named payload"[..]),
        "the table is matched case-insensitively, like FindResourceW"
    );
    assert_eq!(
        files.find(&Selector::name("STAGED")).as_deref(),
        Some(&b"staged payload"[..]),
        "a name that is not in the table still finds its staged file"
    );
    assert!(files.find(&Selector::name("MISSING")).is_none());
    assert!(!files.is_empty(), "the table alone is something to answer from");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_layer_answers_the_whole_chain_with_real_memory() {
    // On Windows the chain is real Win32 calls; with no image to map, this
    // layer stands in for the script's own module and hands out a pointer the
    // native `RtlMoveMemory` can read.
    let dir = scratch("layer");
    std::fs::write(dir.join("payload.bin"), b"named payload").unwrap();

    let mut files = ResourceFiles::default();
    files.with_dirs([dir.clone()]);
    files.with_aliases([("CFGDATA".to_string(), "payload.bin".to_string())]);
    let mut layer = FileResourceLayer::new(files).expect("a layer with something to answer");

    let module = dll(&mut layer, "GetModuleHandleW", &[Value::Int(0)]);
    assert_eq!(module.to_int(), FILE_MODULE as i64);

    let handle = dll(
        &mut layer,
        "FindResourceW",
        &[module.clone(), Value::str("CFGDATA"), Value::Int(10)],
    );
    assert_ne!(handle.to_int(), 0, "the file answers the lookup");

    let size = dll(
        &mut layer,
        "SizeofResource",
        &[module.clone(), handle.clone()],
    );
    assert_eq!(size.to_int(), 13);

    let global = dll(&mut layer, "LoadResource", &[module, handle]);
    let pointer = dll(&mut layer, "LockResource", &[global]);
    assert_ne!(pointer.to_int(), 0);

    // A real address into memory the layer owns: this is what the native
    // `RtlMoveMemory` would copy out of.
    let bytes = unsafe {
        std::slice::from_raw_parts(pointer.to_int() as usize as *const u8, size.to_int() as usize)
    };
    assert_eq!(bytes, b"named payload");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_null_module_is_the_module_the_layer_stands_in_for() {
    // A script need not ask for the module handle first: `FindResourceW(0, …)`
    // is the shorter spelling of the same thing, and the emulation layer answers
    // it (it reads the name and the type and never looks at the module). This
    // layer has to answer it too, or the same script resolves its payload off
    // Windows and comes back empty on it.
    let dir = scratch("null-module");
    std::fs::write(dir.join("payload.bin"), b"named payload").unwrap();

    let mut files = ResourceFiles::default();
    files.with_dirs([dir.clone()]);
    files.with_aliases([("CFGDATA".to_string(), "payload.bin".to_string())]);
    let mut layer = FileResourceLayer::new(files).expect("a layer");

    let handle = dll(
        &mut layer,
        "FindResourceW",
        &[Value::Int(0), Value::str("CFGDATA"), Value::Int(10)],
    );
    assert_ne!(handle.to_int(), 0, "a NULL module still finds the file");

    let size = dll(&mut layer, "SizeofResource", &[Value::Int(0), handle.clone()]);
    assert_eq!(size.to_int(), 13);

    let global = dll(&mut layer, "LoadResource", &[Value::Int(0), handle]);
    let pointer = dll(&mut layer, "LockResource", &[global]);
    let bytes = unsafe {
        std::slice::from_raw_parts(pointer.to_int() as usize as *const u8, size.to_int() as usize)
    };
    assert_eq!(bytes, b"named payload");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn calls_that_are_not_the_layers_are_left_alone() {
    // Anything without the sentinel handle belongs to the native layer: a real
    // module's resource, or a `DllCall` that has nothing to do with resources.
    let dir = scratch("not-ours");
    std::fs::write(dir.join("payload.bin"), b"named payload").unwrap();
    let mut files = ResourceFiles::default();
    files.with_dirs([dir.clone()]);
    files.with_aliases([("CFGDATA".to_string(), "payload.bin".to_string())]);
    let mut layer = FileResourceLayer::new(files).expect("a layer");

    let mut ctx = Ctx {
        profile: ExecutionProfile::faithful(),
        error: 0,
    };
    for (function, args) in [
        ("GetModuleHandleW", vec![Value::str("kernel32.dll")]),
        ("FindResourceW", vec![Value::str("kernel32.dll"), Value::str("CFGDATA"), Value::Int(10)]),
        ("FindResourceW", vec![Value::Int(0x7fff_0000), Value::str("CFGDATA"), Value::Int(10)]),
        ("SizeofResource", vec![Value::Int(0x7fff_0000), Value::Int(1)]),
        // A `NULL` module is ours, but a handle we never issued is not: a real
        // `HRSRC` from some other module stays the native layer's to describe.
        ("SizeofResource", vec![Value::Int(0), Value::Int(0x7fff_0000)]),
        ("LoadResource", vec![Value::Int(0), Value::Int(0x7fff_0000)]),
        ("LoadResource", vec![Value::Int(0x7fff_0000), Value::Int(1)]),
        ("LockResource", vec![Value::Int(0x7fff_0000)]),
        ("GetTickCount", vec![]),
    ] {
        let mut call = vec![
            Value::str("kernel32.dll"),
            Value::str("handle"),
            Value::str(function),
        ];
        for value in args {
            call.push(Value::str("ptr"));
            call.push(value);
        }
        assert!(
            layer.call("DllCall", call, &mut ctx).expect("no error").is_none(),
            "{function} is not the layer's to answer"
        );
    }
    assert!(
        layer
            .call("ConsoleWrite", vec![Value::str("hi")], &mut ctx)
            .expect("no error")
            .is_none(),
        "only DllCall is answered here"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_layer_sits_in_front_of_the_native_one() {
    // The Windows stack is `file-resources → windows → common → winemu`, and
    // the native `DllCall` answers anything it is given: the order is what
    // decides who owns the resource chain. A stub stands in for the native
    // layer here, so the arrangement is checked on every host.
    struct Native(Rc<Cell<usize>>);
    impl Platform for Native {
        fn name(&self) -> &'static str {
            "stub-native"
        }
        fn provides(&self, _name: &str) -> bool {
            true
        }
        fn call(
            &mut self,
            _name: &str,
            _args: Vec<Value>,
            ctx: &mut dyn HostContext,
        ) -> Result<Option<Value>, autoitv3_runtime::RuntimeError> {
            self.0.set(self.0.get() + 1);
            ctx.set_error(0, 0);
            Ok(Some(Value::str("native")))
        }
    }

    let dir = scratch("in-front");
    std::fs::write(dir.join("payload.bin"), b"named payload").unwrap();
    let mut files = ResourceFiles::default();
    files.with_dirs([dir.clone()]);
    files.with_aliases([("CFGDATA".to_string(), "payload.bin".to_string())]);
    let layer = FileResourceLayer::new(files).expect("a layer");
    let native_calls = Rc::new(Cell::new(0));
    let mut stack = crate::CompositePlatform::new(
        "file-resources+stub",
        vec![Box::new(layer), Box::new(Native(native_calls.clone()))],
    );

    let mut ctx = Ctx {
        profile: ExecutionProfile::faithful(),
        error: 0,
    };
    let mut call = |stack: &mut crate::CompositePlatform, function: &str| {
        stack
            .call(
                "DllCall",
                vec![
                    Value::str("kernel32.dll"),
                    Value::str("handle"),
                    Value::str(function),
                    Value::str("ptr"),
                    Value::Int(0),
                ],
                &mut ctx,
            )
            .expect("no error")
            .expect("somebody answers")
    };
    let Value::Array(items) = call(&mut stack, "GetModuleHandleW") else {
        panic!("DllCall returns an array");
    };
    assert_eq!(items.borrow()[0].to_int(), FILE_MODULE as i64);
    assert_eq!(native_calls.get(), 0, "the file layer answered first");

    assert_eq!(call(&mut stack, "LoadLibraryW").to_autoit_string(), "native");
    assert_eq!(native_calls.get(), 1, "anything else reaches the native layer");

    let _ = std::fs::remove_dir_all(&dir);
}
