//! The DllStruct/address model, and the buffer helpers built on it.
//!
//! A `DllStruct` has a handle *and* an address, and a script mixes the two
//! (`DllStructGetPtr` hands the address to `DllCall`), so both resolve here.

use std::rc::Rc;

use autoitv3_runtime::value::Value;

use crate::winfmt::DllStruct;

use super::WindowsEmulation;
use std::cell::RefCell;

impl WindowsEmulation {
    /// Hand out a synthetic address for `len` bytes.
    ///
    /// Only the emulation resolves these: they are handed to the script by
    /// `DllStructGetPtr` / `LockResource` and consumed by `RtlMoveMemory`.
    pub(in crate::winemu) fn allocate(&mut self, len: usize) -> u64 {
        let base = self.next_addr;
        self.next_addr = base + ((len as u64 + 0xFFF) & !0xFFF).max(0x1000);
        base
    }

    /// Resolve a `DllStruct` by its handle *or* by an address it was given.
    pub(in crate::winemu) fn struct_any_mut(&mut self, value: i64) -> Option<&mut DllStruct> {
        if value >= 1 {
            let index = value as usize - 1;
            if self.structs.get(index).is_some_and(|s| s.is_some()) {
                return self.structs.get_mut(index)?.as_mut();
            }
        }
        let addr = value as u64;
        let found = self.structs.iter().position(|slot| {
            slot.as_ref().is_some_and(|s| {
                s.address() != 0 && addr >= s.address() && addr < s.address() + s.size() as u64
            })
        })?;
        self.structs.get_mut(found)?.as_mut()
    }

    /// Read `len` bytes at an emulated address.
    pub(in crate::winemu) fn memory_read(&self, addr: u64, len: usize) -> Option<Vec<u8>> {
        for s in self.structs.iter().flatten() {
            let (base, size) = (s.address(), s.size() as u64);
            if base != 0 && addr >= base && addr + len as u64 <= base + size {
                let start = (addr - base) as usize;
                return Some(s.bytes()[start..start + len].to_vec());
            }
        }
        for (base, data) in &self.blobs {
            let data = data.borrow();
            if addr >= *base && addr + len as u64 <= *base + data.len() as u64 {
                let start = (addr - *base) as usize;
                return Some(data[start..start + len].to_vec());
            }
        }
        None
    }

    /// The shared storage an emulated address points into, with the offset of
    /// that address inside it. `DllStructCreate($def, $ptr)` maps onto this
    /// instead of allocating, so the two views stay aliases of one another.
    pub(in crate::winemu) fn memory_storage(&self, addr: u64) -> Option<(Rc<RefCell<Vec<u8>>>, usize, u64)> {
        for s in self.structs.iter().flatten() {
            let (base, size) = (s.address(), s.size() as u64);
            if base != 0 && addr >= base && addr < base + size {
                let (storage, offset) = s.storage();
                return Some((storage, offset + (addr - base) as usize, addr));
            }
        }
        for (base, data) in &self.blobs {
            let len = data.borrow().len() as u64;
            if addr >= *base && addr < *base + len {
                return Some((Rc::clone(data), (addr - *base) as usize, addr));
            }
        }
        None
    }

    /// Write `bytes` at an emulated address.
    pub(in crate::winemu) fn memory_write(&mut self, addr: u64, bytes: &[u8]) -> bool {
        for s in self.structs.iter_mut().flatten() {
            let (base, size) = (s.address(), s.size() as u64);
            if base != 0 && addr >= base && addr + bytes.len() as u64 <= base + size {
                return s.write_at((addr - base) as usize, bytes);
            }
        }
        for (base, data) in &mut self.blobs {
            let len = data.borrow().len() as u64;
            if addr >= *base && addr + bytes.len() as u64 <= *base + len {
                let start = (addr - *base) as usize;
                data.borrow_mut()[start..start + bytes.len()].copy_from_slice(bytes);
                return true;
            }
        }
        false
    }

    /// The name `open_dll` handed `handle` out for.
    pub(in crate::winemu) fn dll_name(&self, handle: i64) -> Option<&str> {
        self.dlls
            .get(handle as usize - 1)?
            .as_ref()
            .map(String::as_str)
    }

    /// Resolve a string argument that is either a literal (`"kernel32.dll"`)
    /// or a pointer into emulated memory (`DllStructGetPtr`).
    pub(in crate::winemu) fn c_string_arg(&self, value: Value) -> Option<String> {
        match value {
            Value::Str(text) => Some(text),
            Value::Int(addr) | Value::Ptr(addr) => self.c_string_at(addr as u64, true),
            _ => None,
        }
    }

    /// Read a NUL-terminated string at an emulated address.
    ///
    /// Unit-by-unit, with the storage bounds as the hard stop: a short
    /// `DllStruct` buffer is a valid string home, and emulated memory has no
    /// pages to over-read into.
    pub(in crate::winemu) fn c_string_at(&self, addr: u64, wide: bool) -> Option<String> {
        let mut out = String::new();
        let mut at = addr;
        if wide {
            loop {
                let b = self.memory_read(at, 2)?;
                let unit = u16::from_le_bytes([b[0], b[1]]);
                if unit == 0 {
                    return Some(out);
                }
                out.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
                at += 2;
            }
        } else {
            loop {
                let b = self.memory_read(at, 1)?;
                if b[0] == 0 {
                    return Some(out);
                }
                out.push(b[0] as char);
                at += 1;
            }
        }
    }

    /// Write a NUL-terminated string into emulated memory; `max` bounds the
    /// byte footprint (buffer size semantics).
    pub(in crate::winemu) fn write_c_string(&mut self, addr: u64, text: &str, wide: bool, max: usize) -> bool {
        let bytes: Vec<u8> = if wide {
            text.encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(|u| u.to_le_bytes())
                .take(max)
                .collect()
        } else {
            text.bytes()
                .chain(std::iter::once(0))
                .take(max)
                .collect()
        };
        self.memory_write(addr, &bytes)
    }

    pub(in crate::winemu) fn push_struct(&mut self, s: DllStruct) -> i64 {
        // Reuse a freed slot so handles stay small, like the file table.
        if let Some(i) = self.structs.iter().position(|slot| slot.is_none()) {
            self.structs[i] = Some(s);
            return i as i64 + 1;
        }
        self.structs.push(Some(s));
        self.structs.len() as i64
    }

    pub(in crate::winemu) fn struct_mut(&mut self, handle: i64) -> Option<&mut DllStruct> {
        if handle < 1 {
            return None;
        }
        self.structs.get_mut(handle as usize - 1)?.as_mut()
    }

    pub(in crate::winemu) fn struct_ref(&self, handle: i64) -> Option<&DllStruct> {
        if handle < 1 {
            return None;
        }
        self.structs.get(handle as usize - 1)?.as_ref()
    }

    /// Read up to `len` bytes from whatever a `DllCall` argument names: a
    /// `DllStruct` handle (AutoIt's `struct*`), an emulated address, a binary
    /// value, or a string.
    pub(in crate::winemu) fn read_buffer(&self, value: &Value, len: usize) -> Option<Vec<u8>> {
        match value {
            Value::Binary(bytes) => {
                let mut out = bytes.as_ref().clone();
                out.truncate(if len == 0 { out.len() } else { len });
                Some(out)
            }
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            _ => {
                let handle = value.to_int();
                let bytes = self.struct_ref(handle).map(|s| s.bytes())?;
                let take = if len == 0 { bytes.len() } else { len.min(bytes.len()) };
                Some(bytes[..take].to_vec())
            }
        }
    }

    /// Bytes for a `DllCall` argument that names a buffer.
    ///
    /// Unlike [`WindowsEmulation::read_buffer`] this also resolves a raw
    /// address, which is what the crypto calls pass: the scripts hand CNG a
    /// `PTR` from `DllStructGetPtr`, not the struct handle.
    pub(in crate::winemu) fn dll_bytes(&self, value: &Value, len: usize) -> Option<Vec<u8>> {
        match value {
            Value::Binary(_) | Value::Str(_) | Value::Null => self.read_buffer(value, len),
            _ => {
                let raw = value.to_int();
                if raw == 0 {
                    return None;
                }
                if let Some(bytes) = self.read_buffer(value, len) {
                    return Some(bytes);
                }
                self.memory_read(raw as u64, len)
            }
        }
    }

    /// Write `bytes` into whatever a `DllCall` argument names.
    pub(in crate::winemu) fn write_buffer(&mut self, value: &Value, bytes: &[u8]) -> bool {
        let handle = value.to_int();
        if let Some(s) = self.struct_any_mut(handle) {
            let n = bytes.len().min(s.size());
            return s.write_all(&bytes[..n]);
        }
        self.memory_write(handle as u64, bytes)
    }
}
