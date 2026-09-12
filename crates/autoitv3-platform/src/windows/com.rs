//! COM objects through `IDispatch` — the `Obj*` family against real
//! automation servers.
//!
//! `ObjCreate("ProgID")` goes through `CLSIDFromProgID` + `CoCreateInstance`
//! asking for `IID_IDispatch`; every member access is the classic late-bound
//! pair `GetIDsOfNames` → `Invoke` with a hand-marshalld `VARIANT` list. The
//! vtable is walked by slot (`IUnknown` 0-2, `IDispatch` 3-6) so no COM
//! bindings beyond the raw `CoCreateInstance` imports are needed.
//!
//! Supported value marshalling: `Int` (VT_I4/VT_I8 by width), `Float`
//! (VT_R8), `Bool` (VT_BOOL), `Str` (VT_BSTR), `Binary` (VT_UI1 safe array is
//! *not* built — a BSTR of the hex form is sent instead) and object results
//! (VT_DISPATCH → a new `Value::Obj`). `ObjEvent` is not implemented:
//! connection points need a message-pumping event sink, so it reports
//! `@error = 1` like the emulation layer does.

use std::rc::Rc;

use autoitv3_runtime::value::{NativeObject, ObjRef, Value};

use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::{
    CLSIDFromProgID, CoCreateInstance, CoInitializeEx, CLSCTX_ALL,
};
use windows_sys::Win32::Foundation::{SysAllocStringLen, SysFreeString};

/// `VT_*` codes used here.
const VT_EMPTY: u16 = 0;
const VT_NULL: u16 = 1;
const VT_I4: u16 = 3;
const VT_R8: u16 = 5;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;
const VT_BOOL: u16 = 11;
const VT_I8: u16 = 20;

/// `wFlags` for `Invoke`.
const DISPATCH_METHOD: u16 = 1;
const DISPATCH_PROPERTYGET: u16 = 2;
const DISPATCH_PROPERTYPUT: u16 = 4;

/// `DISPID_PROPERTYPUT` — the named-argument marker for property assignment.
const DISPID_PROPERTYPUT: i32 = -3;

/// IID_IDispatch `{00020400-0000-0000-C000-000000000046}` — only for
/// `CoCreateInstance`.
const IID_IDISPATCH: GUID = GUID {
    data1: 0x0002_0400,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

/// `GetIDsOfNames`/`Invoke` require `riid == IID_NULL`.
const IID_NULL: GUID = GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] };

/// A hand-marshalld `VARIANT` (24 bytes, matching the ABI layout).
#[repr(C)]
struct Variant {
    vt: u16,
    reserved: [u16; 3],
    data: [u64; 2],
}

impl Variant {
    fn empty() -> Self {
        Self { vt: VT_EMPTY, reserved: [0; 3], data: [0; 2] }
    }

    fn from_bstr(bstr: windows_sys::core::BSTR) -> Self {
        let mut v = Self::empty();
        v.vt = VT_BSTR;
        v.data[0] = bstr as u64;
        v
    }

    /// Free any owned payload (a BSTR) in place.
    unsafe fn clear(&mut self) {
        if self.vt == VT_BSTR && self.data[0] != 0 {
            SysFreeString(self.data[0] as *mut u16);
        }
        self.vt = VT_EMPTY;
        self.data = [0; 2];
    }
}

/// An `EXCEPINFO`-shaped scratch block; contents are never inspected, only
/// zeroed (the error surfaces through the returned `HRESULT`).
#[repr(C)]
struct ExcepInfo([u64; 8]);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn bstr(s: &str) -> windows_sys::core::BSTR {
    let units: Vec<u16> = s.encode_utf16().collect();
    unsafe { SysAllocStringLen(units.as_ptr() as _, units.len() as u32) }
}

fn succeeded(hr: i32) -> bool {
    hr >= 0
}

/// The function pointer at `slot` of a COM interface's vtable.
unsafe fn vtable_fn(iface: *mut core::ffi::c_void, slot: usize) -> *const core::ffi::c_void {
    let vtable = *(iface as *mut *mut *const core::ffi::c_void);
    *vtable.add(slot)
}

/// `IUnknown::AddRef` — keep the object alive while we hold it.
unsafe fn add_ref(iface: *mut core::ffi::c_void) {
    type FnAddRef = unsafe extern "system" fn(*mut core::ffi::c_void) -> u32;
    let f: FnAddRef = std::mem::transmute(vtable_fn(iface, 1));
    f(iface);
}

/// `IUnknown::Release`.
unsafe fn release(iface: *mut core::ffi::c_void) {
    type FnRelease = unsafe extern "system" fn(*mut core::ffi::c_void) -> u32;
    let f: FnRelease = std::mem::transmute(vtable_fn(iface, 2));
    f(iface);
}

/// `IDispatch::GetIDsOfNames` for one name.
unsafe fn dispatch_id(iface: *mut core::ffi::c_void, member: &str) -> Result<i32, i32> {
    type FnGetIds = unsafe extern "system" fn(
        *mut core::ffi::c_void, // this
        *const GUID,            // riid
        *mut *mut u16,          // rgszNames (LPOLESTR)
        u32,                    // cNames
        u32,                    // lcid
        *mut i32,               // rgDispId
    ) -> i32;
    let f: FnGetIds = std::mem::transmute(vtable_fn(iface, 5));
    let mut name = wide(member);
    let mut id: i32 = -1;
    let hr = f(
        iface,
        &IID_NULL,
        &mut name.as_mut_ptr() as *mut *mut u16,
        1,
        0,
        &mut id,
    );
    succeeded(hr).then_some(id).ok_or(hr)
}

/// `IDispatch::Invoke` with positional arguments.
unsafe fn dispatch_invoke(
    iface: *mut core::ffi::c_void,
    id: i32,
    flags: u16,
    args: &mut [Variant],
    put: bool,
) -> Result<Variant, i32> {
    type FnInvoke = unsafe extern "system" fn(
        *mut core::ffi::c_void, // this
        i32,                    // dispIdMember
        *const GUID,            // riid
        u32,                    // lcid
        u16,                    // wFlags
        *mut DispParams,        // pDispParams
        *mut Variant,           // pVarResult
        *mut ExcepInfo,         // pExcepInfo
        *mut u32,               // puArgErr
    ) -> i32;
    // COM arguments are right-to-left on the stack of DISPPARAMS.
    args.reverse();
    let mut params = DispParams {
        rgvarg: args.as_mut_ptr(),
        rgdispidNamedArgs: std::ptr::null_mut(),
        cArgs: args.len() as u32,
        cNamedArgs: 0,
    };
    let mut named: [i32; 1] = [DISPID_PROPERTYPUT];
    if put {
        params.rgdispidNamedArgs = named.as_mut_ptr();
        params.cNamedArgs = 1;
    }
    let mut result = Variant::empty();
    let mut excep = ExcepInfo([0; 8]);
    let mut arg_err: u32 = 0;
    let f: FnInvoke = std::mem::transmute(vtable_fn(iface, 6));
    let hr = f(
        iface, id, &IID_NULL, 0, flags, &mut params, &mut result, &mut excep, &mut arg_err,
    );
    succeeded(hr).then_some(result).ok_or(hr)
}

/// `DISPPARAMS`, mirroring the ABI layout.
#[repr(C)]
#[allow(non_snake_case)]
struct DispParams {
    rgvarg: *mut Variant,
    rgdispidNamedArgs: *mut i32,
    cArgs: u32,
    cNamedArgs: u32,
}

/// Encode an AutoIt value as an out-of-line `VARIANT` (owned BSTR payloads
/// are cleared by the caller after `Invoke`).
fn to_variant(value: &Value) -> Variant {
    let mut v = Variant::empty();
    match value {
        Value::Null | Value::Default => v.vt = VT_NULL,
        Value::Bool(b) => {
            v.vt = VT_BOOL;
            v.data[0] = u64::from(*b);
        }
        Value::Int(i) if i32::try_from(*i).is_ok() => {
            v.vt = VT_I4;
            v.data[0] = *i as u32 as u64;
        }
        Value::Int(i) => {
            v.vt = VT_I8;
            v.data[0] = *i as u64;
        }
        Value::Float(f) => {
            v.vt = VT_R8;
            v.data[0] = f.to_bits();
        }
        Value::Binary(b) => {
            // No safe-array marshalling: send the hex text form instead.
            let hex: String = b.iter().map(|byte| format!("{byte:02X}")).collect();
            v = to_variant(&Value::Str(format!("0x{hex}")));
        }
        other => {
            let text = other.to_autoit_string();
            v = Variant::from_bstr(bstr(&text));
        }
    }
    v
}

/// Decode an out-parameter `VARIANT` into an AutoIt value.
unsafe fn from_variant(v: &Variant) -> Value {
    match v.vt {
        VT_BSTR if v.data[0] != 0 => {
            // Read the BSTR; the byte-length prefix sits 4 bytes before it,
            // i.e. two `u16` slots back.
            let ptr = v.data[0] as *const u16;
            let len = *(ptr.wrapping_sub(2) as *const u32) as usize / 2;
            let slice = std::slice::from_raw_parts(ptr, len);
            Value::Str(String::from_utf16_lossy(slice))
        }
        VT_I4 => Value::Int((v.data[0] as u32) as i32 as i64),
        VT_I8 => Value::Int(v.data[0] as i64),
        VT_R8 => Value::Float(f64::from_bits(v.data[0])),
        VT_BOOL => Value::Bool(v.data[0] != 0),
        VT_DISPATCH if v.data[0] != 0 => {
            let iface = v.data[0] as *mut core::ffi::c_void;
            unsafe { add_ref(iface) };
            wrap_object("", iface)
        }
        _ => Value::Null,
    }
}

/// Wrap a raw `IDispatch*` in a runtime object whose destructor releases it.
fn wrap_object(name: &str, iface: *mut core::ffi::c_void) -> Value {
    Value::obj(Rc::new(NativeObject {
        name: name.to_string(),
        handle: iface as usize,
        release: Some(Box::new(move || unsafe { release(iface) })),
    }))
}

/// The raw dispatch pointer behind a runtime object.
fn dispatch_of(obj: &ObjRef) -> *mut core::ffi::c_void {
    obj.handle as *mut core::ffi::c_void
}

/// Ensure an STA COM apartment exists (first `ObjCreate` in the process).
fn ensure_com() -> Result<(), String> {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    static mut FAILED: bool = false;
    unsafe {
        ONCE.call_once(|| {
            let hr = CoInitializeEx(
                std::ptr::null(),
                windows_sys::Win32::System::Com::COINIT_MULTITHREADED as u32,
            );
            FAILED = !(succeeded(hr) || hr == windows_sys::Win32::Foundation::RPC_E_CHANGED_MODE);
        });
        if FAILED {
            return Err("CoInitializeEx failed".to_string());
        }
    }
    Ok(())
}

/// `ObjCreate("ProgID" [, args…])` — an `IDispatch`-backed object.
pub(crate) fn obj_create(name: &str, args: &[Value]) -> Result<Value, String> {
    ensure_com()?;
    if name.is_empty() {
        return Err("empty ProgID".to_string());
    }
    let prog_w = wide(name);
    let mut clsid: GUID = unsafe { std::mem::zeroed() };
    let hr = unsafe { CLSIDFromProgID(prog_w.as_ptr(), &mut clsid) };
    if !succeeded(hr) {
        return Err(format!("CLSIDFromProgID({name}) failed: 0x{hr:08X}"));
    }
    let mut iface: *mut core::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe {
        CoCreateInstance(&clsid, std::ptr::null_mut(), CLSCTX_ALL, &IID_IDISPATCH, &mut iface)
    };
    if !succeeded(hr) {
        return Err(format!("CoCreateInstance({name}) failed: 0x{hr:08X}"));
    }
    // Construction arguments: invoke the object's default member when given.
    if !args.is_empty() {
        let mut variants: Vec<Variant> = args.iter().map(to_variant).collect();
        unsafe {
            let id = dispatch_id(iface, "").unwrap_or(0);
            let _ = dispatch_invoke(iface, id, DISPATCH_METHOD | DISPATCH_PROPERTYGET, &mut variants, false);
        }
    }
    Ok(wrap_object(name, iface))
}

/// Resolve `member` and invoke it. `put` performs a property assignment.
fn invoke_member(
    obj: &ObjRef,
    member: &str,
    args: &[Value],
    put: bool,
) -> Result<Value, String> {
    let iface = dispatch_of(obj);
    if iface.is_null() {
        return Err("released object".to_string());
    }
    let mut variants: Vec<Variant> = args.iter().map(to_variant).collect();
    let id = unsafe { dispatch_id(iface, member) }
        .map_err(|hr| format!("{}.{}: 0x{hr:08X}", obj.name, member))?;
    let flags = if put {
        DISPATCH_PROPERTYPUT
    } else {
        DISPATCH_METHOD | DISPATCH_PROPERTYGET
    };
    let result = unsafe { dispatch_invoke(iface, id, flags, &mut variants, put) }
        .map_err(|hr| format!("{}.{}: 0x{hr:08X}", obj.name, member))?;
    for v in &mut variants {
        unsafe { v.clear() };
    }
    unsafe { Ok(from_variant(&result)) }
}

/// `$obj.Member` — property read (plain `PROPERTYGET`, no method bit: some
/// servers reject the combined flags for pure properties).
pub(crate) fn obj_get(obj: &ObjRef, member: &str) -> Result<Value, String> {
    let iface = dispatch_of(obj);
    if iface.is_null() {
        return Err("released object".to_string());
    }
    let id = unsafe { dispatch_id(iface, member) }
        .map_err(|hr| format!("{}.{}: 0x{hr:08X}", obj.name, member))?;
    let mut variants: Vec<Variant> = Vec::new();
    let result = unsafe { dispatch_invoke(iface, id, DISPATCH_PROPERTYGET, &mut variants, false) }
        .map_err(|hr| format!("{}.{}: 0x{hr:08X}", obj.name, member))?;
    unsafe { Ok(from_variant(&result)) }
}

/// `$obj.Member = value` — property assignment.
pub(crate) fn obj_set(obj: &ObjRef, member: &str, value: &Value) -> Result<Value, String> {
    invoke_member(obj, member, std::slice::from_ref(value), true)
}

/// `$obj.Method(args…)` — method dispatch.
pub(crate) fn obj_call(obj: &ObjRef, member: &str, args: &[Value]) -> Result<Value, String> {
    invoke_member(obj, member, args, false)
}

/// `IsObj($o)`.
pub(crate) fn is_obj(value: &Value) -> bool {
    matches!(value, Value::Obj(_))
}

/// `ObjName($o)` — the name it was created under.
pub(crate) fn obj_name(obj: &ObjRef) -> String {
    obj.name.clone()
}

#[cfg(test)]
mod com_tests {
    use super::*;
    use autoitv3_runtime::value::Value;

    #[test]
    fn fso_drive_exists_smoke() {
        let obj = match obj_create("Scripting.FileSystemObject", &[]) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skip: {e}");
                return;
            }
        };
        let Value::Obj(o) = &obj else { panic!("not an object") };
        let v = obj_call(o, "DriveExists", &[Value::Str("C:\\".to_string())]).expect("invoke");
        eprintln!("DriveExists -> {v:?}");
        assert!(matches!(v, Value::Bool(true) | Value::Int(1)), "got {v:?}");

        let folder = obj_call(o, "GetSpecialFolder", &[Value::Int(2)]).expect("invoke");
        eprintln!("GetSpecialFolder -> {folder:?}");
        let Value::Obj(f) = &folder else { panic!("folder not an object: {folder:?}") };
        let path = obj_get(f, "Path").expect("invoke");
        eprintln!("Path -> {path:?}");
        let name = obj_call(o, "GetTempName", &[]).expect("invoke");
        eprintln!("GetTempName -> {name:?}");
        assert!(matches!(path, Value::Str(ref s) if !s.is_empty()), "got {path:?}");
    }
}
