//! Native `DllCall` — `LoadLibraryW`/`GetProcAddress` plus a type-driven
//! invocation bridge.
//!
//! AutoIt's call shape is `DllCall("dll", "rettype", "func", type1, val1, …)`.
//! The return is an array whose element 0 is the return value and whose
//! remaining elements echo the arguments, updated in place for the
//! by-reference types (`int*`, `wstr*`, …) — the same shape the emulation
//! layer produces.
//!
//! # Invocation
//!
//! On x64 the Windows calling convention passes integer-class arguments in
//! `rcx`/`rdx`/`r8`/`r9` and float-class ones in `xmm0`–`xmm3`, with the rest
//! on the stack. A variadic call uses exactly the same placement, so every
//! homogeneous call goes through one variadic site: all-integer argument
//! lists as `usize` words, all-float lists as `f64`. A *mixed* argument list
//! would need per-argument register-class placement that a single variadic
//! signature cannot express, so it is rejected with `@error = 1` (and
//! reported under `AU3_WINEMU_TRACE`) rather than mis-called.
//!
//! On x86 the variadic form forces caller cleanup (cdecl), so the stdcall
//! targets AutoIt binds by default go through explicit arities instead, and
//! the same homogeneous-only rule applies.

use std::rc::Rc;

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use super::WindowsPlatform;

/// Maximum supported argument count.
const MAX_ARGS: usize = 16;

/// How an AutoIt `DllCall` type maps onto the FFI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ArgClass {
    /// Integer / pointer / handle / bool — passed as a machine word.
    Int,
    /// `float`/`double` — passed as an f64.
    Float,
    /// ANSI string (`char*`) — passed as a pointer.
    Str,
    /// UTF-16 string (`wstr`) — passed as a pointer.
    WStr,
    /// A `DllStruct` handle or pointer.
    Struct,
}

/// One parsed type: the class plus whether it is by-reference (`*` suffix).
#[derive(Clone, Copy, Debug)]
struct ArgType {
    class: ArgClass,
    by_ref: bool,
    /// Width in bytes for the integer classes.
    width: u8,
    signed: bool,
    /// True for the pointer-ish types (`ptr`, `handle`, `hwnd`, …) whose
    /// integer argument may actually be a `DllStruct` handle or pointer.
    is_pointer: bool,
}

/// Parse a type token. Unknown tokens fail the whole call, like AutoIt.
fn parse_type(raw: &str) -> Option<ArgType> {
    let token = raw.trim().to_ascii_lowercase();
    let (base, by_ref) = match token.strip_suffix('*') {
        Some(b) => (b.to_string(), true),
        None => (token, false),
    };
    // Be lenient about a convention prefix left on an argument type.
    let base = base.rsplit(':').next().unwrap_or("").to_string();
    let ptr_size = (usize::BITS / 8) as u8;
    let (class, width, signed, is_pointer) = match base.as_str() {
        "none" | "void" => (ArgClass::Int, 4, false, false),
        "bool" | "boolean" | "int" | "long" => (ArgClass::Int, 4, true, false),
        "byte" | "ubyte" => (ArgClass::Int, 1, false, false),
        "char" => (ArgClass::Int, 1, true, false),
        "short" => (ArgClass::Int, 2, true, false),
        "ushort" | "wchar" => (ArgClass::Int, 2, false, false),
        "int_ptr" | "long_ptr" | "lresult" | "lparam" | "wparam" => {
            (ArgClass::Int, ptr_size, true, false)
        }
        "uint_ptr" | "ulong_ptr" | "dword_ptr" | "size_t" => {
            (ArgClass::Int, ptr_size, false, false)
        }
        "uint" | "ulong" | "dword" => (ArgClass::Int, 4, false, false),
        "handle" | "hwnd" | "hwnd_ptr" | "hfile" | "hmodule" | "hinstance" | "hbitmap"
        | "hicon" | "hcursor" | "hfont" | "hbrush" | "hdesk" | "hhook" | "hglobal"
        | "hprocess" | "hthread" | "hkey" | "hsocket" | "hlocal" | "hdwp" | "hdc" | "hrgn" => {
            (ArgClass::Int, ptr_size, false, true)
        }
        "ptr" => (ArgClass::Int, ptr_size, false, true),
        "int64" | "qword" => (ArgClass::Int, 8, true, false),
        "uint64" => (ArgClass::Int, 8, false, false),
        "float" => (ArgClass::Float, 4, true, false),
        "double" => (ArgClass::Float, 8, true, false),
        "str" | "astr" => (ArgClass::Str, ptr_size, false, false),
        "wstr" => (ArgClass::WStr, ptr_size, false, false),
        "struct" => (ArgClass::Struct, ptr_size, false, false),
        _ => return None,
    };
    Some(ArgType {
        class,
        by_ref,
        width,
        signed,
        is_pointer,
    })
}

/// Calling convention, honoured on 32-bit hosts (x64 has one convention).
#[derive(Clone, Copy)]
enum Convention {
    StdCall,
    Cdecl,
}

/// One prepared argument.
enum ArgSlot {
    /// A plain machine word.
    Word(u64),
    /// A float value.
    Float(f64),
    /// Owned ANSI string, NUL-terminated; passed as a pointer.
    Str(Rc<[u8]>),
    /// Owned UTF-16 string, NUL-terminated; passed as a pointer.
    WStr(Rc<[u16]>),
    /// Scratch buffer for a by-reference argument, plus its element width.
    Buffer(Rc<[u8]>, u8),
    /// A struct's live memory: real address plus the handle to echo back.
    StructBuf(u64, i64),
}

impl ArgSlot {
    fn is_float(&self) -> bool {
        matches!(self, ArgSlot::Float(_))
    }

    /// The machine word handed to the callee.
    fn word(&self) -> usize {
        match self {
            ArgSlot::Word(w) => *w as usize,
            ArgSlot::Str(s) => s.as_ptr() as usize,
            ArgSlot::WStr(s) => s.as_ptr() as usize,
            ArgSlot::Buffer(b, _) => b.as_ptr() as usize,
            ArgSlot::StructBuf(addr, _) => *addr as usize,
            ArgSlot::Float(_) => 0,
        }
    }

    /// The array element this argument echoes back as.
    fn result(&self, ty: &ArgType, original: &Value) -> Value {
        match self {
            ArgSlot::Buffer(buf, width) if ty.by_ref => match ty.class {
                ArgClass::Int => {
                    let mut word = [0u8; 8];
                    word[..*width as usize].copy_from_slice(&buf[..*width as usize]);
                    int_to_value(u64::from_le_bytes(word), *width, ty.signed)
                }
                ArgClass::Float => {
                    if *width == 4 {
                        Value::Float(f32::from_le_bytes(buf[..4].try_into().unwrap()) as f64)
                    } else {
                        Value::Float(f64::from_le_bytes(buf[..8].try_into().unwrap()))
                    }
                }
                ArgClass::Str => {
                    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
                    Value::Str(String::from_utf8_lossy(&buf[..end]).into_owned())
                }
                ArgClass::WStr => {
                    let units: Vec<u16> = buf
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
                        .collect();
                    let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
                    Value::Str(String::from_utf16_lossy(&units[..end]))
                }
                _ => original.clone(),
            },
            ArgSlot::StructBuf(_, handle) => Value::Int(*handle),
            _ => original.clone(),
        }
    }
}

fn value_to_word(value: &Value) -> u64 {
    match value {
        Value::Bool(b) => u64::from(*b),
        other => other.to_int() as u64,
    }
}

fn int_to_value(word: u64, width: u8, signed: bool) -> Value {
    if width >= 8 {
        return Value::Int(if signed { word as i64 } else { word as i64 });
    }
    if !signed {
        return Value::Int((word & ((1u64 << (width * 8)) - 1)) as i64);
    }
    let bits = width * 8;
    let sign = 1u64 << (bits - 1);
    Value::Int(if word & sign != 0 {
        (word | !((1u64 << bits) - 1)) as i64
    } else {
        word as i64
    })
}

fn read_wstr(ptr: usize) -> Value {
    if ptr == 0 {
        return Value::str("");
    }
    let mut units = Vec::new();
    unsafe {
        let mut p = ptr as *const u16;
        while *p != 0 {
            units.push(*p);
            p = p.add(1);
            if units.len() > 32 * 1024 {
                break;
            }
        }
    }
    Value::Str(String::from_utf16_lossy(&units))
}

fn read_astr(ptr: usize) -> Value {
    if ptr == 0 {
        return Value::str("");
    }
    let mut bytes = Vec::new();
    unsafe {
        let mut p = ptr as *const u8;
        while *p != 0 {
            bytes.push(*p);
            p = p.add(1);
            if bytes.len() > 64 * 1024 {
                break;
            }
        }
    }
    Value::Str(String::from_utf8_lossy(&bytes).into_owned())
}

impl WindowsPlatform {
    /// `DllCall(dll, rettype, function, type1, val1, …)`.
    pub(crate) fn dll_call(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let dll = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
        let ret_raw = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
        let function = args
            .get(2)
            .map(|v| v.to_autoit_string())
            .unwrap_or_default();

        // Peel an explicit calling convention off the return type
        // (`cdecl:int`); x64 ignores it, x86 honours it.
        let (convention, ret_type_raw) = match ret_raw.split_once(':') {
            Some(("cdecl", rest)) => (Convention::Cdecl, rest.to_string()),
            Some(("stdcall", rest)) => (Convention::StdCall, rest.to_string()),
            _ => (Convention::StdCall, ret_raw.clone()),
        };
        let Some(ret_type) = parse_type(&ret_type_raw) else {
            ctx.set_error(2, 0);
            return Value::Int(0);
        };

        let pairs: Vec<(String, Value)> = args
            .get(3..)
            .unwrap_or(&[])
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| (pair[0].to_autoit_string(), pair[1].clone()))
            .collect();
        if pairs.len() > MAX_ARGS {
            ctx.set_error(2, 0);
            return Value::Int(0);
        }
        let mut types = Vec::with_capacity(pairs.len());
        for (token, _) in &pairs {
            match parse_type(token) {
                Some(t) => types.push(t),
                None => {
                    ctx.set_error(2, 0);
                    return Value::Int(0);
                }
            }
        }

        let module = match args.first() {
            Some(v) => self.resolve_module(v),
            None => 0,
        };
        if module == 0 {
            // The DLL itself is unavailable (`DllOpen` failed or a bad handle).
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let Some(address) = load_function(module, &function) else {
            // The DLL loaded but does not export the requested function.
            self.note_unimplemented_call(&dll, &function);
            ctx.set_error(3, 0);
            return Value::Int(0);
        };

        // ---- prepare arguments ----
        let mut slots: Vec<ArgSlot> = Vec::with_capacity(pairs.len());
        for (i, (_, value)) in pairs.iter().enumerate() {
            match self.prepare_arg(&types[i], value) {
                Some(slot) => slots.push(slot),
                None => {
                    ctx.set_error(2, 0);
                    return Value::Int(0);
                }
            }
        }

        // ---- invoke ----
        let float_args = slots.iter().any(ArgSlot::is_float);
        let all_float = slots.iter().all(ArgSlot::is_float);
        let float_ret = ret_type.class == ArgClass::Float;
        let raw = if !float_args || all_float {
            invoke(address, &slots, all_float, float_ret, convention)
        } else {
            // Mixed integer/float argument classes: a single variadic call
            // cannot place each argument in its right register file.
            self.note_unimplemented_call(&dll, &function);
            ctx.set_error(1, 0);
            return Value::Int(0);
        };

        // ---- collect results ----
        let mut out = Vec::with_capacity(pairs.len() + 1);
        out.push(match ret_type.class {
            ArgClass::Int => int_to_value(raw.0 as u64, ret_type.width, ret_type.signed),
            ArgClass::Float => Value::Float(raw.1),
            ArgClass::Str => read_astr(raw.0),
            ArgClass::WStr => read_wstr(raw.0),
            ArgClass::Struct => Value::Int(0),
        });
        for (i, slot) in slots.iter().enumerate() {
            out.push(slot.result(&types[i], &pairs[i].1));
        }
        ctx.set_error(0, 0);
        Value::array(out)
    }

    /// `DllCallAddress(rettype, address, type1, val1, …)` — invoke a raw
    /// routine pointer with the same type machinery as `DllCall`.
    pub(crate) fn dll_call_address(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let ret_raw = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
        let address = args.get(1).map(|v| v.to_int()).unwrap_or(0);
        if address <= 0 {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let (convention, ret_type_raw) = match ret_raw.split_once(':') {
            Some(("cdecl", rest)) => (Convention::Cdecl, rest.to_string()),
            Some(("stdcall", rest)) => (Convention::StdCall, rest.to_string()),
            _ => (Convention::StdCall, ret_raw.clone()),
        };
        let Some(ret_type) = parse_type(&ret_type_raw) else {
            ctx.set_error(2, 0);
            return Value::Int(0);
        };
        let pairs: Vec<(String, Value)> = args
            .get(2..)
            .unwrap_or(&[])
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| (pair[0].to_autoit_string(), pair[1].clone()))
            .collect();
        if pairs.len() > MAX_ARGS {
            ctx.set_error(2, 0);
            return Value::Int(0);
        }
        let mut types = Vec::with_capacity(pairs.len());
        for (token, _) in &pairs {
            match parse_type(token) {
                Some(t) => types.push(t),
                None => {
                    ctx.set_error(2, 0);
                    return Value::Int(0);
                }
            }
        }
        let mut slots: Vec<ArgSlot> = Vec::with_capacity(pairs.len());
        for (i, (_, value)) in pairs.iter().enumerate() {
            match self.prepare_arg(&types[i], value) {
                Some(slot) => slots.push(slot),
                None => {
                    ctx.set_error(2, 0);
                    return Value::Int(0);
                }
            }
        }
        let float_args = slots.iter().any(ArgSlot::is_float);
        let all_float = slots.iter().all(ArgSlot::is_float);
        let float_ret = ret_type.class == ArgClass::Float;
        let raw = if !float_args || all_float {
            invoke(address as usize, &slots, all_float, float_ret, convention)
        } else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        let mut out = Vec::with_capacity(pairs.len() + 1);
        out.push(match ret_type.class {
            ArgClass::Int => int_to_value(raw.0 as u64, ret_type.width, ret_type.signed),
            ArgClass::Float => Value::Float(raw.1),
            ArgClass::Str => read_astr(raw.0),
            ArgClass::WStr => read_wstr(raw.0),
            ArgClass::Struct => Value::Int(0),
        });
        for (i, slot) in slots.iter().enumerate() {
            out.push(slot.result(&types[i], &pairs[i].1));
        }
        ctx.set_error(0, 0);
        Value::array(out)
    }

    /// `DllOpen` — really load the library so `DllCall` can use the handle.
    pub(crate) fn dll_open(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let name = args
            .first()
            .map(|v| v.to_autoit_string())
            .unwrap_or_default();
        let handle = load_library(&name);
        if handle == 0 {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        self.track_module(handle);
        ctx.set_error(0, 0);
        // The real module address: scripts hand this to pointer arguments.
        Value::Int(handle as i64)
    }

    /// `DllClose` — free a handle this layer handed out.
    pub(crate) fn dll_close(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = args.first().map(|v| v.to_int()).unwrap_or(0);
        if self.close_module(handle) {
            ctx.set_error(0, 0);
            Value::Int(1)
        } else {
            ctx.set_error(1, 0);
            Value::Int(0)
        }
    }

    /// The first argument is a library name or a module handle.
    fn resolve_module(&mut self, value: &Value) -> usize {
        match value {
            Value::Int(handle) => *handle as usize,
            other => load_library(&other.to_autoit_string()),
        }
    }

    /// Turn one AutoIt value into an FFI slot for `ty`.
    fn prepare_arg(&mut self, ty: &ArgType, value: &Value) -> Option<ArgSlot> {
        // By-reference: a scratch buffer the callee can write through.
        if ty.by_ref {
            return match ty.class {
                ArgClass::Int => {
                    let mut buf = [0u8; 8];
                    let bytes = &value_to_word(value).to_le_bytes()[..ty.width as usize];
                    buf[..ty.width as usize].copy_from_slice(bytes);
                    Some(ArgSlot::Buffer(Rc::from(buf.as_slice()), ty.width))
                }
                ArgClass::Float => {
                    let mut buf = [0u8; 8];
                    if ty.width == 4 {
                        buf[..4].copy_from_slice(&(value.to_f64() as f32).to_le_bytes());
                    } else {
                        buf.copy_from_slice(&value.to_f64().to_le_bytes());
                    }
                    Some(ArgSlot::Buffer(Rc::from(buf.as_slice()), ty.width))
                }
                ArgClass::Str => {
                    let mut buf = value.to_autoit_string().into_bytes();
                    buf.push(0);
                    Some(ArgSlot::Buffer(Rc::from(buf.as_slice()), 1))
                }
                ArgClass::WStr => {
                    let mut units: Vec<u16> = value.to_autoit_string().encode_utf16().collect();
                    units.push(0);
                    let mut bytes = Vec::with_capacity(units.len() * 2);
                    for u in units {
                        bytes.extend_from_slice(&u.to_le_bytes());
                    }
                    Some(ArgSlot::Buffer(Rc::from(bytes.as_slice()), 2))
                }
                ArgClass::Struct => match self.struct_memory(value.to_int()) {
                    Some((addr, _)) => Some(ArgSlot::StructBuf(addr, value.to_int())),
                    // `struct*` also carries raw pointers (`DllStructGetPtr`
                    // of foreign memory, a `LockResource` address, …); pass
                    // the value through when it names no live struct.
                    None => Some(ArgSlot::Word(value_to_word(value))),
                },
            };
        }
        Some(match ty.class {
            // A pointer-type integer may name one of our structs (a handle
            // from `DllStructCreate` or an address from `DllStructGetPtr`);
            // pass the struct's real memory in that case.
            ArgClass::Int if ty.is_pointer && value.is_number() => {
                match self.struct_memory(value.to_int()) {
                    Some((addr, _)) => ArgSlot::StructBuf(addr, value.to_int()),
                    None => ArgSlot::Word(value_to_word(value)),
                }
            }
            ArgClass::Int => ArgSlot::Word(value_to_word(value)),
            ArgClass::Float => ArgSlot::Float(value.to_f64()),
            ArgClass::Str => {
                let mut buf = value.to_autoit_string().into_bytes();
                buf.push(0);
                ArgSlot::Str(Rc::from(buf.as_slice()))
            }
            ArgClass::WStr => {
                let mut units: Vec<u16> = value.to_autoit_string().encode_utf16().collect();
                units.push(0);
                ArgSlot::WStr(Rc::from(units.as_slice()))
            }
            ArgClass::Struct => match self.struct_memory(value.to_int()) {
                Some((addr, _)) => ArgSlot::StructBuf(addr, value.to_int()),
                None => ArgSlot::Word(value_to_word(value)),
            },
        })
    }

    /// A struct argument's real address. The address is the struct's own
    /// (never-reallocated) heap buffer, so a native callee writes through it
    /// and the live struct observes the changes with no copy-back.
    fn struct_memory(&mut self, handle: i64) -> Option<(u64, ())> {
        let s = self.struct_any_mut(handle)?;
        Some((s.address(), ()))
    }
}

// ---------------------------------------------------------------------------
// loading
// ---------------------------------------------------------------------------

/// `LoadLibraryW`, falling back to an already-loaded module.
pub(crate) fn load_library(name: &str) -> usize {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
    if name.is_empty() {
        return 0;
    }
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let loaded = (unsafe { LoadLibraryW(wide.as_ptr()) }) as usize;
    if loaded != 0 {
        return loaded;
    }
    // A name without extension or a module that is already mapped.
    (unsafe { GetModuleHandleW(wide.as_ptr()) }) as usize
}

pub(crate) fn load_function(module: usize, name: &str) -> Option<usize> {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    if module == 0 || name.is_empty() {
        return None;
    }
    let proc = unsafe { GetProcAddress(module as _, format!("{name}\0").as_ptr() as *const u8) };
    proc.map(|f| f as usize)
}

pub(crate) fn free_library(module: usize) {
    use windows_sys::Win32::Foundation::FreeLibrary;
    if module != 0 {
        unsafe { FreeLibrary(module as *mut _) };
    }
}

// ---------------------------------------------------------------------------
// invocation bridges
// ---------------------------------------------------------------------------

/// Invoke one variadic call site with the argument expressions `$a`.
macro_rules! call {
    ($f:expr, [$($a:expr),*]) => {
        unsafe { $f($($a),*) }
    };
}

/// Invoke `address` with the prepared slots.
///
/// Homogeneous lists only: `all_float` selects the f64 bridge, everything else
/// goes through the machine-word bridge. Both are variadic on x64 (variadic
/// and non-variadic share one convention there) and explicit-arity on x86.
fn invoke(
    address: usize,
    slots: &[ArgSlot],
    all_float: bool,
    float_ret: bool,
    convention: Convention,
) -> (usize, f64) {
    if all_float {
        let args: Vec<f64> = slots
            .iter()
            .map(|s| match s {
                ArgSlot::Float(f) => *f,
                _ => 0.0,
            })
            .collect();
        invoke_float(address, &args, float_ret)
    } else {
        let args: Vec<usize> = slots.iter().map(|s| s.word()).collect();
        invoke_words(address, &args, float_ret, convention)
    }
}

/// The machine-word bridge. `extern "system"` is stdcall on x86, cdecl
/// nowhere; `extern "cdecl"` covers the `cdecl:` declarations.
fn invoke_words(
    address: usize,
    args: &[usize],
    float_ret: bool,
    #[allow(unused_variables)] convention: Convention,
) -> (usize, f64) {
    macro_rules! arms {
        ([$($t:ty),*], [$($a:expr),*]) => {{
            // x64: one variadic site per arity; the first argument is the
            // named anchor the variadic form requires.
            #[cfg(target_pointer_width = "64")]
            {
                if float_ret {
                    let f: unsafe extern "system" fn(usize, ...) -> f64 =
                        unsafe { std::mem::transmute(address) };
                    return (0, call!(f, [$($a),*]));
                } else {
                    let f: unsafe extern "system" fn(usize, ...) -> usize =
                        unsafe { std::mem::transmute(address) };
                    return (call!(f, [$($a),*]), 0.0);
                }
            }
            // x86: the declared signature must match exactly, stdcall by
            // default and cdecl when the declaration asked for it.
            #[cfg(not(target_pointer_width = "64"))]
            match convention {
                Convention::StdCall => {
                    if float_ret {
                        let f: unsafe extern "system" fn($($t),*) -> f64 =
                            unsafe { std::mem::transmute(address) };
                        return (0, call!(f, [$($a),*]));
                    } else {
                        let f: unsafe extern "system" fn($($t),*) -> usize =
                            unsafe { std::mem::transmute(address) };
                        return (call!(f, [$($a),*]), 0.0);
                    }
                }
                Convention::Cdecl => {
                    if float_ret {
                        let f: unsafe extern "cdecl" fn($($t),*) -> f64 =
                            unsafe { std::mem::transmute(address) };
                        return (0, call!(f, [$($a),*]));
                    } else {
                        let f: unsafe extern "cdecl" fn($($t),*) -> usize =
                            unsafe { std::mem::transmute(address) };
                        return (call!(f, [$($a),*]), 0.0);
                    }
                }
            }
        }};
    }
    match args.len() {
        0 => {
            if float_ret {
                let f: unsafe extern "system" fn() -> f64 = unsafe { std::mem::transmute(address) };
                (0, unsafe { f() })
            } else {
                #[cfg(target_pointer_width = "64")]
                {
                    let f: unsafe extern "system" fn() -> usize =
                        unsafe { std::mem::transmute(address) };
                    (unsafe { f() }, 0.0)
                }
                #[cfg(not(target_pointer_width = "64"))]
                match convention {
                    Convention::StdCall => {
                        let f: unsafe extern "system" fn() -> usize =
                            unsafe { std::mem::transmute(address) };
                        (unsafe { f() }, 0.0)
                    }
                    Convention::Cdecl => {
                        let f: unsafe extern "cdecl" fn() -> usize =
                            unsafe { std::mem::transmute(address) };
                        (unsafe { f() }, 0.0)
                    }
                }
            }
        }
        1 => arms!([usize], [args[0]]),
        2 => arms!([usize, usize], [args[0], args[1]]),
        3 => arms!([usize, usize, usize], [args[0], args[1], args[2]]),
        4 => arms!([usize, usize, usize, usize], [args[0], args[1], args[2], args[3]]),
        5 => arms!([usize, usize, usize, usize, usize], [args[0], args[1], args[2], args[3], args[4]]),
        6 => arms!([usize, usize, usize, usize, usize, usize], [args[0], args[1], args[2], args[3], args[4], args[5]]),
        7 => arms!([usize, usize, usize, usize, usize, usize, usize], [args[0], args[1], args[2], args[3], args[4], args[5], args[6]]),
        8 => arms!([usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7]
        ]),
        9 => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8]
        ]),
        10 => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8],
            args[9]
        ]),
        11 => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8],
            args[9], args[10]
        ]),
        12 => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8],
            args[9], args[10], args[11]
        ]),
        13 => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8],
            args[9], args[10], args[11], args[12]
        ]),
        14 => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8],
            args[9], args[10], args[11], args[12], args[13]
        ]),
        15 => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8],
            args[9], args[10], args[11], args[12], args[13], args[14]
        ]),
        _ => arms!([usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize, usize], [
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], args[8],
            args[9], args[10], args[11], args[12], args[13], args[14], args[15]
        ]),
    }
}

/// The float bridge: every argument is an f64. On x64 the variadic form places
/// them in the xmm registers exactly like the callee expects.
fn invoke_float(address: usize, args: &[f64], float_ret: bool) -> (usize, f64) {
    macro_rules! arms {
        ([$($a:expr),*]) => {{
            if float_ret {
                let f: unsafe extern "system" fn(f64, ...) -> f64 = unsafe { std::mem::transmute(address) };
                return (0, call!(f, [$($a),*]));
            } else {
                let f: unsafe extern "system" fn(f64, ...) -> usize = unsafe { std::mem::transmute(address) };
                return (call!(f, [$($a),*]), 0.0);
            }
        }};
    }
    match args.len() {
        0 => {
            if float_ret {
                let f: unsafe extern "system" fn() -> f64 = unsafe { std::mem::transmute(address) };
                (0, unsafe { f() })
            } else {
                let f: unsafe extern "system" fn() -> usize = unsafe { std::mem::transmute(address) };
                (unsafe { f() }, 0.0)
            }
        }
        1 => arms!([args[0]]),
        2 => arms!([args[0], args[1]]),
        3 => arms!([args[0], args[1], args[2]]),
        4 => arms!([args[0], args[1], args[2], args[3]]),
        5 => arms!([args[0], args[1], args[2], args[3], args[4]]),
        6 => arms!([args[0], args[1], args[2], args[3], args[4], args[5]]),
        7 => arms!([args[0], args[1], args[2], args[3], args[4], args[5], args[6]]),
        _ => arms!([
            args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7]
        ]),
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn resolves_known_exports() {
        let m = load_library("kernel32.dll");
        assert!(m != 0, "kernel32 not loaded");
        assert!(load_function(m, "GetCurrentProcessId").is_some());
        assert!(load_function(m, "GetSystemTimeAsFileTime").is_some());
        assert!(load_function(m, "lstrlenW").is_some());
        assert!(load_function(m, "GetVersionExW").is_some());
        assert!(load_function(m, "RtlMoveMemory").is_some());
        assert!(load_function(m, "NoSuchExportInTheDll").is_none());
    }
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private type table.
#[cfg(test)]
#[path = "../../tests/unit/windows_dll.rs"]
mod tests;
