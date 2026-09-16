//! Unit tests for the Windows stack's arrangement.
//!
//! `windows_stack` and `file_resource_layer` are compiled for the crate's tests
//! on every host (the module only builds them for production on Windows), so
//! the layering the CLI depends on is checked here rather than only on a
//! Windows machine.

use autoitv3_runtime::Platform;

use crate::{file_resource_layer, windows_stack, CommonPlatform, WindowsEmulation};

/// A stand-in for the native layer, which is not compiled off Windows.
struct Native;

impl Platform for Native {
    fn name(&self) -> &'static str {
        "stub-native"
    }
}

fn stack(file_layer: Option<Box<dyn Platform>>, emulation: Option<Box<dyn Platform>>) -> String {
    windows_stack(
        Box::new(Native),
        Box::new(CommonPlatform::new()),
        file_layer,
        emulation,
    )
    .name()
    .to_string()
}

#[test]
fn the_stack_names_its_layers_in_order() {
    assert_eq!(stack(None, None), "windows+common");
    assert_eq!(stack(None, Some(Box::new(Native))), "windows+common+winemu");

    let dir = std::env::temp_dir().join(format!("au3-stack-order-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("__PAYLOAD"), b"payload").unwrap();
    let emulation = WindowsEmulation::new().with_resource_dirs([dir.clone()]);
    let layer = file_resource_layer(&emulation).expect("files to answer from");
    assert_eq!(
        stack(Some(layer), Some(Box::new(Native))),
        "file-resources+windows+common+winemu"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_file_layer_is_installed_only_when_it_has_something_to_answer() {
    // Nothing staged and no table: the run goes straight to real Win32.
    assert!(file_resource_layer(&WindowsEmulation::new()).is_none());

    // An image wins over files — the native layer maps it, and the real
    // `FindResourceW` is then the right answer.
    let dir = std::env::temp_dir().join(format!("au3-stack-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("__PAYLOAD"), b"payload").unwrap();
    let image = dir.join("build.exe");
    std::fs::write(&image, b"MZ not really").unwrap();
    let with_image = WindowsEmulation::new()
        .with_resource_dirs([dir.clone()])
        .with_module_file(&image);
    assert!(file_resource_layer(&with_image).is_none());

    // Staged files (no image): the layer takes over the chain.
    let staged = WindowsEmulation::new().with_resource_dirs([dir.clone()]);
    assert!(file_resource_layer(&staged).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
