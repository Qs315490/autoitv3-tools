//! Runtime value model (AutoIt v3 semantics).
//!
//! AutoIt is dynamically typed and has no user-visible distinction between
//! "int" and "float" beyond how a value prints: both are `Number` variants of
//! the single `Variant` type. We keep `Int`/`Float` apart so that arithmetic
//! and formatting mimic AutoIt's observable behaviour (`String(2)` is `"2"`,
//! not `"2.0"`).
//!
//! Arrays and maps are reference-counted and interior-mutable so that the
//! `ByRef` parameter semantics used by the obfuscator's helpers (`MergeArrays`,
//! `ReDim`) work naturally: a callee mutating (or resizing) an array mutates the
//! caller's array.

use autoitv3_ast::ast::LitKind;
use std::cell::{OnceCell, RefCell};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

/// A shared, mutable AutoIt array. AutoIt arrays are 0-based.
pub type ArrayRef = Rc<RefCell<Vec<Value>>>;

/// A map key. AutoIt map keys are strings or integers, and an integer key is
/// **not** the same entry as its string spelling (`$m[3]` and `$m["3"]` are
/// distinct). String keys are case sensitive.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MapKey {
    Int(i64),
    Str(String),
}

impl MapKey {
    /// The key a value denotes when used as a map subscript: integers keep
    /// their integer identity, everything else becomes its string form.
    pub fn from_value(v: &Value) -> MapKey {
        match v {
            Value::Int(i) | Value::Ptr(i) => MapKey::Int(*i),
            other => MapKey::Str(other.to_autoit_string()),
        }
    }
}

/// A shared, mutable AutoIt `Map`.
pub type MapRef = Rc<RefCell<BTreeMap<MapKey, Value>>>;

/// The GUI window/control handles a run has minted, shared between the
/// runtime (the `IsHWnd`/`HWnd` builtins) and the platform layer that
/// produces them.
///
/// AutoIt's `Hwnd` is a flavour of its `Ptr` base type: `GUICreate`
/// answers a `Ptr` that also satisfies `IsHWnd`, while a `DllCall`
/// `hwnd` return is a plain `Ptr` and does not (all measured on the
/// official x64 interpreter). The flavour cannot ride inside the value —
/// arithmetic and `Ptr()` keep the numeric value while the flavour has to
/// survive — so the runtime keeps the set of live handles and the predicates
/// consult it.
pub type HwndSet = Rc<RefCell<std::collections::HashSet<i64>>>;

/// An opaque object created by a platform layer (`ObjCreate`, …).
///
/// The runtime only knows its name and an opaque handle; every member access
/// is delegated back to the [`crate::platform::Platform`] that made it.
/// `release`, when set, is invoked on drop so the platform can free the
/// underlying resource.
pub struct NativeObject {
    /// The name the object was created under (a ProgID, typically).
    pub name: String,
    /// Opaque platform handle (an interface pointer, a key, …).
    pub handle: usize,
    /// Platform-installed destructor.
    pub release: Option<Box<dyn Fn()>>,
}

impl fmt::Debug for NativeObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Obj({})", self.name)
    }
}

impl Drop for NativeObject {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// A shared reference to a platform object.
pub type ObjRef = Rc<NativeObject>;

/// A function named by the script: the spelling it was written with, plus the
/// lower-cased key the runtime looks it up by.
///
/// Function references are shared (`Rc`), so the key is computed once per named
/// function and every indirect call reuses it: cloning a reference is a
/// refcount bump, and neither the lookup nor the call allocates for the name.
#[derive(Debug, Clone)]
pub struct FuncRefName {
    display: String,
    key: OnceCell<String>,
}

/// A shared function reference.
pub type FuncRef = Rc<FuncRefName>;

impl FuncRefName {
    /// A reference to the function spelled `display`.
    pub fn new(display: impl Into<String>) -> Rc<Self> {
        Rc::new(Self {
            display: display.into(),
            key: OnceCell::new(),
        })
    }

    /// The name as the script wrote it.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// The lookup key: lower-cased, without a leading `$`.
    pub fn key(&self) -> &str {
        self.key
            .get_or_init(|| self.display.trim_start_matches('$').to_ascii_lowercase())
    }
}

/// A runtime AutoIt value.
#[derive(Clone)]
pub enum Value {
    /// `Null` — no value.
    Null,
    /// `Default` — "use the default for this parameter".
    Default,
    /// `True` / `False`.
    Bool(bool),
    /// An integral number.
    Int(i64),
    /// A pointer - AutoIt's `Ptr` **base type**.
    ///
    /// It behaves like an integer everywhere (arithmetic, comparison, `DllCall`
    /// arguments), but it is a distinct type: `IsPtr` answers 1 for it, `IsInt`
    /// does not, `VarGetType` says "Ptr", and turning it into a string gives hex
    /// rather than decimal - all measured against the official x64 interpreter
    /// (`0x` + 16 upper-case digits).
    Ptr(i64),
    /// A floating point number.
    Float(f64),
    /// A string.
    Str(String),
    /// An array (`Local $a[3]`, `[1, 2, 3]`).
    Array(ArrayRef),
    /// A `Map`.
    Map(MapRef),
    /// A binary buffer (`Binary(...)`).
    Binary(Rc<Vec<u8>>),
    /// A reference to a function by name. Produced by evaluating a bare
    /// identifier such as the function names stored in the obfuscator's
    /// function table.
    FuncRef(Rc<FuncRefName>),
    /// A platform object (`ObjCreate`); opaque to the runtime.
    Obj(ObjRef),
}

impl Value {
    /// Convenience constructor for a string value.
    pub fn str(s: impl Into<String>) -> Self {
        Value::Str(s.into())
    }

    /// Convenience constructor for a platform object value.
    pub fn obj(o: ObjRef) -> Self {
        Value::Obj(o)
    }

    /// Convenience constructor for an array value.
    pub fn array(items: Vec<Value>) -> Self {
        Value::Array(Rc::new(RefCell::new(items)))
    }

    /// Convenience constructor for an empty array of `len` zero elements.
    pub fn array_sized(len: usize) -> Self {
        Value::array(vec![Value::Int(0); len])
    }

    /// Convenience constructor for a map value.
    pub fn map() -> Self {
        Value::Map(Rc::new(RefCell::new(BTreeMap::new())))
    }

    /// The AutoIt type name, as `IsInt`/`IsString`-style predicates see it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Default => "Default",
            Value::Bool(_) => "Bool",
            Value::Int(_) => "Int",
            Value::Ptr(_) => "Ptr",
            Value::Float(_) => "Float",
            Value::Str(_) => "String",
            Value::Array(_) => "Array",
            Value::Map(_) => "Map",
            Value::Binary(_) => "Binary",
            Value::FuncRef(_) => "Function",
            Value::Obj(_) => "Object",
        }
    }

    /// True when this value is a number variant.
    pub fn is_number(&self) -> bool {
        matches!(
            self,
            Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Ptr(_)
        )
    }

    /// AutoIt truthiness: `0`, `0.0`, `""` and `Null` are false.
    ///
    /// A **string** is judged by whether it is empty, not by whether it looks
    /// like a number: `"0"` and `"abc"` are both true, `""` is false. That is
    /// what makes `If Not $path Then` mean "the path is empty" in AutoIt, and
    /// code written that way silently takes the wrong branch if strings are
    /// parsed as numbers here.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null | Value::Default => false,
            Value::Bool(b) => *b,
            Value::Int(i) | Value::Ptr(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Array(_) | Value::Map(_) | Value::Binary(_) | Value::FuncRef(_)
            | Value::Obj(_) => true,
        }
    }

    /// Convert to `i64` using AutoIt coercion rules (strings parse a leading
    /// number, otherwise 0).
    pub fn to_int(&self) -> i64 {
        match self {
            Value::Int(i) | Value::Ptr(i) => *i,
            Value::Float(f) => *f as i64,
            Value::Bool(b) => *b as i64,
            Value::Str(s) => parse_number(s.trim()).unwrap_or(0.0) as i64,
            _ => 0,
        }
    }

    /// Convert to `f64` using AutoIt coercion rules.
    pub fn to_f64(&self) -> f64 {
        match self {
            Value::Int(i) | Value::Ptr(i) => *i as f64,
            Value::Float(f) => *f,
            Value::Bool(b) => *b as i64 as f64,
            Value::Str(s) => parse_number(s.trim()).unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// Render using AutoIt's `String()` rules.
    ///
    /// A binary renders as `0x` followed by upper-case hex — AutoIt's string
    /// form of a byte string, and the shape scripts rely on: the obfuscator
    /// writes `String($binary)` and then strips exactly that `0x` prefix before
    /// reading the hex digits back.
    pub fn to_autoit_string(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Default => String::new(),
            Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Ptr(i) => pointer_hex(*i),
            Value::Float(f) => format_float(*f),
            Value::Str(s) => s.clone(),
            Value::Array(_) => String::new(),
            Value::Map(_) => String::new(),
            Value::Binary(b) => binary_to_hex(b),
            Value::FuncRef(name) => name.display().to_string(),
            Value::Obj(o) => o.name.clone(),
        }
    }

    /// Convert to an AST literal, when this value can be represented as one.
    ///
    /// Used by deobfuscation passes that want to *inline* an evaluated
    /// constant back into the syntax tree (`Array`, `Map` and function
    /// references have no literal form).
    pub fn to_lit_kind(&self) -> Option<LitKind> {
        Some(match self {
            Value::Null => LitKind::Null,
            Value::Default => LitKind::Default,
            Value::Bool(b) => LitKind::Bool(*b),
            Value::Int(i) | Value::Ptr(i) => LitKind::Int(*i),
            Value::Float(f) => LitKind::Float(*f),
            Value::Str(s) => LitKind::Str(s.clone()),
            _ => return None,
        })
    }

    /// Case-sensitive equality (AutoIt `==`).
    pub fn eq_strict(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Default, Value::Default) => true,
            (a, b) if a.is_number() && b.is_number() => a.to_f64() == b.to_f64(),
            (Value::Str(a), Value::Str(b)) => a == b,
            // Two binaries are equal when their bytes are — the obfuscator
            // checks a decrypted buffer against a stored digest this way.
            (Value::Binary(a), Value::Binary(b)) => a == b,
            (Value::Str(a), b) => a.as_str() == b.to_autoit_string(),
            (a, Value::Str(b)) => a.to_autoit_string() == b.as_str(),
            (Value::FuncRef(a), Value::FuncRef(b)) => a.display() == b.display(),
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),
            (Value::Obj(a), Value::Obj(b)) => Rc::ptr_eq(a, b),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            _ => false,
        }
    }

    /// Case-insensitive equality (AutoIt `=` and `<>`).
    ///
    /// AutoIt tries a case-insensitive **string** comparison first and falls
    /// back to a **numeric** one when the two values are not the same text.
    /// Both halves are load-bearing, and both were measured on the official
    /// x64 interpreter:
    ///
    /// * a string compared with a number is parsed as a number, and a string
    ///   with no leading number counts as `0` - `"" = 0` and `"abc" = 0` are
    ///   **true**, while `"" = "0"` (two strings) is **false**;
    /// * the string comparison happens first, so `"true" = True` is true by
    ///   text even though `"true"` as a number is `0`;
    /// * `Null` and `Default` equal only themselves: `Null = ""` is false.
    pub fn eq_loose(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) | (Value::Default, Value::Default) => true,
            (Value::Null, _) | (_, Value::Null) => false,
            (Value::Default, _) | (_, Value::Default) => false,
            (Value::Str(a), Value::Str(b)) => a.eq_ignore_ascii_case(b),
            (Value::Str(a), b) => {
                a.eq_ignore_ascii_case(&b.to_autoit_string())
                    || (b.is_number() && parse_number(a.trim()).unwrap_or(0.0) == b.to_f64())
            }
            (a, Value::Str(b)) => {
                a.to_autoit_string().eq_ignore_ascii_case(b)
                    || (a.is_number() && a.to_f64() == parse_number(b.trim()).unwrap_or(0.0))
            }
            _ => self.eq_strict(other),
        }
    }

    /// Ordering comparison (`<`, `<=`, `>`, `>=`).
    ///
    /// Numbers compare numerically and two strings compare as case-insensitive
    /// strings; a string mixed with a number is coerced to a number. Measured
    /// on the official x64 interpreter: `"10" < 9` is false while
    /// `"10" < "9"` is true, and `"abc" < 5` is true because `"abc"`
    /// counts as `0`.
    pub fn compare(&self, other: &Value) -> Ordering {
        fn numeric(a: f64, b: f64) -> Ordering {
            a.partial_cmp(&b).unwrap_or(Ordering::Equal)
        }
        fn text(a: &Value, b: &Value) -> Ordering {
            let (a, b) = (a.to_autoit_string(), b.to_autoit_string());
            a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
        }
        match (self, other) {
            (Value::Str(a), Value::Str(b)) => a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()),
            (Value::Str(a), b) if b.is_number() => {
                numeric(parse_number(a.trim()).unwrap_or(0.0), b.to_f64())
            }
            (a, Value::Str(b)) if a.is_number() => {
                numeric(a.to_f64(), parse_number(b.trim()).unwrap_or(0.0))
            }
            _ if self.is_number() && other.is_number() => numeric(self.to_f64(), other.to_f64()),
            _ => text(self, other),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "Null"),
            Value::Default => write!(f, "Default"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Ptr(i) => write!(f, "Ptr({i})"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Array(a) => write!(f, "Array(len={})", a.borrow().len()),
            Value::Map(m) => write!(f, "Map(len={})", m.borrow().len()),
            Value::Binary(b) => write!(f, "Binary(len={})", b.len()),
            Value::FuncRef(n) => write!(f, "FuncRef({})", n.display()),
            Value::Obj(o) => write!(f, "Obj({})", o.name),
        }
    }
}

/// Format a float the way AutoIt's `String()` does: integral floats print
/// without a fractional part (`1.0` -> `"1"`), others use the shortest
/// round-trippable representation.
pub fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "-1.#IND".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "1.#INF" } else { "-1.#INF" }.to_string();
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    let s = format!("{f}");
    s
}

/// Parse a leading AutoIt number from `s` (decimal, `0x` hex, or float).
///
/// Measured on the official x64 interpreter, a leading `+` is accepted
/// (`"+5" + 0` is `5`), the mantissa stops at the second dot or at any other
/// character (`"1.5.5"` is `1.5`, `"3abc"` is `3`), and a **complete**
/// exponent is honoured (`"1e2" + 0` is `100`, `"1E3"` is `1000`, while
/// `"1e"` is `1`).
pub fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, t.strip_prefix('+').unwrap_or(t).trim_start()),
    };
    if rest.is_empty() {
        return None;
    }
    let value = if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if digits.is_empty() {
            return None;
        }
        i64::from_str_radix(&digits, 16).ok()? as f64
    } else {
        let bytes = rest.as_bytes();
        let (mut end, mut dot) = (0, false);
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            if bytes[end] == b'.' {
                if dot {
                    break;
                }
                dot = true;
            }
            end += 1;
        }
        let mut text = rest[..end].to_string();
        if text.is_empty() || text == "." {
            return None;
        }
        if end < bytes.len() && bytes[end].eq_ignore_ascii_case(&b'e') {
            let mut i = end + 1;
            let mut exponent = String::new();
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                exponent.push(bytes[i] as char);
                i += 1;
            }
            let digits = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                exponent.push(bytes[i] as char);
                i += 1;
            }
            if i > digits {
                text.push('e');
                text.push_str(&exponent);
            }
        }
        text.parse::<f64>().ok()?
    };
    Some(if neg { -value } else { value })
}
/// AutoIt's string form of a pointer: `0x` and 16 upper-case hex digits.
///
/// Measured on the official x64 interpreter - `DllStructGetPtr($s)` prints
/// `0x000001CA37350F60`, zero-padded to the pointer width of that build (the
/// 64-bit one, which the emulation models by default).
pub fn pointer_hex(value: i64) -> String {
    format!("0x{:016X}", value as u64)
}

/// AutoIt's string form of a binary: `0x` followed by upper-case hex.
pub fn binary_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}
