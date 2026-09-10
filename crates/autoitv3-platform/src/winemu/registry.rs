//! Registry emulation: the store interface, plus two ready-to-use stores.
//!
//! Off Windows there is no registry to call, so the `Reg*` builtins are
//! redirected to a [`RegistryStore`]. The trait is the extension point: an
//! embedder analysing a particular sample can plug in its own view (a snapshot
//! captured from a real machine, a fixture, a database) and the `RegRead` /
//! `RegWrite` / `RegDelete` / `RegEnum*` functions keep working unchanged.
//!
//! Two stores ship with the emulation:
//!
//! * [`FileRegistry`] — **the default**. It is a [`MemoryRegistry`] whose state
//!   is loaded from, and written back to, a text file (`.au3_registry` in the
//!   working directory unless `AU3_WIN_REGISTRY` says otherwise). A real
//!   machine's registry can be captured into that file and a script's writes
//!   survive the run.
//! * [`MemoryRegistry`] — the same store without the file, for tests and for
//!   callers that want the emulation to leave no trace on disk. Opt in with
//!   [`WindowsEmulation::with_memory_registry`](super::WindowsEmulation::with_memory_registry).
//!
//! Both store keys case-insensitively (as the registry does), understand the
//! usual hive aliases (`HKLM`, `HKCU`, ...), and `seeded` lays down the handful
//! of keys a Windows program usually consults — the version keys under
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`, the `Shell Folders`
//! paths, the session-manager environment — derived from the selected
//! [`WindowsVersion`] so registry and macros tell the same story.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use autoitv3_runtime::value::Value;

use super::paths::WindowsPaths;
use super::version::{WindowsArch, WindowsVersion};

/// A typed registry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryData {
    /// `REG_SZ`.
    Sz(String),
    /// `REG_EXPAND_SZ` — an unexpanded `%VAR%` string.
    ExpandSz(String),
    /// `REG_MULTI_SZ`.
    MultiSz(Vec<String>),
    /// `REG_DWORD`.
    Dword(u32),
    /// `REG_QWORD`.
    Qword(u64),
    /// `REG_BINARY`.
    Binary(Vec<u8>),
}

impl RegistryData {
    /// The Win32 type name, for diagnostics.
    pub fn type_name(&self) -> &'static str {
        match self {
            RegistryData::Sz(_) => "REG_SZ",
            RegistryData::ExpandSz(_) => "REG_EXPAND_SZ",
            RegistryData::MultiSz(_) => "REG_MULTI_SZ",
            RegistryData::Dword(_) => "REG_DWORD",
            RegistryData::Qword(_) => "REG_QWORD",
            RegistryData::Binary(_) => "REG_BINARY",
        }
    }

    /// The value as AutoIt's `RegRead` returns it.
    pub fn to_value(&self) -> Value {
        match self {
            RegistryData::Sz(s) | RegistryData::ExpandSz(s) => Value::Str(s.clone()),
            RegistryData::MultiSz(items) => {
                Value::array(items.iter().map(|s| Value::Str(s.clone())).collect())
            }
            RegistryData::Dword(v) => Value::Int(*v as i64),
            RegistryData::Qword(v) => Value::Int(*v as i64),
            RegistryData::Binary(bytes) => Value::Binary(std::rc::Rc::new(bytes.clone())),
        }
    }

    /// Interpret an AutoIt `RegWrite(key, value, type, data)` argument.
    ///
    /// `type_code` follows the `REG_*` constants AutoIt exposes; when it is
    /// omitted the value's own shape decides.
    pub fn from_autoit(type_code: Option<i64>, value: &Value) -> Option<RegistryData> {
        let data = match type_code {
            Some(1) => RegistryData::Sz(value.to_autoit_string()),
            Some(2) => RegistryData::ExpandSz(value.to_autoit_string()),
            Some(3) => RegistryData::Binary(binary_of(value)),
            Some(4) => RegistryData::Dword(value.to_int() as u32),
            Some(7) => RegistryData::MultiSz(strings_of(value)),
            Some(11) => RegistryData::Qword(value.to_int() as u64),
            Some(_) => return None,
            None => match value {
                Value::Binary(bytes) => RegistryData::Binary(bytes.as_ref().clone()),
                Value::Int(_) | Value::Float(_) | Value::Bool(_) => {
                    RegistryData::Dword(value.to_int() as u32)
                }
                Value::Array(_) => RegistryData::MultiSz(strings_of(value)),
                _ => RegistryData::Sz(value.to_autoit_string()),
            },
        };
        Some(data)
    }
}

/// The backing store behind the emulated registry functions.
///
/// Implementations are addressed by the same key strings a script writes:
/// `HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Product`, `HKCU\Environment`, and the
/// usual aliases. Value names are case-insensitive; the empty name is the
/// key's default value.
pub trait RegistryStore: std::fmt::Debug {
    /// Read `value` under `key`. `None` means the value (or key) is absent.
    fn read(&self, key: &str, value: &str) -> Option<RegistryData>;

    /// Write `value` under `key`, creating the key (and its parents) when
    /// needed, as `RegWrite` does. Returns whether the write happened.
    fn write(&mut self, key: &str, value: &str, data: RegistryData) -> bool;

    /// Remove one value. Returns whether it existed.
    fn delete_value(&mut self, key: &str, value: &str) -> bool;

    /// Remove a key. With `recurse` the whole subtree goes; otherwise the call
    /// fails when subkeys remain, matching `RegDeleteKey`.
    fn delete_key(&mut self, key: &str, recurse: bool) -> bool;

    /// Whether `key` exists, even if it holds no values.
    fn key_exists(&self, key: &str) -> bool;

    /// The immediate subkeys of `key`, in their stored spelling.
    fn enum_keys(&self, key: &str) -> Vec<String>;

    /// The value names directly under `key`.
    fn enum_values(&self, key: &str) -> Vec<String>;
}

/// The default in-memory [`RegistryStore`].
#[derive(Debug, Default, Clone)]
pub struct MemoryRegistry {
    /// Normalised key path → node.
    keys: BTreeMap<String, Node>,
    /// Normalised key path → the spelling to hand back when enumerating.
    display: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone)]
struct Node {
    /// Normalised value name → (display name, data).
    values: BTreeMap<String, (String, RegistryData)>,
}

impl MemoryRegistry {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store pre-populated with the keys a Windows program typically reads,
    /// consistent with `version`, `arch` and `paths`.
    pub fn seeded(version: WindowsVersion, arch: WindowsArch, paths: &WindowsPaths) -> Self {
        let mut reg = Self::new();
        seed(&mut reg, version, arch, paths);
        reg
    }

    /// Write a `REG_SZ`, creating keys as needed.
    pub fn set_sz(&mut self, key: &str, value: &str, data: &str) {
        self.write(key, value, RegistryData::Sz(data.to_string()));
    }

    /// Write a `REG_DWORD`, creating keys as needed.
    pub fn set_dword(&mut self, key: &str, value: &str, data: u32) {
        self.write(key, value, RegistryData::Dword(data));
    }

    /// Write a `REG_MULTI_SZ`, creating keys as needed.
    pub fn set_multi_sz(&mut self, key: &str, value: &str, data: &[&str]) {
        self.write(
            key,
            value,
            RegistryData::MultiSz(data.iter().map(|s| s.to_string()).collect()),
        );
    }

    /// Normalise a key and make sure it and its ancestors exist.
    ///
    /// The display path is built up component by component so every stored key
    /// has the spelling the caller used (`HKCU\Software\X` stays that way even
    /// though it is keyed internally as `HKEY_CURRENT_USER\SOFTWARE\X`).
    fn ensure_key(&mut self, raw: &str) -> String {
        let normalized = normalize_key(raw);
        if normalized.is_empty() {
            return normalized;
        }
        let original = raw.trim().trim_end_matches('\\');
        let original_parts: Vec<&str> = original.split('\\').collect();
        let mut acc = String::new();
        let mut shown = String::new();
        for (i, part) in normalized.split('\\').enumerate() {
            if !acc.is_empty() {
                acc.push('\\');
                shown.push('\\');
            }
            acc.push_str(part);
            shown.push_str(original_parts.get(i).copied().unwrap_or(part));
            self.keys.entry(acc.clone()).or_default();
            self.display.entry(acc.clone()).or_insert_with(|| shown.clone());
        }
        normalized
    }
}

impl RegistryStore for MemoryRegistry {
    fn read(&self, key: &str, value: &str) -> Option<RegistryData> {
        let node = self.keys.get(&normalize_key(key))?;
        node.values
            .get(&normalize_value(value))
            .map(|(_, data)| data.clone())
    }

    fn write(&mut self, key: &str, value: &str, data: RegistryData) -> bool {
        let normalized = self.ensure_key(key);
        if normalized.is_empty() {
            return false;
        }
        let Some(node) = self.keys.get_mut(&normalized) else {
            return false;
        };
        let name = value.trim().to_string();
        node.values
            .insert(normalize_value(value), (name, data));
        true
    }

    fn delete_value(&mut self, key: &str, value: &str) -> bool {
        let Some(node) = self.keys.get_mut(&normalize_key(key)) else {
            return false;
        };
        node.values.remove(&normalize_value(value)).is_some()
    }

    fn delete_key(&mut self, key: &str, recurse: bool) -> bool {
        let normalized = normalize_key(key);
        if !self.keys.contains_key(&normalized) {
            return false;
        }
        let child_prefix = format!("{normalized}\\");
        let has_children = self
            .keys
            .keys()
            .any(|k| k.starts_with(&child_prefix));
        if has_children && !recurse {
            return false;
        }
        self.keys
            .retain(|k, _| k != &normalized && !k.starts_with(&child_prefix));
        self.display
            .retain(|k, _| k != &normalized && !k.starts_with(&child_prefix));
        true
    }

    fn key_exists(&self, key: &str) -> bool {
        self.keys.contains_key(&normalize_key(key))
    }

    fn enum_keys(&self, key: &str) -> Vec<String> {
        let prefix = normalize_key(key);
        let child_prefix = format!("{prefix}\\");
        let mut out: Vec<String> = self
            .keys
            .keys()
            .filter_map(|k| {
                let rest = k.strip_prefix(&child_prefix)?;
                if rest.contains('\\') {
                    return None;
                }
                // Stored display paths are full paths; enumeration yields the
                // immediate component.
                let shown = self.display.get(k).map(String::as_str).unwrap_or(rest);
                Some(
                    shown
                        .rsplit('\\')
                        .next()
                        .unwrap_or(shown)
                        .to_string(),
                )
            })
            .collect();
        out.sort();
        out
    }

    fn enum_values(&self, key: &str) -> Vec<String> {
        let Some(node) = self.keys.get(&normalize_key(key)) else {
            return Vec::new();
        };
        let mut out: Vec<String> = node
            .values
            .values()
            .map(|(display, _)| display.clone())
            .collect();
        out.sort();
        out
    }
}

/// A [`RegistryStore`] whose state lives in a text file.
///
/// This is the default backing store. Reads are served from the in-memory copy
/// loaded at construction; every write is flushed back to the file, so a
/// script's `RegWrite` / `RegDelete` outlives the run and the emulated registry
/// can be inspected, edited, diffed or committed.
///
/// The file holds **records, not a dump of the seed**: the per-version seed is
/// laid down first and the file's records are overlaid on top, so dropping a
/// captured snapshot into the file is the intended way to give the emulation a
/// specific machine's registry while the standard keys remain available
/// underneath. Only the file's own records (and later writes) are written back,
/// which means selecting a different [`WindowsVersion`] still updates every key
/// the file does not mention. The one asymmetry is that deleting a *seeded*
/// value is not remembered across runs — there is no tombstone in the format.
///
/// The default path is `.au3_registry` in the working directory; set
/// `AU3_WIN_REGISTRY` or call
/// [`WindowsEmulation::with_registry_file`](super::WindowsEmulation::with_registry_file)
/// to point it elsewhere. A missing file is not an error — the store just
/// starts from its seed, and nothing is created until something is written.
///
/// # Format
///
/// Line-oriented UTF-8, one record per line, four tab-separated fields
/// (`key`, `value name`, `type`, `payload`):
///
/// ```text
/// # au3-registry v1
/// HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\t\tKEY\t
/// HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\tName\tREG_SZ\thello
/// HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\tCount\tREG_DWORD\t7
/// ```
///
/// `KEY` records a key that exists but holds no values; the remaining type
/// names are [`RegistryData`]'s. `\`, `|`, tab, CR, LF and NUL are escaped as
/// `\\`, `\|`, `\t`, `\r`, `\n` and `\0`, and `REG_MULTI_SZ` items are joined
/// with `|`, so every value fits on one line and no text can forge a record.
/// `#` starts a comment line.
#[derive(Debug, Clone)]
pub struct FileRegistry {
    /// The working view: the per-version seed with the file's records overlaid,
    /// plus everything written through this store.
    inner: MemoryRegistry,
    /// What gets written back: the file's records plus the writes. The seed is
    /// deliberately **not** copied out, so selecting a different
    /// [`WindowsVersion`] later re-seeds every key the file does not mention.
    stored: MemoryRegistry,
    path: PathBuf,
}

impl FileRegistry {
    /// An empty store that will persist to `path` (nothing is read yet).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: MemoryRegistry::new(),
            stored: MemoryRegistry::new(),
            path: path.into(),
        }
    }

    /// The per-version seed with the file (when it exists) overlaid on top.
    pub fn seeded(
        version: WindowsVersion,
        arch: WindowsArch,
        paths: &WindowsPaths,
        path: impl Into<PathBuf>,
    ) -> Self {
        let mut store = Self {
            inner: MemoryRegistry::seeded(version, arch, paths),
            stored: MemoryRegistry::new(),
            path: path.into(),
        };
        // A malformed/unreadable file must not take the emulation down; the
        // seed alone is still a usable registry.
        let _ = store.reload();
        store
    }

    /// The file this store persists to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The working in-memory view (seed + file + writes).
    pub fn memory(&self) -> &MemoryRegistry {
        &self.inner
    }

    /// Overlay the file's records on the current contents (the file wins),
    /// exactly as at construction. Missing file → no-op.
    pub fn reload(&mut self) -> io::Result<()> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.splitn(4, '\t');
            let key = unescape(fields.next().unwrap_or(""));
            let name = unescape(fields.next().unwrap_or(""));
            let type_token = fields.next().unwrap_or("");
            let payload = fields.next().unwrap_or("");
            if key.is_empty() {
                continue;
            }
            if type_token == KEY_RECORD {
                self.inner.ensure_key(&key);
                self.stored.ensure_key(&key);
                continue;
            }
            // Unknown types are ignored rather than fatal, so a file written by
            // a newer version still loads.
            if let Some(data) = parse_data(type_token, payload) {
                self.inner.write(&key, &name, data.clone());
                self.stored.write(&key, &name, data);
            }
        }
        Ok(())
    }

    /// Write the persisted records back to the file, creating parent
    /// directories.
    ///
    /// The write goes through a sibling temporary file and a rename, so a crash
    /// mid-write cannot leave a half-written registry behind.
    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let mut tmp = self.path.clone();
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".tmp");
        tmp.set_file_name(name);
        fs::write(&tmp, self.serialize())?;
        fs::rename(&tmp, &self.path)
    }

    /// Serialise the persisted records (file records + writes) in the format
    /// documented on the type.
    fn serialize(&self) -> String {
        let mut out = String::from("# au3-registry v1\n");
        for (key, node) in &self.stored.keys {
            let shown = self.stored.display.get(key).map(String::as_str).unwrap_or(key);
            out.push_str(&escape(shown));
            out.push('\t');
            out.push('\t');
            out.push_str(KEY_RECORD);
            out.push('\t');
            out.push('\n');
            for (name, data) in node.values.values() {
                out.push_str(&escape(shown));
                out.push('\t');
                out.push_str(&escape(name));
                out.push('\t');
                out.push_str(data.type_name());
                out.push('\t');
                out.push_str(&data_payload(data));
                out.push('\n');
            }
        }
        out
    }

    /// Persist after a mutation.
    ///
    /// `recorded` says whether the *file's* records changed: deleting a value
    /// that only ever came from the per-version seed touches nothing on disk,
    /// so no file is created for it. Returns whether the caller's mutation
    /// stands — `false` when it did not happen, or when the record could not be
    /// written (which the `Reg*` functions surface as `@error = 1`).
    fn persisted(&mut self, changed: bool, recorded: bool) -> bool {
        if !changed {
            return false;
        }
        if recorded {
            return self.save().is_ok();
        }
        true
    }
}

impl RegistryStore for FileRegistry {
    fn read(&self, key: &str, value: &str) -> Option<RegistryData> {
        self.inner.read(key, value)
    }

    fn write(&mut self, key: &str, value: &str, data: RegistryData) -> bool {
        let changed = self.inner.write(key, value, data.clone());
        let recorded = changed && self.stored.write(key, value, data);
        self.persisted(changed, recorded)
    }

    fn delete_value(&mut self, key: &str, value: &str) -> bool {
        let changed = self.inner.delete_value(key, value);
        let recorded = changed && self.stored.delete_value(key, value);
        self.persisted(changed, recorded)
    }

    fn delete_key(&mut self, key: &str, recurse: bool) -> bool {
        let changed = self.inner.delete_key(key, recurse);
        let recorded = changed && self.stored.delete_key(key, recurse);
        self.persisted(changed, recorded)
    }

    fn key_exists(&self, key: &str) -> bool {
        self.inner.key_exists(key)
    }

    fn enum_keys(&self, key: &str) -> Vec<String> {
        self.inner.enum_keys(key)
    }

    fn enum_values(&self, key: &str) -> Vec<String> {
        self.inner.enum_values(key)
    }
}

/// The type token that marks a key with no values.
const KEY_RECORD: &str = "KEY";

/// Serialise one value's payload (everything after the type field).
fn data_payload(data: &RegistryData) -> String {
    match data {
        RegistryData::Sz(s) | RegistryData::ExpandSz(s) => escape(s),
        RegistryData::MultiSz(items) => items
            .iter()
            .map(|s| escape(s))
            .collect::<Vec<_>>()
            .join("|"),
        RegistryData::Dword(v) => v.to_string(),
        RegistryData::Qword(v) => v.to_string(),
        RegistryData::Binary(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// Parse a payload back into typed data (`None` for an unknown type token).
fn parse_data(type_token: &str, payload: &str) -> Option<RegistryData> {
    Some(match type_token {
        "REG_SZ" => RegistryData::Sz(unescape(payload)),
        "REG_EXPAND_SZ" => RegistryData::ExpandSz(unescape(payload)),
        "REG_MULTI_SZ" => {
            if payload.is_empty() {
                RegistryData::MultiSz(Vec::new())
            } else {
                // Split on *unescaped* separators, so an item may contain `|`.
                RegistryData::MultiSz(split_escaped(payload, '|'))
            }
        }
        "REG_DWORD" => RegistryData::Dword(payload.trim().parse().ok()?),
        "REG_QWORD" => RegistryData::Qword(payload.trim().parse().ok()?),
        "REG_BINARY" => RegistryData::Binary(hex_decode(payload)?),
        _ => return None,
    })
}

/// Escape the characters that would otherwise break the record layout.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out
}

/// Inverse of [`escape`]; an unknown escape keeps its backslash.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('|') => out.push('|'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Split `s` on `separator`, honouring backslash escapes, and unescape each
/// piece. Used for `REG_MULTI_SZ`, whose items may themselves contain `|`.
fn split_escaped(s: &str, separator: char) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Keep the escape pair intact for `unescape`, so an escaped
            // separator is not mistaken for a real one.
            current.push(c);
            if let Some(next) = chars.next() {
                current.push(next);
            }
        } else if c == separator {
            items.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    items.push(current);
    items.iter().map(|s| unescape(s)).collect()
}

/// Decode a `REG_BINARY` payload (lower-/upper-case hex, no prefix).
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let text = s.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// Canonical, case-insensitive form of a registry key path.
///
/// Expands the short hive aliases and drops a trailing separator, so
/// `HKLM\SOFTWARE\X\`, `HKEY_LOCAL_MACHINE\Software\X` and
/// `hklm\software\x` all name the same key.
pub fn normalize_key(key: &str) -> String {
    let trimmed = key.trim().trim_end_matches('\\');
    if trimmed.is_empty() {
        return String::new();
    }
    let (hive, rest) = match trimmed.split_once('\\') {
        Some((h, r)) => (h, Some(r)),
        None => (trimmed, None),
    };
    let hive = expand_hive(hive);
    match rest {
        Some(r) if !r.is_empty() => format!("{hive}\\{r}").to_ascii_uppercase(),
        _ => hive.to_ascii_uppercase(),
    }
}

/// Case-insensitive form of a registry value name.
fn normalize_value(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

/// Expand the short hive aliases AutoIt accepts.
fn expand_hive(hive: &str) -> &str {
    let h = hive.trim();
    let is = |a: &str, b: &str| h.eq_ignore_ascii_case(a) || h.eq_ignore_ascii_case(b);
    if is("HKLM", "HKEY_LOCAL_MACHINE") {
        "HKEY_LOCAL_MACHINE"
    } else if is("HKCU", "HKEY_CURRENT_USER") {
        "HKEY_CURRENT_USER"
    } else if is("HKCR", "HKEY_CLASSES_ROOT") {
        "HKEY_CLASSES_ROOT"
    } else if is("HKU", "HKEY_USERS") {
        "HKEY_USERS"
    } else if is("HKCC", "HKEY_CURRENT_CONFIG") {
        "HKEY_CURRENT_CONFIG"
    } else if is("HKPD", "HKEY_PERFORMANCE_DATA") {
        "HKEY_PERFORMANCE_DATA"
    } else {
        h
    }
}

/// Flatten an AutoIt array (or a `|`-separated string) into `REG_MULTI_SZ`.
fn strings_of(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .borrow()
            .iter()
            .map(|v| v.to_autoit_string())
            .collect(),
        other => other
            .to_autoit_string()
            .split('|')
            .map(|s| s.to_string())
            .collect(),
    }
}

/// Bytes for `REG_BINARY`: a `Binary` value, or an AutoIt `"0x..."` literal.
fn binary_of(value: &Value) -> Vec<u8> {
    if let Value::Binary(bytes) = value {
        return bytes.as_ref().clone();
    }
    let text = value.to_autoit_string();
    let body = text
        .trim()
        .strip_prefix("0x")
        .or_else(|| text.trim().strip_prefix("0X"))
        .unwrap_or("");
    if body.len().is_multiple_of(2) && !body.is_empty() && body.chars().all(|c| c.is_ascii_hexdigit()) {
        return (0..body.len() / 2)
            .map(|i| u8::from_str_radix(&body[i * 2..i * 2 + 2], 16).unwrap_or(0))
            .collect();
    }
    text.into_bytes()
}

/// Lay down the standard keys for one emulated machine.
fn seed(reg: &mut MemoryRegistry, version: WindowsVersion, arch: WindowsArch, p: &WindowsPaths) {
    let current_version = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = version.build().to_string();
    reg.set_sz(current_version, "ProductName", version.registry_product_name());
    reg.set_sz(current_version, "EditionID", "Professional");
    reg.set_sz(current_version, "InstallationType", "Client");
    reg.set_sz(current_version, "CurrentBuild", &build);
    reg.set_sz(current_version, "CurrentBuildNumber", &build);
    reg.set_dword(current_version, "UBR", 0);
    reg.set_sz(current_version, "CurrentVersion", version.registry_current_version());
    reg.set_dword(current_version, "CurrentMajorVersionNumber", version.major());
    reg.set_dword(current_version, "CurrentMinorVersionNumber", version.minor());
    reg.set_sz(
        current_version,
        "BuildLabEx",
        &format!("{}.1.amd64fre.emulated.0", build),
    );
    reg.set_sz(
        current_version,
        "DisplayVersion",
        version.registry_display_version(),
    );
    reg.set_sz(current_version, "ReleaseId", version.registry_release_id());
    reg.set_sz(current_version, "RegisteredOwner", &p.user_name);
    reg.set_sz(current_version, "RegisteredOrganization", "");
    reg.set_sz(current_version, "SystemRoot", &p.windows_dir);
    reg.set_sz(current_version, "PathName", &p.windows_dir);
    reg.set_sz(current_version, "ProductId", "00330-80000-00000-AA000");
    reg.set_sz(
        current_version,
        "CSDVersion",
        version.service_pack(),
    );

    let cv = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion";
    reg.set_sz(cv, "ProgramFilesDir", &p.program_files);
    reg.set_sz(cv, "ProgramFilesDir (x86)", &p.program_files_x86);
    reg.set_sz(cv, "ProgramW6432Dir", &p.program_files);
    reg.set_sz(cv, "CommonFilesDir", &p.common_files);
    reg.set_sz(cv, "CommonFilesDir (x86)", &p.common_files_x86);
    reg.set_sz(cv, "ProgramData", &p.program_data);
    reg.set_dword(cv, "ProgramFilesDirType", 1);

    let env = r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    reg.set_sz(env, "windir", &p.windows_dir);
    reg.set_sz(env, "SystemRoot", &p.windows_dir);
    reg.set_sz(env, "ComSpec", &format!(r"{}\System32\cmd.exe", p.windows_dir));
    reg.set_sz(env, "TEMP", &p.temp());
    reg.set_sz(env, "TMP", &p.temp());
    reg.set_sz(env, "USERPROFILE", &p.user_profile);
    reg.set_sz(env, "ProgramData", &p.program_data);
    reg.set_sz(env, "ProgramFiles", &p.program_files);
    reg.set_sz(env, "ProgramFiles(x86)", &p.program_files_x86);
    reg.set_sz(env, "ProgramW6432", &p.program_files);
    reg.set_sz(env, "OS", "Windows_NT");
    reg.set_sz(env, "PROCESSOR_ARCHITECTURE", arch.env_value());
    reg.set_sz(env, "NUMBER_OF_PROCESSORS", "4");
    reg.set_sz(
        env,
        "Path",
        &format!(
            r"{}\system32;{}\;{}\System32\Wbem;{}\System32\WindowsPowerShell\v1.0\",
            p.windows_dir, p.windows_dir, p.windows_dir, p.windows_dir
        ),
    );
    reg.set_sz(
        env,
        "PATHEXT",
        ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC",
    );
    reg.set_sz(env, "PROCESSOR_IDENTIFIER", "Intel64 Family 6 Model 158 Stepping 10, GenuineIntel");

    let cpu = r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    reg.set_sz(cpu, "ProcessorNameString", "Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz");
    reg.set_sz(cpu, "Identifier", "Intel64 Family 6 Model 158 Stepping 10");
    reg.set_sz(cpu, "VendorIdentifier", "GenuineIntel");
    reg.set_dword(cpu, "~MHz", 3192);

    let shell = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders";
    reg.set_sz(shell, "AppData", &p.appdata());
    reg.set_sz(shell, "Local AppData", &p.local_appdata());
    reg.set_sz(shell, "Desktop", &p.desktop());
    reg.set_sz(shell, "Personal", &p.documents());
    reg.set_sz(shell, "Favorites", &format!(r"{}\Favorites", p.user_profile));
    reg.set_sz(shell, "Programs", &format!(r"{}\Programs", p.start_menu()));
    reg.set_sz(shell, "Start Menu", &p.start_menu());
    reg.set_sz(shell, "Startup", &format!(r"{}\Programs\Startup", p.start_menu()));
    reg.set_sz(shell, "Templates", &p.templates());
    reg.set_sz(shell, "My Music", &format!(r"{}\Music", p.user_profile));
    reg.set_sz(shell, "My Pictures", &format!(r"{}\Pictures", p.user_profile));
    reg.set_sz(shell, "My Video", &format!(r"{}\Videos", p.user_profile));
    reg.set_sz(shell, "Cache", &p.internet_cache());
    reg.set_sz(shell, "History", &p.history());
    reg.set_sz(shell, "Recent", &p.recent());

    let env_user = r"HKCU\Environment";
    reg.set_sz(env_user, "TEMP", &p.temp());
    reg.set_sz(env_user, "TMP", &p.temp());
    reg.set_sz(env_user, "Path", &format!(r"{}\System32", p.windows_dir));

    // Keys a script may enumerate even though the emulation does not fill them.
    reg.ensure_key(r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall");
    reg.ensure_key(r"HKLM\SOFTWARE\Classes");
    reg.ensure_key(r"HKLM\SOFTWARE\Wow6432Node");
    reg.ensure_key(r"HKCU\Software");
}
