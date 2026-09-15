//! Pseudo-COM objects: `Scripting.Dictionary`, `WScript.Shell` and
//! `Scripting.FileSystemObject`, emulated.
//!
//! A real COM runtime is out of scope, but samples reach for these three
//! ProgIDs through `ObjCreate`, so they are modelled as plain objects over a
//! small member table. The `obj_get`/`obj_call` trait methods stay in
//! `mod.rs` (a trait impl cannot be split) and delegate to these.

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use super::WindowsEmulation;
use super::*;
use std::collections::BTreeMap;

/// One live pseudo-COM object (`ObjCreate` on the emulation).
pub(in crate::winemu) struct PseudoObject {
    kind: PseudoKind,
    /// `Scripting.Dictionary` storage (case-insensitive keys).
    dict: BTreeMap<String, Value>,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::winemu) enum PseudoKind {
    Dictionary,
    WScriptShell,
    FileSystemObject,
}

impl PseudoKind {
    /// The ProgIDs the emulation answers.
    pub(in crate::winemu) fn from_progid(progid: &str) -> Option<PseudoKind> {
        let p = progid.trim().to_ascii_lowercase();
        Some(match p.as_str() {
            "scripting.dictionary" => PseudoKind::Dictionary,
            "wscript.shell" | "wscript.shell.1" => PseudoKind::WScriptShell,
            "scripting.filesystemobject" => PseudoKind::FileSystemObject,
            _ => return None,
        })
    }
}

impl WindowsEmulation {
    /// `ObjCreate` for a ProgID the emulation answers. `None` = this ProgID is
    /// not simulated (the honest `@error = 1` path).
    pub(in crate::winemu) fn pseudo_com_create(&mut self, progid: &str) -> Option<usize> {
        let kind = PseudoKind::from_progid(progid)?;
        let object = PseudoObject {
            kind,
            dict: BTreeMap::new(),
        };
        if let Some(i) = self.pseudo_objects.iter().position(|slot| slot.is_none()) {
            self.pseudo_objects[i] = Some(object);
            return Some(i + 1);
        }
        self.pseudo_objects.push(Some(object));
        Some(self.pseudo_objects.len())
    }

    fn pseudo_object(&mut self, handle: usize) -> Option<&mut PseudoObject> {
        self.pseudo_objects
            .get_mut(handle.checked_sub(1)?)?
            .as_mut()
    }

    /// Property read on a pseudo object (`$o.Count`).
    pub(in crate::winemu) fn pseudo_com_get(&mut self, handle: usize, member: &str) -> Option<Result<Value, String>> {
        let kind = self
            .pseudo_objects
            .get(handle.checked_sub(1)?)?
            .as_ref()?
            .kind;
        match kind {
            PseudoKind::Dictionary => {
                if member.eq_ignore_ascii_case("count") {
                    let n = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .map(|o| o.dict.len())
                        .unwrap_or(0);
                    Some(Ok(Value::Int(n as i64)))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Method call on a pseudo object.
    pub(in crate::winemu) fn pseudo_com_call(
        &mut self,
        handle: usize,
        member: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Option<Result<Value, String>> {
        let kind = self
            .pseudo_objects
            .get(handle.checked_sub(1)?)?
            .as_ref()?
            .kind;
        let arg_str_at = |args: &[Value], i: usize| -> String {
            args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
        };
        match kind {
            PseudoKind::Dictionary => {
                let m = member.to_ascii_lowercase();
                if m == "add" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    let value = args.get(1).cloned().unwrap_or(Value::Null);
                    let object = self.pseudo_object(handle)?;
                    if object.dict.contains_key(&key) {
                        return Some(Err("This key is already associated with an element of this collection".into()));
                    }
                    object.dict.insert(key, value);
                    return Some(Ok(Value::Null));
                }
                if m == "exists" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    let hit = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .map(|o| o.dict.contains_key(&key))
                        .unwrap_or(false);
                    return Some(Ok(Value::Bool(hit)));
                }
                if m == "item" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    let value = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .and_then(|o| o.dict.get(&key).cloned())
                        .unwrap_or(Value::Null);
                    return Some(Ok(value));
                }
                if m == "remove" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    self.pseudo_object(handle)?.dict.remove(&key);
                    return Some(Ok(Value::Null));
                }
                if m == "removeall" {
                    self.pseudo_object(handle)?.dict.clear();
                    return Some(Ok(Value::Null));
                }
                if m == "keys" || m == "items" {
                    let list = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .map(|o| match m.as_str() {
                            "keys" => o.dict.keys().map(|k| Value::Str(k.clone())).collect(),
                            _ => o.dict.values().cloned().collect(),
                        })
                        .unwrap_or_default();
                    return Some(Ok(Value::array(list)));
                }
                None
            }
            PseudoKind::WScriptShell => {
                let m = member.to_ascii_lowercase();
                // WSH registry names carry the value in the path itself:
                // `HKCU\...\Value` addresses a value, a trailing `\` the
                // `(Default)` value of the key.
                let split_name = |full: &str| -> (String, String) {
                    match full.rfind('\\') {
                        Some(i) => (full[..i].to_string(), full[i + 1..].to_string()),
                        None => (full.to_string(), String::new()),
                    }
                };
                if m == "regread" {
                    let (key, value) = split_name(&arg_str_at(args, 0));
                    let data = self.registry.read(&key, &value)?;
                    return Some(Ok(Self::registry_data_to_value(&data)));
                }
                if m == "regwrite" {
                    if !ctx.effect_allowed(EffectKind::RegistryWrite) {
                        return Some(Err("@error (writes denied)".into()));
                    }
                    // RegWrite(Name, Value [, Type]).
                    let (key, value_name) = split_name(&arg_str_at(args, 0));
                    let value = args.get(1).cloned().unwrap_or(Value::Null);
                    let data = RegistryData::from_autoit(
                        args.get(2).and_then(reg_type_code),
                        &value,
                    )?;
                    let ok = self.registry.write(&key, &value_name, data);
                    return Some(Ok(Value::Bool(ok)));
                }
                if m == "regdelete" {
                    if !ctx.effect_allowed(EffectKind::RegistryWrite) {
                        return Some(Err("@error (writes denied)".into()));
                    }
                    let path = arg_str_at(args, 0);
                    let ok = if let Some(key) = path.strip_suffix('\\') {
                        self.registry.delete_key(key, true)
                    } else {
                        let (key, value) = split_name(&path);
                        self.registry.delete_value(&key, &value)
                    };
                    return Some(Ok(Value::Bool(ok)));
                }
                if m == "expandenvironmentstrings" {
                    let raw = arg_str_at(args, 0);
                    // Expand %VAR% against the *host* environment: reads only.
                    let mut out = String::new();
                    let mut rest = raw.as_str();
                    while let Some(start) = rest.find('%') {
                        out.push_str(&rest[..start]);
                        let tail = &rest[start + 1..];
                        match tail.find('%') {
                            Some(end) => {
                                let name = &tail[..end];
                                out.push_str(
                                    &host_env(name).unwrap_or_else(|| format!("%{name}%")),
                                );
                                rest = &tail[end + 1..];
                            }
                            None => {
                                out.push_str(tail);
                                break;
                            }
                        }
                    }
                    return Some(Ok(Value::Str(out)));
                }
                if m == "run" || m == "runwait" {
                    if !ctx.effect_allowed(EffectKind::Spawn) {
                        return Some(Err("@error (spawn denied)".into()));
                    }
                    let cmdline = arg_str_at(args, 0);
                    let mut tokens = cmdline.splitn(2, ' ');
                    let program = tokens.next().unwrap_or_default();
                    let params = tokens.next().unwrap_or_default();
                    match shell::spawn(program, params, "", 0) {
                        Ok(mut child) => {
                            let v = if m == "runwait" {
                                let _ = child.wait();
                                Value::Int(0)
                            } else {
                                Value::Int(child.id() as i64)
                            };
                            return Some(Ok(v));
                        }
                        Err(_) => return Some(Err("@error (spawn failed)".into())),
                    }
                }
                None
            }
            PseudoKind::FileSystemObject => {
                let m = member.to_ascii_lowercase();
                if m == "fileexists" {
                    let path = arg_str_at(args, 0);
                    let hit = Path::new(&path).exists()
                        || self
                            .sandbox_files
                            .contains_key(&normalise_sandbox_path(&path));
                    return Some(Ok(Value::Bool(hit)));
                }
                if m == "driveexists" {
                    let path = arg_str_at(args, 0);
                    let hit = self.find_drive(&path).is_some()
                        || Path::new(&format!("{}\\", path.trim())).exists();
                    return Some(Ok(Value::Bool(hit)));
                }
                if m == "getspecialfolder" {
                    let which = args.first().map(|v| v.to_int()).unwrap_or(0);
                    let dir = match which {
                        1 => self.paths.system_dir.clone(),
                        2 => self.paths.temp(),
                        _ => self.paths.windows_dir.clone(),
                    };
                    return Some(Ok(Value::Str(dir)));
                }
                if m == "gettempname" {
                    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Some(Ok(Value::Str(format!("au3{}.tmp", std::process::id() as usize + n))));
                }
                // Pure path arithmetic: GetExtensionName / GetBaseName /
                // GetFileName / GetParentFolderName.
                let path = arg_str_at(args, 0);
                let file_name = path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&path)
                    .to_string();
                let value = match m.as_str() {
                    "getfilename" => Some(file_name.clone()),
                    "getextensionname" => file_name
                        .rsplit_once('.')
                        .map(|(_, ext)| ext.to_string()),
                    "getbasename" => Some(
                        file_name
                            .rsplit_once('.')
                            .map(|(base, _)| base.to_string())
                            .unwrap_or_else(|| file_name.clone()),
                    ),
                    "getparentfoldername" => Some(
                        match path.rfind(['\\', '/']) {
                            Some(i) => path[..i].to_string(),
                            None => String::new(),
                        }
                    ),
                    _ => None,
                }?;
                Some(Ok(Value::Str(value)))
            }
        }
    }
}
