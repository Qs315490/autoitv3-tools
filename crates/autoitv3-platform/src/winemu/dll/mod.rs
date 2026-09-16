//! `DllCall`, emulated: the name dispatch and the result shape.
//!
//! AutoIt hands back `[retval, arg1, arg2, ...]`, so an emulated call reports
//! its by-ref results the same way. Which function does what is decided here;
//! the pointer/DllStruct machinery lives in [`memory`], the CryptoAPI bridge in
//! [`crypto`] and the version/system structs in [`system`].

mod crypto;
mod memory;
mod system;

use std::rc::Rc;

use autoitv3_i18n::msg;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use super::WindowsEmulation;
use super::*;

/// What an emulated `DllCall` produced.
///
/// AutoIt's `DllCall` returns an array: element 0 is the function's return
/// value and elements 1..n are the arguments (the obfuscator reads its
/// out-parameters straight out of there — `$r[5]` for the fifth argument), so
/// an emulated call reports its by-ref results the same way.
pub(in crate::winemu) struct DllOutcome {
    retval: Value,
    /// `(argument index, value after the call)`.
    writes: Vec<(usize, Value)>,
}

impl DllOutcome {
    /// A call with no by-ref results.
    pub(in crate::winemu) fn value(retval: Value) -> Self {
        Self {
            retval,
            writes: Vec::new(),
        }
    }

    /// A call that wrote `value` into argument `index`.
    pub(in crate::winemu) fn with(retval: Value, index: usize, value: Value) -> Self {
        Self {
            retval,
            writes: vec![(index, value)],
        }
    }
}

impl WindowsEmulation {
    /// Emulate `DllCall(dll, rettype, function, type1, arg1, ...)`.
    pub(in crate::winemu) fn dll_call(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let function = arg_str(args, 2);
        // The tail is `type, value, type, value, ...`.
        let pairs: Vec<(String, Value)> = args
            .get(3..)
            .unwrap_or(&[])
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| (pair[0].to_autoit_string(), pair[1].clone()))
            .collect();

        let outcome = self.dll_call_inner(&function, &pairs).or_else(|| {
            // AutoIt resolves an unsuffixed name to its ANSI variant
            // (`MessageBox` → `MessageBoxA`), so an arm written for `...A`
            // must also answer the bare name.
            let lower = function.trim().to_ascii_lowercase();
            if lower.is_empty() || lower.ends_with('a') || lower.ends_with('w') {
                return None;
            }
            self.dll_call_inner(&format!("{lower}A"), &pairs)
        });

        match outcome {
            Some(out) => {
                ctx.set_error(0, 0);
                // `[return value, arg1, arg2, ...]`, as AutoIt hands it back.
                let mut result = Vec::with_capacity(pairs.len() + 1);
                result.push(out.retval);
                for (i, (_, value)) in pairs.iter().enumerate() {
                    let updated = out
                        .writes
                        .iter()
                        .find(|(index, _)| *index == i)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| value.clone());
                    result.push(updated);
                }
                Value::array(result)
            }
            // Not emulated: hand control back to the script's error handling
            // rather than inventing a result. AutoIt returns 0 (not an array)
            // when a call fails, and scripts test `@error` first.
            None => {
                if self.trace_dll && self.traced.insert(function.to_ascii_lowercase()) {
                    let dll = arg_str(args, 0);
                    eprintln!(
                        "{}",
                        msg!(
                            "[winemu] DllCall not emulated: {dll}!{function}",
                            dll = dll,
                            function = function
                        )
                    );
                }
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    /// Dispatch one emulated `DllCall`.
    pub(in crate::winemu) fn dll_call_inner(
        &mut self,
        function: &str,
        pairs: &[(String, Value)],
    ) -> Option<DllOutcome> {
        let arg = |i: usize| pairs.get(i).map(|(_, v)| v.clone());
        let values: Vec<Value> = pairs.iter().map(|(_, v)| v.clone()).collect();
        // Arms that branch on A/W suffixes compare against the lower-cased
        // name, not the caller's spelling.
        let lower = function.to_ascii_lowercase();
        let wide_name = lower.ends_with('w');
        match lower.as_str() {
            "getversionexw" | "getversionexa" | "rtlgetversion" => {
                Some(DllOutcome::value(self.fill_version_struct(&values)?))
            }
            "getversion" => Some(DllOutcome::value(Value::Int(
                self.version.packed_get_version() as i64,
            ))),
            "getsysteminfo" | "getnativesysteminfo" => {
                Some(DllOutcome::value(self.fill_system_info(&values)?))
            }
            "getlasterror" | "setlasterror" => Some(DllOutcome::value(Value::Int(0))),
            "getcurrentprocessid" | "getcurrentthreadid" => {
                Some(DllOutcome::value(Value::Int(std::process::id() as i64)))
            }
            "getcurrentprocess" => Some(DllOutcome::value(Value::Int(-1))),
            "gettickcount" | "gettickcount64" => Some(DllOutcome::value(Value::Int(
                self.origin.elapsed().as_millis() as i64,
            ))),
            // "Is this pointer bad?" — our addresses only ever name buffers the
            // emulation allocated, so the honest answer is "no".
            "isbadreadptr" | "isbadwriteptr" => Some(DllOutcome::value(Value::Int(0))),

            // ---------------- module resources ----------------
            "getmodulehandlew" | "getmodulehandlea" => {
                // Either source can answer `FindResourceW`: the image, or
                // resources already extracted next to the script. Failing
                // here when neither exists keeps the boundary visible.
                if self.module.is_none() && self.resource_dirs.is_empty() {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(EMULATED_IMAGE_BASE)))
            }
            "findresourcew" | "findresourcea" => {
                let name = resource_selector(arg(1).as_ref())?;
                let kind = resource_selector(arg(2).as_ref())?;
                // Files win over the image — the script's own `_Res_File_Add`
                // table first, then the resources extracted next to it: an
                // analysis usually has the payload and not the `.exe` it came
                // from, and looking there first is what makes that work.
                let (data, from_file) = self.read_resource(&name, &kind)?;
                if from_file && self.trace_dll {
                    eprintln!(
                        "{}",
                        msg!(
                            "[winemu] resource {name} from file",
                            name = name.name.as_deref().unwrap_or("?")
                        )
                    );
                }
                self.handles.push(Some(ResourceHandle {
                    data,
                    address: None,
                }));
                Some(DllOutcome::value(Value::Int(self.handles.len() as i64)))
            }
            "sizeofresource" => {
                let handle = arg(1).map(|v| v.to_int()).unwrap_or(0);
                let size = self.resource(handle)?.data.len() as i64;
                Some(DllOutcome::value(Value::Int(size)))
            }
            "loadresource" => {
                let handle = arg(1).map(|v| v.to_int()).unwrap_or(0);
                self.resource(handle)?;
                Some(DllOutcome::value(Value::Int(handle)))
            }
            "lockresource" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                Some(DllOutcome::value(self.lock_resource(handle)?))
            }
            "rtlmovememory" | "copymemory" => {
                let dest = arg(0).map(|v| v.to_int()).unwrap_or(0) as u64;
                let src = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let len = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                let bytes = self.memory_read(src, len)?;
                if !self.memory_write(dest, &bytes) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(0)))
            }

            // ---------------- modules ----------------
            "loadlibraryw" | "loadlibrarya" => {
                let name = self.c_string_arg(arg(0)?)?;
                Some(DllOutcome::value(Value::Int(self.open_dll(&name))))
            }
            "getprocaddress" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                if self.dll_name(handle).is_none() {
                    return None;
                }
                // A non-zero pseudo address: enough for a script to detect
                // "the export exists" and to compare two exports.
                Some(DllOutcome::value(Value::Int(self.allocate(0x10) as i64)))
            }
            "getmodulefilenamew" | "getmodulefilenamea" => {
                let wide = wide_name;
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let name = self
                    .dll_name(handle)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "emulated.dll".to_string());
                let full = format!("{}\\{}", self.paths.windows_dir, name);
                let buf = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let size = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                if size == 0 || !self.write_c_string(buf, &full, wide, size) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(full.len() as i64)))
            }

            // ---------------- memory ----------------
            "virtualalloc" | "virtualallocex" | "heapalloc" => {
                let size = arg(1).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                if size == 0 {
                    return None;
                }
                let addr = self.allocate(size);
                self.blobs
                    .push((addr, Rc::new(RefCell::new(vec![0u8; size]))));
                Some(DllOutcome::value(Value::Int(addr as i64)))
            }
            "virtualfree" | "heapfree" => Some(DllOutcome::value(Value::Bool(true))),
            "getprocessheap" => Some(DllOutcome::value(Value::Int(0x1))),

            // ---------------- sandboxed files ----------------
            "createfilew" | "createfilea" => {
                let path = self.c_string_arg(arg(0)?)?;
                let access = arg(1).map(|v| v.to_int()).unwrap_or(0);
                let write = access & 0x4000_0000 != 0; // GENERIC_WRITE
                let read = access & 0x8000_0000 != 0 || !write; // GENERIC_READ
                if !read && !write {
                    return None;
                }
                let handle = self.next_file_handle;
                self.next_file_handle += 1;
                let content = if write {
                    Vec::new()
                } else {
                    match self.sandbox_files.get(&normalise_sandbox_path(&path)) {
                        Some(c) => c.clone(),
                        None => return None, // file not found
                    }
                };
                self.open_files
                    .insert(handle, OpenFile { path, content, write, pos: 0 });
                Some(DllOutcome::value(Value::Int(handle)))
            }
            "readfile" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let buf = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let count = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                let lpread = arg(3).map(|v| v.to_int()).unwrap_or(0) as u64;
                let file = self.open_files.get_mut(&handle)?;
                if file.write {
                    return None;
                }
                let start = (file.pos as usize).min(file.content.len());
                let end = (start + count).min(file.content.len());
                let bytes = file.content[start..end].to_vec();
                file.pos = end as u64;
                if !self.memory_write(buf, &bytes) {
                    return None;
                }
                if lpread != 0 {
                    self.memory_write(lpread, &(bytes.len() as u32).to_le_bytes());
                }
                Some(DllOutcome::value(Value::Bool(true)))
            }
            "writefile" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let buf = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let count = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                let lpwritten = arg(3).map(|v| v.to_int()).unwrap_or(0) as u64;
                let bytes = self.memory_read(buf, count)?;
                let file = self.open_files.get_mut(&handle)?;
                if !file.write {
                    return None;
                }
                file.content.extend_from_slice(&bytes);
                if lpwritten != 0 {
                    self.memory_write(lpwritten, &(bytes.len() as u32).to_le_bytes());
                }
                Some(DllOutcome::value(Value::Bool(true)))
            }
            "getfilesize" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let len = self
                    .open_files
                    .get(&handle)
                    .map(|f| f.content.len() as i64)
                    .unwrap_or(-1);
                if len < 0 {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(len)))
            }
            "closehandle" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                match self.open_files.remove(&handle) {
                    Some(f) => {
                        if f.write {
                            self.sandbox_files
                                .insert(normalise_sandbox_path(&f.path), f.content);
                        }
                        Some(DllOutcome::value(Value::Bool(true)))
                    }
                    None => Some(DllOutcome::value(Value::Bool(true))),
                }
            }

            // ---------------- CRT-style strings ----------------
            "lstrlenw" | "lstrlena" => {
                let s = self.c_string_arg(arg(0)?)?;
                Some(DllOutcome::value(Value::Int(s.chars().count() as i64)))
            }
            "lstrcpyw" | "lstrcpya" => {
                let dst = arg(0).map(|v| v.to_int()).unwrap_or(0) as u64;
                let src = self.c_string_arg(arg(1)?)?;
                let wide = wide_name;
                let max = if wide { (src.chars().count() + 1) * 2 } else { src.len() + 1 };
                if !self.write_c_string(dst, &src, wide, max) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(dst as i64)))
            }
            "lstrcatw" | "lstrcata" => {
                let dst = arg(0).map(|v| v.to_int()).unwrap_or(0) as u64;
                let tail = self.c_string_arg(arg(1)?)?;
                let wide = wide_name;
                let mut head = self.c_string_at(dst, wide)?;
                head.push_str(&tail);
                let max = if wide { (head.chars().count() + 1) * 2 } else { head.len() + 1 };
                if !self.write_c_string(dst, &head, wide, max) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(dst as i64)))
            }

            // ---------------- scripted enumeration ----------------
            "enumwindows" | "enumchildwindows" | "enumthreadwindows" => {
                let cb = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let lparam = arg(function.starts_with("enumthread") as usize + 1)
                    .map(|v| v.to_int())
                    .unwrap_or(0);
                let index = callback_index(cb)?;
                let name = self
                    .callbacks
                    .get(index)
                    .cloned()
                    .flatten()?;
                for hwnd in self.scripted_windows.clone() {
                    self.pending_callbacks
                        .push((name.clone(), vec![Value::Int(hwnd), Value::Int(lparam)]));
                }
                Some(DllOutcome::value(Value::Bool(true)))
            }

            // ---------------- CryptoAPI ----------------
            // ---------------- bcrypt.dll (CNG) ----------------
            "bcryptopenalgorithmprovider" => {
                let alg = arg_str(&values, 1);
                let hmac = arg_int(&values, 3) & 8 != 0; // BCRYPT_ALG_HANDLE_HMAC_FLAG
                let handle = self.bcrypt.open_provider(&alg, hmac)?;
                Some(DllOutcome::with(Value::Int(0), 0, Value::Int(handle)))
            }
            "bcryptclosealgorithmprovider" => {
                self.bcrypt.close_provider(arg_int(&values, 0));
                Some(DllOutcome::value(Value::Int(0)))
            }
            "bcryptgetproperty" => {
                let property = self
                    .bcrypt
                    .property(arg_int(&values, 0), &arg_str(&values, 1))?;
                let bytes = property.bytes();
                let out = values.get(2).cloned().unwrap_or(Value::Null);
                if !is_null_ptr(&out) {
                    let cap = arg_int(&values, 3).max(0) as usize;
                    let take = if cap == 0 { bytes.len() } else { cap.min(bytes.len()) };
                    self.write_buffer(&out, &bytes[..take]);
                }
                Some(DllOutcome::with(Value::Int(0), 4, Value::Int(bytes.len() as i64)))
            }
            "bcryptsetproperty" => {
                let ok = self.bcrypt.set_property(
                    arg_int(&values, 0),
                    &arg_str(&values, 1),
                    &arg_str(&values, 2),
                );
                // An unknown property leaves the script's own error path to run.
                ok.then(|| DllOutcome::value(Value::Int(0)))
            }
            "bcryptcreatehash" => {
                let secret = if arg_int(&values, 4) != 0 {
                    let len = arg_int(&values, 5).max(0) as usize;
                    self.dll_bytes(&values[4], len)
                } else {
                    None
                };
                let handle = self
                    .bcrypt
                    .create_hash(arg_int(&values, 0), secret.as_deref())?;
                Some(DllOutcome::with(Value::Int(0), 1, Value::Int(handle)))
            }
            "bcrypthashdata" => {
                let len = arg_int(&values, 2).max(0) as usize;
                let data = self.dll_bytes(&values[1], len)?;
                if !self.bcrypt.hash_data(arg_int(&values, 0), &data) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(0)))
            }
            "bcryptfinishhash" => {
                let digest = self.bcrypt.finish_hash(arg_int(&values, 0))?;
                let cap = arg_int(&values, 2).max(0) as usize;
                // STATUS_BUFFER_TOO_SMALL, the way CNG reports a short buffer.
                if cap < digest.len() {
                    return Some(DllOutcome::value(Value::Int(0xC000_0023u32 as i64)));
                }
                self.write_buffer(&values[1], &digest);
                Some(DllOutcome::value(Value::Int(0)))
            }
            "bcryptdestroyhash" => {
                self.bcrypt.destroy_hash(arg_int(&values, 0));
                Some(DllOutcome::value(Value::Int(0)))
            }
            "bcryptgeneratesymmetrickey" => {
                // The secret is either its own argument or, as these scripts
                // pass it, the bytes of the key-object buffer.
                let secret = if arg_int(&values, 2) != 0 {
                    let len = arg_int(&values, 3).max(0) as usize;
                    self.dll_bytes(&values[2], len)
                } else {
                    let len = arg_int(&values, 5).max(0) as usize;
                    self.dll_bytes(&values[4], len)
                }?;
                let handle = self
                    .bcrypt
                    .generate_symmetric_key(arg_int(&values, 0), &secret)?;
                Some(DllOutcome::with(Value::Int(0), 1, Value::Int(handle)))
            }
            "bcryptdestroykey" => {
                self.bcrypt.destroy_key(arg_int(&values, 0));
                Some(DllOutcome::value(Value::Int(0)))
            }
            "bcryptencrypt" | "bcryptdecrypt" => {
                let encrypt = lower == "bcryptencrypt";
                let key = arg_int(&values, 0);
                let len = arg_int(&values, 2).max(0) as usize;
                let data = self.dll_bytes(&values[1], len)?;
                let iv_len = arg_int(&values, 5).max(0) as usize;
                let iv = if arg_int(&values, 4) != 0 {
                    self.dll_bytes(&values[4], iv_len)
                } else {
                    None
                };
                let flags = arg_int(&values, 9);
                let padding = flags & 1 != 0; // BCRYPT_BLOCK_PADDING
                let out = values.get(6).cloned().unwrap_or(Value::Null);
                // A null output buffer asks "how much would this take?".
                if is_null_ptr(&out) {
                    let size = self.bcrypt.crypt_output_len(key, data.len(), padding)?;
                    return Some(DllOutcome::with(Value::Int(0), 8, Value::Int(size as i64)));
                }
                let result = self.bcrypt.crypt(key, &data, iv.as_deref(), encrypt, padding)?;
                let cap = arg_int(&values, 7).max(0) as usize;
                let take = if cap == 0 { result.len() } else { cap.min(result.len()) };
                self.write_buffer(&out, &result[..take]);
                Some(DllOutcome::with(Value::Int(0), 8, Value::Int(take as i64)))
            }
            "bcryptderivekeypbkdf2" => {
                let pw_len = arg_int(&values, 2).max(0) as usize;
                let salt_len = arg_int(&values, 4).max(0) as usize;
                let out_len = arg_int(&values, 7).max(0) as usize;
                let password = self.dll_bytes(&values[1], pw_len)?;
                let salt = self.dll_bytes(&values[3], salt_len)?;
                let iterations = arg_int(&values, 5).max(0) as u64;
                let derived = self.bcrypt.derive_pbkdf2(
                    arg_int(&values, 0),
                    &password,
                    &salt,
                    iterations,
                    out_len,
                )?;
                self.write_buffer(&values[6], &derived);
                Some(DllOutcome::value(Value::Int(0)))
            }
            "bcryptgenrandom" => {
                let len = arg_int(&values, 2).max(0) as usize;
                let bytes = self.bcrypt.random(len);
                self.write_buffer(&values[1], &bytes);
                Some(DllOutcome::value(Value::Int(0)))
            }
            "bcryptimportkeypair" => {
                let blob_type = arg_str(&values, 2);
                if !blob_type.eq_ignore_ascii_case("RSAPRIVATEBLOB")
                    && !blob_type.eq_ignore_ascii_case("BCRYPT_RSAPRIVATE_BLOB")
                {
                    return None;
                }
                let len = arg_int(&values, 5).max(0) as usize;
                let blob = self.dll_bytes(&values[4], len)?;
                let handle = self
                    .bcrypt
                    .import_rsa_private_key(arg_int(&values, 0), &blob)?;
                Some(DllOutcome::with(Value::Int(0), 3, Value::Int(handle)))
            }
            "cryptacquirecontexta" | "cryptacquirecontextw" => {
                Some(DllOutcome::with(
                    Value::Bool(true),
                    0,
                    Value::Int(0x0c00_0001),
                ))
            }
            "cryptreleasecontext" => Some(DllOutcome::value(Value::Bool(true))),
            "cryptcreatehash" => self.crypt_create_hash(&arg(1)?),
            "crypthashdata" => self.crypt_hash_data(&arg(0)?, &arg(1)?, &arg(2)?),
            "cryptgethashparam" => self.crypt_get_hash_param(&arg(0)?, &arg(1)?, &arg(2)?),
            "cryptderivekey" => self.crypt_derive_key(&arg(1)?, &arg(2)?),
            "cryptdecrypt" => {
                let final_block = arg(2).map(|v| v.is_truthy()).unwrap_or(false);
                self.crypt_decrypt(&arg(0)?, &arg(4)?, &arg(5)?, final_block)
            }
            // KP_IV = 7: the IV the derived key already carries.
            "cryptgetkeyparam" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                let key = self.crypto.keys.get(index)?.as_ref()?.clone();
                match arg(1)?.to_int() {
                    7 => {
                        self.write_buffer(&arg(2)?, &key.iv);
                        Some(DllOutcome::with(
                            Value::Bool(true),
                            2,
                            Value::Binary(std::rc::Rc::new(key.iv)),
                        ))
                    }
                    _ => None,
                }
            }
            "cryptsetkeyparam" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                let value = self.read_buffer(&arg(2)?, 0)?;
                if arg(1)?.to_int() == 7 {
                    if let Some(slot) = self.crypto.keys.get_mut(index).and_then(|k| k.as_mut()) {
                        slot.iv = value;
                    }
                    Some(DllOutcome::value(Value::Bool(true)))
                } else {
                    None
                }
            }
            "cryptdestroyhash" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                *self.crypto.hashes.get_mut(index)? = None;
                Some(DllOutcome::value(Value::Bool(true)))
            }
            "cryptdestroykey" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                *self.crypto.keys.get_mut(index)? = None;
                Some(DllOutcome::value(Value::Bool(true)))
            }

            // ---------------- checksums ----------------
            // `RtlComputeCrc32(initial, data, len)`: scripts fold a digest
            // through it to get the short check value a data file advertises.
            "rtlcomputecrc32" => {
                let initial = arg(0)?.to_int() as u32;
                let len = arg(2)?.to_int().max(0) as usize;
                let data = self.dll_bytes(&arg(1)?, len)?;
                Some(DllOutcome::value(Value::Int(i64::from(super::crypto::crc32(
                    initial, &data,
                )))))
            }

            // ---------------- LZNT1 ----------------
            "rtlgetcompressionworkspacesize" => {
                Some(DllOutcome::with(Value::Int(0), 1, Value::Int(0)))
            }
            "rtldecompressbuffer" => {
                self.rtl_decompress_buffer(&arg(0)?, &arg(1)?, &arg(2)?, &arg(3)?, &arg(4)?)
            }
            _ => None,
        }
    }
}
