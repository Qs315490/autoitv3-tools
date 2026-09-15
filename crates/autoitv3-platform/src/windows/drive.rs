//! The `DriveGet*` family through the real Win32 drive and volume APIs.
//!
//! `DriveMap*` is not implemented natively (it is an emulation-layer
//! function); everything here reads the actual volumes this process can see.

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
    SetVolumeLabelW,
};

use autoitv3_runtime::profile::EffectKind;

/// Which `DriveGet*` field a call wants.
#[derive(Clone, Copy)]
pub(crate) enum DriveField {
    FileSystem,
    Label,
    Serial,
    SpaceTotal,
    SpaceFree,
}

/// All drive roots as `C:\`-style strings.
fn logical_drives() -> Vec<String> {
    unsafe {
        let len = GetLogicalDriveStringsW(0, std::ptr::null_mut());
        if len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        if GetLogicalDriveStringsW(len, buf.as_mut_ptr()) == 0 {
            return Vec::new();
        }
        buf.truncate(len as usize);
        // The buffer is a NUL-separated list with a trailing NUL.
        buf.split(|u| *u == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}

/// Normalise an argument into a `C:\`-style root, or `None` when it does not
/// name a drive this system knows.
fn drive_root(args: &[Value]) -> Option<String> {
    let raw = args.first()?.to_autoit_string();
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        return None;
    }
    // Accept "C", "C:", "C:\", and paths — the drive letter is what matters.
    let letter = s.remove(0).to_ascii_uppercase();
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    let root = format!("{letter}:\\");
    logical_drives()
        .into_iter()
        .find(|d| d.eq_ignore_ascii_case(&root))
}

/// `DriveGetDrive("ALL"|"FIXED"|"CDROM"|"REMOVABLE"|"NETWORK"|"RAMDISK")`.
///
/// The type may be a **list** — `"FIXED,REMOVABLE"` asks for either kind, which
/// is how a script looks for "somewhere a Windows directory could be" — and the
/// result is AutoIt's shape: `[0]` is how many drives were found and the letters
/// start at `[1]`. A failure — an unknown type, or no drives of it — is
/// `@error = 1` and an empty *string*, which is what the official interpreter
/// answers (measured with `DriveGetDrive("BOGUS")`).
pub(crate) fn drive_get_drive(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    let wanted = args
        .first()
        .map(|v| v.to_autoit_string().to_ascii_uppercase())
        .unwrap_or_else(|| "ALL".to_string());
    let wanted = if wanted.trim().is_empty() {
        "ALL".to_string()
    } else {
        wanted
    };
    let kinds: Vec<&str> = wanted
        .split(',')
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .collect();
    let drives: Vec<Value> = logical_drives()
        .into_iter()
        .filter(|root| {
            let kind = drive_kind_string(root);
            kinds.iter().any(|other| {
                *other == "ALL"
                    || kind.eq_ignore_ascii_case(other)
                    // AutoIt reports FIXED for "UNKNOWN" drives too.
                    || (matches!(*other, "UNKNOWN") && kind == "UNKNOWN")
            })
        })
        .map(Value::Str)
        .collect();
    if drives.is_empty() {
        ctx.set_error(1, 0);
        return Value::Str(String::new());
    }
    ctx.set_error(0, 0);
    let mut out = vec![Value::Int(drives.len() as i64)];
    out.extend(drives);
    Value::array(out)
}

fn drive_kind_string(root: &str) -> String {
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    match unsafe { GetDriveTypeW(wide.as_ptr()) } {
        2 => "REMOVABLE",
        3 => "FIXED",
        4 => "NETWORK",
        5 => "CDROM",
        6 => "RAMDISK",
        _ => "UNKNOWN",
    }
    .to_string()
}

/// `DriveGetType(drive)`.
pub(crate) fn drive_get_type(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    match drive_root(args) {
        Some(root) => {
            ctx.set_error(0, 0);
            Value::Str(drive_kind_string(&root))
        }
        None => {
            ctx.set_error(1, 0);
            Value::str("")
        }
    }
}

/// `DriveGetStatus(drive)` — "READY", "NOTREADY", or "INVALID".
pub(crate) fn drive_get_status(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    let Some(root) = drive_root(args) else {
        ctx.set_error(1, 0);
        return Value::Str("INVALID".to_string());
    };
    // A volume that answers `GetVolumeInformationW` is ready.
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let ready = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        ) != 0
    };
    ctx.set_error(0, 0);
    Value::Str(if ready { "READY" } else { "NOTREADY" }.to_string())
}

/// The `DriveGetFileSystem/Label/Serial/SpaceTotal/SpaceFree` family.
pub(crate) fn drive_get_field(
    args: &[Value],
    ctx: &mut dyn HostContext,
    field: DriveField,
) -> Value {
    let Some(root) = drive_root(args) else {
        ctx.set_error(1, 0);
        return match field {
            DriveField::Serial | DriveField::SpaceTotal | DriveField::SpaceFree => {
                Value::Int(0)
            }
            _ => Value::str(""),
        };
    };
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let mut volume = [0u16; 261];
    let mut fs_name = [0u16; 261];
    let mut serial: u32 = 0;
    let mut free_bytes: u64 = 0;
    let mut total_bytes: u64 = 0;
    let ok =
        unsafe { GetVolumeInformationW(wide.as_ptr(), volume.as_mut_ptr(), 261, &mut serial, std::ptr::null_mut(), std::ptr::null_mut(), fs_name.as_mut_ptr(), 261) } != 0;
    // Space queries stay valid for a not-yet-mounted volume letter.
    let space_ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut total_bytes,
            &mut free_bytes,
        )
    } != 0;
    let len = |buf: &[u16]| buf.iter().position(|u| *u == 0).unwrap_or(buf.len());
    match field {
        DriveField::FileSystem => {
            if !ok {
                ctx.set_error(1, 0);
                return Value::str("");
            }
            ctx.set_error(0, 0);
            Value::Str(String::from_utf16_lossy(&fs_name[..len(&fs_name)]))
        }
        DriveField::Label => {
            if !ok {
                ctx.set_error(1, 0);
                return Value::str("");
            }
            ctx.set_error(0, 0);
            Value::Str(String::from_utf16_lossy(&volume[..len(&volume)]))
        }
        DriveField::Serial => {
            if !ok {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
            ctx.set_error(0, 0);
            Value::Int(serial as i64)
        }
        DriveField::SpaceTotal => {
            if !space_ok {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
            ctx.set_error(0, 0);
            Value::Int((total_bytes / (1024 * 1024)) as i64)
        }
        DriveField::SpaceFree => {
            if !space_ok {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
            ctx.set_error(0, 0);
            Value::Int((free_bytes / (1024 * 1024)) as i64)
        }
    }
}

/// `DriveSetLabel(drive, label)` — effect-gated.
pub(crate) fn drive_set_label(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !ctx.effect_allowed(EffectKind::FileWrite) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let Some(root) = drive_root(args) else {
        ctx.set_error(1, 0);
        return Value::Int(0);
    };
    let label = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
    let root_wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let label_wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
    let ok = unsafe { SetVolumeLabelW(root_wide.as_ptr(), label_wide.as_ptr()) } != 0;
    ctx.set_error(if ok { 0 } else { 1 }, 0);
    Value::Int(i64::from(ok))
}
