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
use std::cell::RefCell;
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
            Value::Int(i) => MapKey::Int(*i),
            other => MapKey::Str(other.to_autoit_string()),
        }
    }
}

/// A shared, mutable AutoIt `Map`.
pub type MapRef = Rc<RefCell<BTreeMap<MapKey, Value>>>;

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
    FuncRef(String),
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
        matches!(self, Value::Int(_) | Value::Float(_) | Value::Bool(_))
    }

    /// AutoIt truthiness: `0`, `""` and `Null` are false.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null | Value::Default => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    return false;
                }
                match parse_number(t) {
                    Some(n) => n != 0.0,
                    None => false,
                }
            }
            Value::Array(_) | Value::Map(_) | Value::Binary(_) | Value::FuncRef(_)
            | Value::Obj(_) => true,
        }
    }

    /// Convert to `i64` using AutoIt coercion rules (strings parse a leading
    /// number, otherwise 0).
    pub fn to_int(&self) -> i64 {
        match self {
            Value::Int(i) => *i,
            Value::Float(f) => *f as i64,
            Value::Bool(b) => *b as i64,
            Value::Str(s) => parse_number(s.trim()).unwrap_or(0.0) as i64,
            _ => 0,
        }
    }

    /// Convert to `f64` using AutoIt coercion rules.
    pub fn to_f64(&self) -> f64 {
        match self {
            Value::Int(i) => *i as f64,
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
            Value::Float(f) => format_float(*f),
            Value::Str(s) => s.clone(),
            Value::Array(_) => String::new(),
            Value::Map(_) => String::new(),
            Value::Binary(b) => binary_to_hex(b),
            Value::FuncRef(name) => name.clone(),
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
            Value::Int(i) => LitKind::Int(*i),
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
            (Value::FuncRef(a), Value::FuncRef(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),
            (Value::Obj(a), Value::Obj(b)) => Rc::ptr_eq(a, b),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            _ => false,
        }
    }

    /// Case-insensitive equality (AutoIt `=` and `<>`).
    pub fn eq_loose(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Str(a), Value::Str(b)) => a.eq_ignore_ascii_case(b),
            (a, b) if a.is_number() && b.is_number() => a.to_f64() == b.to_f64(),
            (Value::Str(a), b) => a.eq_ignore_ascii_case(&b.to_autoit_string()),
            (a, Value::Str(b)) => a.to_autoit_string().eq_ignore_ascii_case(b),
            _ => self.eq_strict(other),
        }
    }

    /// Ordering comparison (`<`, `<=`, `>`, `>=`).
    ///
    /// Numbers compare numerically; anything else compares as a
    /// case-insensitive string, matching AutoIt.
    pub fn compare(&self, other: &Value) -> Ordering {
        if self.is_number() && other.is_number() {
            let (a, b) = (self.to_f64(), other.to_f64());
            return a.partial_cmp(&b).unwrap_or(Ordering::Equal);
        }
        let (a, b) = (self.to_autoit_string(), other.to_autoit_string());
        let (al, bl) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
        al.cmp(&bl)
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "Null"),
            Value::Default => write!(f, "Default"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Array(a) => write!(f, "Array(len={})", a.borrow().len()),
            Value::Map(m) => write!(f, "Map(len={})", m.borrow().len()),
            Value::Binary(b) => write!(f, "Binary(len={})", b.len()),
            Value::FuncRef(n) => write!(f, "FuncRef({n})"),
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
pub fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, t),
    };
    let val = if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if digits.is_empty() {
            return None;
        }
        i64::from_str_radix(&digits, 16).ok()? as f64
    } else {
        let digits: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if digits.is_empty() || digits == "." {
            return None;
        }
        digits.parse::<f64>().ok()?
    };
    Some(if neg { -val } else { val })
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
