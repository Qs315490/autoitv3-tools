//! `RegRead`/`RegWrite`/`RegDelete`/`RegEnumKey`/`RegEnumVal` against the real
//! registry.
//!
//! The semantics follow real AutoIt — type codes (`1`=Sz, `2`=ExpandSz,
//! `3`=Binary, `4`=Dword, `7`=MultiSz, `11`=Qword), shape-based type inference
//! when the type is omitted, `REG_BINARY` reading back as `Value::Binary` and
//! `REG_MULTI_SZ` as one `@LF`-joined string, `RegDelete` with a value
//! argument deleting that value (an empty name meaning the `(Default)` value)
//! while the two-argument-less form deletes the whole key, `@extended`
//! carrying the `$REG_*` type on successful reads, and the documented
//! `@error` ladder (`1` key, `2` main key, `-1` value, `-2` type) — so a
//! script behaves identically on either backend. Writes are gated by the
//! execution profile like every other side effect.
//!
//! Access always asks for the 64-bit view (`KEY_WOW64_64KEY`) so a 32-bit
//! build of this toolset is not silently redirected into `Wow6432Node`.

use std::rc::Rc;

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteValueW, RegEnumKeyExW, RegEnumValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE,
    KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, KEY_WRITE, REG_BINARY, REG_DWORD, REG_EXPAND_SZ,
    REG_MULTI_SZ, REG_OPTION_NON_VOLATILE, REG_QWORD, REG_SZ, REG_VALUE_TYPE,
};

use super::writes_allowed;

/// AutoIt's registry type-code constants.
const REG_SZ_CODE: i64 = 1;
const REG_EXPAND_SZ_CODE: i64 = 2;
const REG_BINARY_CODE: i64 = 3;
const REG_DWORD_CODE: i64 = 4;
const REG_MULTI_SZ_CODE: i64 = 7;
const REG_QWORD_CODE: i64 = 11;

/// Expand an AutoIt hive alias into a native root key, mirroring the
/// emulation layer's `expand_hive`.
fn root_of(path: &str) -> Option<(HKEY, String)> {
    let (head, rest) = match path.split_once('\\') {
        Some((h, r)) => (h, r.to_string()),
        None => (path, String::new()),
    };
    let root = match head.to_ascii_uppercase().as_str() {
        "HKLM" | "HKEY_LOCAL_MACHINE" => windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        "HKCU" | "HKEY_CURRENT_USER" => windows_sys::Win32::System::Registry::HKEY_CURRENT_USER,
        "HKCR" | "HKEY_CLASSES_ROOT" => windows_sys::Win32::System::Registry::HKEY_CLASSES_ROOT,
        "HKU" | "HKEY_USERS" => windows_sys::Win32::System::Registry::HKEY_USERS,
        "HKCC" | "HKEY_CURRENT_CONFIG" => {
            windows_sys::Win32::System::Registry::HKEY_CURRENT_CONFIG
        }
        "HKPD" | "HKEY_PERFORMANCE_DATA" => {
            windows_sys::Win32::System::Registry::HKEY_PERFORMANCE_DATA
        }
        _ => return None,
    };
    Some((root, rest))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read one value. `Ok` carries the value plus the `$REG_*` type code (for
/// `@extended`); `Err` is AutoIt's `@error` code — `2` the main key is
/// invalid, `1` the key cannot be opened, `-1` the value cannot be read,
/// `-2` the value type is unsupported.
pub(crate) fn reg_read(key: &str, value_name: &str) -> Result<(Value, i64), i64> {
    let Some((root, subkey)) = root_of(key) else {
        return Err(2);
    };
    let sub = wide(&subkey);
    let mut handle: HKEY = std::ptr::null_mut();
    let opened = unsafe {
        RegOpenKeyExW(
            root,
            sub.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut handle,
        )
    };
    if opened != ERROR_SUCCESS {
        return Err(1);
    }
    let name = wide(value_name);
    let mut kind: REG_VALUE_TYPE = 0;
    let mut size: u32 = 0;
    let status = unsafe {
        RegQueryValueExW(
            handle,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        unsafe { RegCloseKey(handle) };
        return Err(-1);
    }
    let mut buffer = vec![0u8; size as usize];
    let status = unsafe {
        RegQueryValueExW(
            handle,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buffer.as_mut_ptr(),
            &mut size,
        )
    };
    unsafe { RegCloseKey(handle) };
    if status != ERROR_SUCCESS {
        return Err(-1);
    }
    buffer.truncate(size as usize);
    let code = match kind {
        REG_DWORD => REG_DWORD_CODE,
        REG_QWORD => REG_QWORD_CODE,
        REG_BINARY => REG_BINARY_CODE,
        REG_MULTI_SZ => REG_MULTI_SZ_CODE,
        REG_EXPAND_SZ => REG_EXPAND_SZ_CODE,
        REG_SZ => REG_SZ_CODE,
        _ => return Err(-2),
    };
    let value = match kind {
        REG_DWORD => Value::Int(u32::from_le_bytes(
            buffer[..4].try_into().unwrap_or([0; 4]),
        ) as i64),
        REG_QWORD => Value::Int(u64::from_le_bytes(
            buffer[..8].try_into().unwrap_or([0; 8]),
        ) as i64),
        REG_BINARY => Value::Binary(Rc::new(buffer)),
        REG_MULTI_SZ => {
            // AutoIt joins a multi-string with `@LF` into a single string.
            let units: Vec<u16> = buffer
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
                .collect();
            Value::Str(
                units
                    .split(|u| *u == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf16_lossy(s))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        // REG_SZ / REG_EXPAND_SZ.
        _ => {
            let units: Vec<u16> = buffer
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
                .take_while(|u| *u != 0)
                .collect();
            Value::Str(String::from_utf16_lossy(&units))
        }
    };
    Ok((value, code))
}

/// Resolve a `RegWrite` type argument — `"REG_SZ"`-style spellings first,
/// then the numeric code.
fn type_code_of(v: &Value) -> Option<i64> {
    match v {
        Value::Str(text) => match text.trim().to_ascii_uppercase().as_str() {
            "REG_SZ" => Some(REG_SZ_CODE),
            "REG_EXPAND_SZ" => Some(REG_EXPAND_SZ_CODE),
            "REG_BINARY" => Some(REG_BINARY_CODE),
            "REG_DWORD" => Some(REG_DWORD_CODE),
            "REG_MULTI_SZ" => Some(REG_MULTI_SZ_CODE),
            "REG_QWORD" => Some(REG_QWORD_CODE),
            _ => None,
        },
        other => {
            let code = other.to_int();
            matches!(code, 1..=11).then_some(code)
        }
    }
}

/// What a write will store: the native registry type plus the encoded bytes.
fn encode_write(type_code: Option<i64>, value: &Value) -> Option<(u32, Vec<u8>)> {
    let code = type_code.unwrap_or_else(|| match value {
        Value::Binary(_) => REG_BINARY_CODE,
        Value::Array(_) => REG_MULTI_SZ_CODE,
        other if other.is_number() => REG_DWORD_CODE,
        _ => REG_SZ_CODE,
    });
    let text = value.to_autoit_string();
    Some(match code {
        REG_SZ_CODE => (
            REG_SZ,
            wide(&text).iter().flat_map(|u| u.to_le_bytes()).collect(),
        ),
        REG_EXPAND_SZ_CODE => (
            REG_EXPAND_SZ,
            wide(&text).iter().flat_map(|u| u.to_le_bytes()).collect(),
        ),
        REG_BINARY_CODE => (
            REG_BINARY,
            match value {
                Value::Binary(b) => b.as_ref().clone(),
                other => hex_bytes(&other.to_autoit_string()),
            },
        ),
        REG_DWORD_CODE => (
            REG_DWORD,
            (value.to_int() as u32).to_le_bytes().to_vec(),
        ),
        REG_MULTI_SZ_CODE => {
            let strings: Vec<String> = match value {
                Value::Array(items) => items
                    .borrow()
                    .iter()
                    .map(|v| v.to_autoit_string())
                    .collect(),
                _ => text.split('\n').map(|s| s.to_string()).collect(),
            };
            let mut bytes: Vec<u8> = strings
                .iter()
                .flat_map(|s| wide(s))
                .flat_map(|u| u.to_le_bytes())
                .collect();
            if strings.is_empty() {
                bytes.extend_from_slice(&[0, 0]);
            }
            (REG_MULTI_SZ, bytes)
        }
        REG_QWORD_CODE => (
            REG_QWORD,
            (value.to_int() as u64).to_le_bytes().to_vec(),
        ),
        _ => return None,
    })
}

fn hex_bytes(text: &str) -> Vec<u8> {
    let cleaned: String = text
        .trim()
        .strip_prefix("0x")
        .or_else(|| text.trim().strip_prefix("0X"))
        .unwrap_or(text.trim())
        .to_string();
    (0..cleaned.len() / 2)
        .filter_map(|i| u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// Open (creating if asked) a subkey of `root`.
fn open_key(root: HKEY, subkey: &str, create: bool, access: u32) -> Option<HKEY> {
    let sub = wide(subkey);
    let mut handle: HKEY = std::ptr::null_mut();
    let status = unsafe {
        if create {
            RegCreateKeyExW(
                root,
                sub.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                access | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut handle,
                std::ptr::null_mut(),
            )
        } else {
            RegOpenKeyExW(root, sub.as_ptr(), 0, access | KEY_WOW64_64KEY, &mut handle)
        }
    };
    (status == ERROR_SUCCESS).then_some(handle)
}

/// `RegWrite(key, value, [type], data)` — `1` on success, `0` + `@error` on
/// refusal, unknown type or write failure.
pub(crate) fn reg_write(
    args: &[Value],
    ctx: &mut dyn HostContext,
) -> Value {
    if !writes_allowed(ctx) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let key = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
    let value_name = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
    // 3-argument form omits the type; the 4th argument is the data. The type
    // is AutoIt's spelling ("REG_SZ", "REG_DWORD", …) or its numeric code.
    let (type_code, data_value) = if args.len() >= 4 {
        let code = args.get(2).map(type_code_of).unwrap_or(None);
        (code, &args[3])
    } else {
        (None, args.get(2).unwrap_or(&Value::Null))
    };
    let Some((kind, bytes)) = encode_write(type_code, data_value) else {
        ctx.set_error(1, 0);
        return Value::Int(0);
    };
    let Some((root, subkey)) = root_of(&key) else {
        ctx.set_error(1, 0);
        return Value::Int(0);
    };
    let Some(handle) = open_key(root, &subkey, true, KEY_WRITE) else {
        ctx.set_error(1, 0);
        return Value::Int(0);
    };
    let name = wide(&value_name);
    let status = unsafe {
        RegSetValueExW(
            handle,
            name.as_ptr(),
            0,
            kind,
            if bytes.is_empty() {
                std::ptr::null()
            } else {
                bytes.as_ptr()
            },
            bytes.len() as u32,
        )
    };
    unsafe { RegCloseKey(handle) };
    let ok = status == ERROR_SUCCESS;
    ctx.set_error(if ok { 0 } else { 1 }, 0);
    Value::Int(i64::from(ok))
}

/// `RegDelete(key [, value])` — without a value argument the whole key is
/// deleted; with one (even an empty name) only that value is removed, the
/// empty name addressing the `(Default)` value.
pub(crate) fn reg_delete(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !writes_allowed(ctx) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let key = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
    let value_name = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
    let Some((root, subkey)) = root_of(&key) else {
        ctx.set_error(1, 0);
        return Value::Int(0);
    };
    let ok = if args.len() < 2 {
        let sub = wide(&subkey);
        (unsafe { RegDeleteKeyW(root, sub.as_ptr()) }) == ERROR_SUCCESS
    } else {
        let Some(handle) = open_key(root, &subkey, false, KEY_SET_VALUE) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        let name = wide(&value_name);
        let status = unsafe { RegDeleteValueW(handle, name.as_ptr()) };
        unsafe { RegCloseKey(handle) };
        status == ERROR_SUCCESS
    };
    ctx.set_error(if ok { 0 } else { 1 }, 0);
    Value::Int(i64::from(ok))
}

/// `RegEnumKey(key, instance)` — 1-based; an out-of-range instance or a
/// missing key reports `@error = 1` and returns the empty string.
pub(crate) fn reg_enum_key(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    enum_step(args, ctx, true)
}

/// `RegEnumVal(key, instance)`.
pub(crate) fn reg_enum_val(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    enum_step(args, ctx, false)
}

fn enum_step(args: &[Value], ctx: &mut dyn HostContext, keys: bool) -> Value {
    let key = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
    let instance = args.get(1).map(|v| v.to_int()).unwrap_or(1);
    if instance < 1 {
        ctx.set_error(1, 0);
        return Value::str("");
    }
    let Some((root, subkey)) = root_of(&key) else {
        ctx.set_error(1, 0);
        return Value::str("");
    };
    let access = if keys { KEY_ENUMERATE_SUB_KEYS } else { KEY_QUERY_VALUE };
    let Some(handle) = open_key(root, &subkey, false, access) else {
        ctx.set_error(1, 0);
        return Value::str("");
    };
    let index = (instance - 1) as u32;
    let mut out = String::new();
    let mut found = false;
    let mut value_kind: REG_VALUE_TYPE = 0;
    unsafe {
        if keys {
            let mut name = [0u16; 261];
            let mut name_len32 = name.len() as u32;
            let status = RegEnumKeyExW(
                handle,
                index,
                name.as_mut_ptr(),
                &mut name_len32,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if status == ERROR_SUCCESS {
                let len = name.iter().position(|u| *u == 0).unwrap_or(name.len());
                out = String::from_utf16_lossy(&name[..len]);
                found = true;
            }
        } else {
            let mut name = [0u16; 16384];
            let mut name_len = name.len() as u32;
            let status = RegEnumValueW(
                handle,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                &mut value_kind,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if status == ERROR_SUCCESS {
                out = String::from_utf16_lossy(&name[..name_len as usize]);
                found = true;
            }
        }
    }
    unsafe { RegCloseKey(handle) };
    if !found {
        ctx.set_error(1, 0);
        return Value::str("");
    }
    ctx.set_error(0, i64::from(value_kind));
    Value::Str(out)
}
