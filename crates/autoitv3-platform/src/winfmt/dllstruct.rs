//! The `DllStruct*` family: an in-memory emulation of AutoIt's binary structs.
//!
//! AutoIt scripts that want to know the Windows version do not call a builtin;
//! they declare a native structure and hand a pointer to `DllCall`:
//!
//! ```autoit
//! Local $t = DllStructCreate("struct;dword OSVersionInfoSize;" & _
//!     "dword MajorVersion;dword MinorVersion;dword BuildNumber;" & _
//!     "dword PlatformId;wchar CSDVersion[128];endstruct")
//! DllStructSetData($t, "OSVersionInfoSize", DllStructGetSize($t))
//! DllCall("kernel32.dll", "int", "GetVersionExW", "ptr", $t)
//! Local $major = DllStructGetData($t, "MajorVersion")
//! ```
//!
//! This module supplies just enough of that model to run such a query off
//! Windows: a definition parser, a byte buffer, and typed field access. It is
//! deliberately **not** a general FFI — nothing here touches real memory or a
//! real DLL, so a struct is an opaque handle backed by a `Vec<u8>` the
//! emulation can fill in (see the emulation layer's `DllCall`); on a real
//! Windows host the native layer points the same buffer at real heap memory.
//!
//! Supported definition syntax:
//!
//! * the `struct;...;endstruct` wrapper (nested wrappers are flattened)
//! * `byte char ubyte wchar short ushort word int uint long ulong dword
//!   int64 uint64 ptr handle hwnd float double bool`
//! * arrays (`wchar CSDVersion[128]`, `byte[16]`)
//! * unnamed fields (referred to by 1-based index)
//! * `align N`, which pads the next field to an N-byte boundary

use std::cell::RefCell;
use std::rc::Rc;

use autoitv3_i18n::msg;
use autoitv3_runtime::value::Value;

use crate::winfmt::WindowsArch;

/// One field in a struct definition.
#[derive(Debug, Clone)]
struct Field {
    /// Declared name; empty when the definition used an unnamed field.
    name: String,
    /// Size of a single element in bytes.
    elem_size: usize,
    /// Number of elements (`1` for a scalar, `N` for `T x[N]`).
    count: usize,
    /// Byte offset from the start of the struct.
    offset: usize,
    /// True for `char`, whose arrays read back as a string.
    is_char: bool,
    /// True for `byte`/`ubyte`, whose arrays read back as a **binary** value —
    /// that is how a script pulls an encrypted blob out of a struct.
    is_binary: bool,
    /// True for `wchar`, whose arrays read back as a UTF-16 string.
    is_wchar: bool,
    /// True for floating point fields.
    is_float: bool,
    /// Whether an integer field is signed.
    signed: bool,
}

impl Field {
    /// Total bytes the field occupies.
    fn total_size(&self) -> usize {
        self.elem_size * self.count
    }
}

/// A parsed, allocated native structure.
///
/// The bytes live behind an `Rc<RefCell<..>>` because `DllStructCreate` can map
/// a *second* struct onto memory that already exists (`DllStructCreate($def,
/// $ptr)`). Both views then have to observe each other's writes, exactly as
/// they do when they are two pointers into the same Windows allocation.
#[derive(Debug, Clone)]
pub struct DllStruct {
    definition: String,
    fields: Vec<Field>,
    /// Backing bytes, shared with every struct mapped over this memory.
    data: Rc<RefCell<Vec<u8>>>,
    /// Where this struct starts inside `data`; non-zero for an alias.
    offset: usize,
    /// Size of this struct in bytes — the alias may be smaller than its owner.
    size: usize,
    /// Where the buffer lives in the emulated address space, so that
    /// `DllStructGetPtr` hands out something `RtlMoveMemory` and friends can
    /// write through. Assigned by the emulation layer at creation.
    address: u64,
}

impl DllStruct {
    /// Parse `definition` and allocate a zeroed buffer for it.
    ///
    /// The pointer size depends on `arch`, so the same definition has different
    /// layout on x86 and x64 — exactly as it does on Windows.
    pub fn create(definition: &str, arch: WindowsArch) -> Result<Self, String> {
        let fields = parse_fields(definition, arch)?;
        let size = fields
            .last()
            .map(|f| f.offset + f.total_size())
            .unwrap_or(0);
        Ok(Self {
            definition: definition.to_string(),
            fields,
            data: Rc::new(RefCell::new(vec![0u8; size])),
            offset: 0,
            size,
            address: 0,
        })
    }

    /// Map `definition` onto memory that already exists at `address`.
    ///
    /// `storage`/`offset` locate that memory; the new struct shares it instead
    /// of allocating, so a write through either view is seen by the other.
    pub fn create_over(
        definition: &str,
        arch: WindowsArch,
        storage: Rc<RefCell<Vec<u8>>>,
        offset: usize,
        address: u64,
    ) -> Result<Self, String> {
        let mut s = Self::create(definition, arch)?;
        s.data = storage;
        s.offset = offset;
        s.address = address;
        Ok(s)
    }

    /// Where this struct lives in the emulated address space.
    pub fn address(&self) -> u64 {
        self.address
    }

    /// Give the struct an address (called by the emulation layer).
    pub fn set_address(&mut self, address: u64) {
        self.address = address;
    }

    /// The real heap address of this struct's own backing buffer.
    ///
    /// The buffer is allocated at exactly its final size and never reallocated,
    /// so the pointer stays valid for as long as the struct lives — the native
    /// Windows layer hands it to real `DllCall` targets.
    pub fn real_address(&self) -> u64 {
        self.data.borrow().as_ptr() as u64
    }

    /// The backing bytes and this struct's offset into them, for mapping
    /// another struct over the same memory.
    pub fn storage(&self) -> (Rc<RefCell<Vec<u8>>>, usize) {
        (Rc::clone(&self.data), self.offset)
    }

    /// The struct's bytes, a copy so the caller does not hold the borrow.
    pub fn bytes(&self) -> Vec<u8> {
        let data = self.data.borrow();
        data[self.offset..self.offset + self.size].to_vec()
    }

    /// Overwrite the struct's first `bytes.len()` bytes.
    pub fn write_all(&self, bytes: &[u8]) -> bool {
        self.write_at(0, bytes)
    }

    /// Overwrite `bytes.len()` bytes at `at` (struct-relative).
    pub fn write_at(&self, at: usize, bytes: &[u8]) -> bool {
        if at + bytes.len() > self.size {
            return false;
        }
        let start = self.offset + at;
        let mut data = self.data.borrow_mut();
        if start + bytes.len() > data.len() {
            return false;
        }
        data[start..start + bytes.len()].copy_from_slice(bytes);
        true
    }

    /// The definition string the struct was created from.
    pub fn definition(&self) -> &str {
        &self.definition
    }

    /// Total size in bytes, as `DllStructGetSize` reports it.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether the definition produced no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Resolve a field selector: a case-insensitive name, or a 1-based index.
    pub fn field_index(&self, selector: &FieldSelector) -> Option<usize> {
        match selector {
            FieldSelector::Index(i) => {
                if *i >= 1 && (*i as usize) <= self.fields.len() {
                    Some(*i as usize - 1)
                } else {
                    None
                }
            }
            FieldSelector::Name(name) => {
                let wanted = name.to_ascii_lowercase();
                self.fields
                    .iter()
                    .position(|f| !f.name.is_empty() && f.name.to_ascii_lowercase() == wanted)
            }
        }
    }

    /// Find a field by any of `aliases`, so a caller can accept both the
    /// Win32 spelling (`dwMajorVersion`) and the short one (`MajorVersion`).
    pub fn field_alias(&self, aliases: &[&str]) -> Option<usize> {
        self.fields.iter().position(|f| {
            !f.name.is_empty()
                && aliases
                    .iter()
                    .any(|a| f.name.eq_ignore_ascii_case(a))
        })
    }

    /// Read a field as an AutoIt value.
    ///
    /// Integer fields become `Int`, `float`/`double` become `Float`, and
    /// `char`/`wchar` fields (including arrays) become a string up to the first
    /// NUL. `element` selects one element of an array field (1-based); pass
    /// `None` for the whole field.
    pub fn get(&self, field: usize, element: Option<usize>) -> Option<Value> {
        let f = self.fields.get(field)?;
        if f.is_binary && f.count > 1 {
            let bytes = self.read(f.offset, f.total_size())?;
            return Some(Value::Binary(std::rc::Rc::new(bytes)));
        }
        if f.is_char {
            let slice = self.char_slice(f, element)?;
            return Some(Value::Str(slice));
        }
        if f.is_wchar {
            let slice = self.wchar_slice(f, element)?;
            return Some(Value::Str(slice));
        }
        if f.is_float {
            let raw = self.element_bytes(f, element)?;
            return Some(match f.elem_size {
                4 => Value::Float(f32::from_le_bytes(raw.try_into().ok()?) as f64),
                _ => Value::Float(f64::from_le_bytes(raw.try_into().ok()?)),
            });
        }
        let raw = self.element_bytes(f, element)?;
        let mut buf = [0u8; 8];
        buf[..f.elem_size].copy_from_slice(&raw);
        let unsigned = u64::from_le_bytes(buf);
        let value = if f.signed {
            sign_extend(unsigned, f.elem_size)
        } else {
            unsigned as i64
        };
        Some(Value::Int(value))
    }

    /// Write a field from an AutoIt value.
    pub fn set(&mut self, field: usize, element: Option<usize>, value: &Value) -> bool {
        let Some(f) = self.fields.get(field).cloned() else {
            return false;
        };
        if f.is_binary && f.count > 1 && element.is_none() {
            let bytes = match value {
                Value::Binary(b) => b.as_ref().clone(),
                other => other.to_autoit_string().into_bytes(),
            };
            let n = bytes.len().min(f.total_size());
            return self.write_at(f.offset, &bytes[..n]);
        }
        if f.is_char || f.is_wchar {
            return self.set_string(field, element, &value.to_autoit_string());
        }
        if f.is_float {
            let bytes: Vec<u8> = if f.elem_size == 4 {
                (value.to_f64() as f32).to_le_bytes().to_vec()
            } else {
                value.to_f64().to_le_bytes().to_vec()
            };
            return self.write_element(&f, element, &bytes);
        }
        let n = value.to_int();
        let bytes = n.to_le_bytes()[..f.elem_size].to_vec();
        self.write_element(&f, element, &bytes)
    }

    /// Write a string into a `char`/`wchar` field, NUL-terminating it.
    pub fn set_string(&mut self, field: usize, element: Option<usize>, text: &str) -> bool {
        let Some(f) = self.fields.get(field).cloned() else {
            return false;
        };
        if !(f.is_char || f.is_wchar) {
            // Setting a string on a numeric field coerces, like AutoIt.
            return self.set(field, element, &Value::Str(text.to_string()));
        }
        // An element write targets one slot; a whole-field write fills the
        // array and NUL-terminates.
        let slots = match element {
            Some(e) => {
                if e < 1 || e > f.count {
                    return false;
                }
                1
            }
            None => f.count,
        };
        let start = f.offset + element.map(|e| (e - 1) * f.elem_size).unwrap_or(0);
        if f.is_wchar {
            let units: Vec<u16> = text.encode_utf16().collect();
            for slot in 0..slots {
                let unit = units.get(slot).copied().unwrap_or(0);
                self.write_at(start + slot * 2, &unit.to_le_bytes());
            }
        } else {
            let bytes = text.as_bytes();
            for slot in 0..slots {
                let b = bytes.get(slot).copied().unwrap_or(0);
                self.write_at(start + slot, &[b]);
            }
        }
        true
    }

    /// Write an integer field directly, bypassing AutoIt coercion.
    pub fn set_int(&mut self, field: usize, value: u64) -> bool {
        let Some(f) = self.fields.get(field).cloned() else {
            return false;
        };
        let bytes = value.to_le_bytes()[..f.elem_size.min(8)].to_vec();
        self.write_element(&f, None, &bytes)
    }

    /// The field's declared name (`""` when unnamed).
    pub fn field_name(&self, field: usize) -> Option<&str> {
        self.fields.get(field).map(|f| f.name.as_str())
    }

    // ----- internals -----

    /// Read `len` bytes at `at` (struct-relative).
    fn read(&self, at: usize, len: usize) -> Option<Vec<u8>> {
        if at + len > self.size {
            return None;
        }
        let start = self.offset + at;
        let data = self.data.borrow();
        data.get(start..start + len).map(<[u8]>::to_vec)
    }

    fn element_bytes(&self, f: &Field, element: Option<usize>) -> Option<Vec<u8>> {
        let index = match element {
            Some(e) if e >= 1 && e <= f.count => e - 1,
            Some(_) => return None,
            None => 0,
        };
        let at = f.offset + index * f.elem_size;
        self.read(at, f.elem_size)
    }

    fn char_slice(&self, f: &Field, element: Option<usize>) -> Option<String> {
        if let Some(e) = element {
            let raw = self.element_bytes(f, Some(e))?;
            return Some(String::from_utf8_lossy(&raw).into_owned());
        }
        let bytes = self.read(f.offset, f.total_size())?;
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    fn wchar_slice(&self, f: &Field, element: Option<usize>) -> Option<String> {
        if let Some(e) = element {
            let raw = self.element_bytes(f, Some(e))?;
            let unit = u16::from_le_bytes(raw.try_into().ok()?);
            return Some(String::from_utf16_lossy(&[unit]));
        }
        let bytes = self.read(f.offset, f.total_size())?;
        let units: Vec<u16> = (0..bytes.len() / 2)
            .map(|i| u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]))
            .collect();
        let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
    }

    fn write_element(&mut self, f: &Field, element: Option<usize>, bytes: &[u8]) -> bool {
        let index = match element {
            Some(e) if e >= 1 && e <= f.count => e - 1,
            Some(_) => return false,
            None => 0,
        };
        let at = f.offset + index * f.elem_size;
        let n = bytes.len().min(f.elem_size);
        self.write_at(at, &bytes[..n])
    }
}

/// How a script referred to a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldSelector {
    /// `DllStructGetData($t, 2)` — 1-based field index.
    Index(i64),
    /// `DllStructGetData($t, "BuildNumber")`.
    Name(String),
}

impl FieldSelector {
    /// Build a selector from an AutoIt argument: a string is a name, anything
    /// else is read as an index.
    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Str(s) => FieldSelector::Name(s.clone()),
            other => FieldSelector::Index(other.to_int()),
        }
    }
}

/// Parse a definition into laid-out fields.
fn parse_fields(definition: &str, arch: WindowsArch) -> Result<Vec<Field>, String> {
    let mut fields: Vec<Field> = Vec::new();
    let mut offset = 0usize;
    let mut next_align = 0usize;

    for raw in definition.split(';') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            // Wrappers carry no layout of their own; a nested struct is
            // flattened, which is what the version queries need.
            "struct" | "endstruct" => continue,
            _ => {}
        }
        if let Some(n) = lower.strip_prefix("align") {
            let n = n.trim().parse::<usize>().map_err(|_| {
                let token = format!("{token:?}");
                msg!("DllStruct: bad alignment in {token}", token = token)
            })?;
            next_align = n.min(8);
            continue;
        }

        let mut words = token.split_whitespace();
        let Some(first) = words.next() else { continue };
        let (type_name, type_count) = split_array(first);
        let second = words.next();
        let (name, count) = match second {
            Some(rest) => {
                let (n, c) = split_array(rest);
                // `wchar CSDVersion[128]`: the count sits on the name.
                (n.to_string(), c.or(type_count))
            }
            None => (String::new(), type_count),
        };
        // No brackets at all is one element; an explicit `[0]` really is zero
        // bytes long. Measured on the official x64 interpreter:
        // `DllStructGetSize(DllStructCreate("BYTE [0]"))` is 0, and a hashing
        // loop depends on it - the empty chunk it feeds a BCrypt context at EOF
        // must add nothing, or every digest comes out one null byte too long.
        let count = count.unwrap_or(1);
        // AutoIt's type keywords are case-insensitive: scripts write `BYTE`,
        // `BYTE [38]`, `ulong` and `ULong` interchangeably.
        let (elem_size, is_char, is_binary, is_wchar, is_float, signed) =
            type_info(&type_name.to_ascii_lowercase(), arch).ok_or_else(|| {
                let type_name = format!("{type_name:?}");
                let definition = format!("{definition:?}");
                msg!(
                    "DllStruct: unknown type {type_name} in {definition}",
                    type_name = type_name,
                    definition = definition
                )
            })?;

        // Natural alignment, capped the way Windows caps it at 8 bytes. An
        // explicit `align N` overrides it for the next field.
        let align = if next_align > 0 { next_align } else { elem_size.min(8) };
        next_align = 0;
        if align > 1 {
            offset = offset.div_ceil(align) * align;
        }

        fields.push(Field {
            name,
            elem_size,
            count,
            offset,
            is_char,
            is_binary,
            is_wchar,
            is_float,
            signed,
        });
        offset += elem_size * count;
    }

    if fields.is_empty() {
        let definition = format!("{definition:?}");
        return Err(msg!(
            "DllStruct: {definition} declares no fields",
            definition = definition
        ));
    }
    Ok(fields)
}

/// Split `name[count]` into its parts; the count is `None` when the token
/// carries no brackets at all, which is not the same as an explicit `[0]`.
fn split_array(token: &str) -> (String, Option<usize>) {
    match token.split_once('[') {
        Some((name, rest)) => {
            let count = rest
                .trim_end_matches(']')
                .trim()
                .parse::<usize>()
                .ok();
            (name.to_string(), count)
        }
        None => (token.to_string(), None),
    }
}

/// `(element size, char, binary, wchar, float, signed)` for a type keyword.
fn type_info(
    type_name: &str,
    arch: WindowsArch,
) -> Option<(usize, bool, bool, bool, bool, bool)> {
    let info = match type_name {
        // `char` arrays are strings; `byte`/`ubyte` arrays are binaries.
        "char" => (1, true, false, false, false, false),
        "byte" | "ubyte" | "boolean" => (1, false, true, false, false, false),
        "wchar" => (2, false, false, true, false, false),
        "short" => (2, false, false, false, false, true),
        "ushort" | "word" => (2, false, false, false, false, false),
        "int" | "long" => (4, false, false, false, false, true),
        "uint" | "ulong" | "dword" => (4, false, false, false, false, false),
        "int64" => (8, false, false, false, false, true),
        "uint64" => (8, false, false, false, false, false),
        "ptr" | "handle" | "hwnd" => {
            (arch.pointer_size(), false, false, false, false, false)
        }
        "float" => (4, false, false, false, true, true),
        "double" => (8, false, false, false, true, true),
        "bool" => (4, false, false, false, false, false),
        _ => return None,
    };
    Some(info)
}

/// Sign-extend the low `size` bytes of `value`.
fn sign_extend(value: u64, size: usize) -> i64 {
    if size >= 8 {
        return value as i64;
    }
    let bits = size * 8;
    let sign = 1u64 << (bits - 1);
    if value & sign != 0 {
        (value | !((1u64 << bits) - 1)) as i64
    } else {
        value as i64
    }
}
