//! Builtin AutoIt function library (the subset the interpreter implements).
//!
//! Two groups live here:
//!
//! * **Pure string/number/math helpers** (`StringLen`, `BitAND`, `Chr`, ...),
//!   which are what the obfuscator's table builders actually need.
//! * **Interpreter-aware helpers** (`Execute`, `Call`, `SetError`,
//!   `IsFunc`, ...), which are implemented against [`Runtime`] because they
//!   re-enter the interpreter.
//!
//! Anything a *complete* runtime needs but this subset does not (Win32, COM,
//! GUI, file I/O) is intentionally left to [`crate::host::Host`].

use std::collections::BTreeMap;
use std::rc::Rc;

use autoitv3_ast::span::Span;

use crate::error::RuntimeError;
use crate::interp::Runtime;
use crate::value::{format_float, Value};

/// Dispatch a builtin call. `Ok(None)` means "not a builtin I know".
pub(crate) fn call(
    rt: &mut Runtime,
    name: &str,
    args: &[Value],
    span: Span,
) -> Result<Option<Value>, RuntimeError> {
    let key = name.to_ascii_lowercase();
    let v = match key.as_str() {
        // ---------------- conversion / numbers ----------------
        "string" => Value::Str(args.first().map(|a| a.to_autoit_string()).unwrap_or_default()),
        "number" => {
            let s = args.first().map(|a| a.to_autoit_string()).unwrap_or_default();
            match crate::value::parse_number(&s) {
                Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Value::Int(f as i64),
                Some(f) => Value::Float(f),
                None => Value::Int(0),
            }
        }
        "int" => Value::Int(args.first().map(|a| a.to_int()).unwrap_or(0)),
        "abs" => {
            let a = args.first().cloned().unwrap_or(Value::Int(0));
            match a {
                Value::Int(i) => Value::Int(i.abs()),
                other => Value::Float(other.to_f64().abs()),
            }
        }
        "mod" => {
            let a = args.first().map(|v| v.to_int()).unwrap_or(0);
            let b = args.get(1).map(|v| v.to_int()).unwrap_or(0);
            Value::Int(if b == 0 { 0 } else { a % b })
        }
        "hex" => {
            let a = args.first().map(|v| v.to_int()).unwrap_or(0);
            let digits = args.get(1).map(|v| v.to_int()).unwrap_or(8).clamp(1, 16) as usize;
            Value::Str(format!("{:0width$X}", a, width = digits))
        }
        "dec" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let t = s.trim();
            let neg = t.starts_with('-');
            let body = t.trim_start_matches('-');
            let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
                i64::from_str_radix(h, 16).unwrap_or(0)
            } else {
                t.parse::<i64>().unwrap_or(0)
            };
            Value::Int(if neg { -v } else { v })
        }
        "chr" => {
            let c = args.first().map(|v| v.to_int()).unwrap_or(0) as u32;
            Value::Str(char::from_u32(c).map(|c| c.to_string()).unwrap_or_default())
        }
        "asc" | "ascw" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Int(s.chars().next().map(|c| c as i64).unwrap_or(0))
        }

        // ---------------- bit operations ----------------
        "bitand" | "bitor" | "bitxor" => {
            let mut acc = args.first().map(|v| v.to_int()).unwrap_or(0);
            for a in args.iter().skip(1) {
                let b = a.to_int();
                acc = match key.as_str() {
                    "bitand" => acc & b,
                    "bitor" => acc | b,
                    _ => acc ^ b,
                };
            }
            Value::Int(acc)
        }
        "bitnot" => Value::Int(!args.first().map(|v| v.to_int()).unwrap_or(0)),
        "bitshift" => {
            let a = args.first().map(|v| v.to_int()).unwrap_or(0);
            let n = args.get(1).map(|v| v.to_int()).unwrap_or(0);
            Value::Int(if n >= 0 { a.wrapping_shl(n as u32) } else { a.wrapping_shr((-n) as u32) })
        }

        // ---------------- strings ----------------
        "stringlen" => Value::Int(
            args.first()
                .map(|v| v.to_autoit_string().chars().count() as i64)
                .unwrap_or(0),
        ),
        "stringleft" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let n = args.get(1).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
            Value::Str(s.chars().take(n).collect())
        }
        "stringright" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let n = args.get(1).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
            let chars: Vec<char> = s.chars().collect();
            let start = chars.len().saturating_sub(n);
            Value::Str(chars[start..].iter().collect())
        }
        "stringtrimleft" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let n = args.get(1).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
            Value::Str(s.chars().skip(n).collect())
        }
        "stringtrimright" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let n = args.get(1).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
            let chars: Vec<char> = s.chars().collect();
            let keep = chars.len().saturating_sub(n);
            Value::Str(chars[..keep].iter().collect())
        }
        "stringmid" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let start = args.get(1).map(|v| v.to_int()).unwrap_or(1);
            let count = args.get(2).map(|v| v.to_int()).unwrap_or(-1);
            let chars: Vec<char> = s.chars().collect();
            let from = (start - 1).max(0) as usize;
            if from >= chars.len() {
                Value::Str(String::new())
            } else {
                let take = if count < 0 { chars.len() - from } else { count as usize };
                Value::Str(chars[from..(from + take).min(chars.len())].iter().collect())
            }
        }
        "stringstripws" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let flags = args.get(1).map(|v| v.to_int()).unwrap_or(3);
            let mut out = s.as_str();
            if flags & 1 != 0 {
                out = out.trim_start();
            }
            if flags & 2 != 0 {
                out = out.trim_end();
            }
            let mut result = out.to_string();
            if flags & 4 != 0 {
                result = result.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            Value::Str(result)
        }
        "stringupper" => Value::Str(
            args.first().map(|v| v.to_autoit_string().to_uppercase()).unwrap_or_default(),
        ),
        "stringlower" => Value::Str(
            args.first().map(|v| v.to_autoit_string().to_lowercase()).unwrap_or_default(),
        ),
        "stringreverse" => Value::Str(
            args.first()
                .map(|v| v.to_autoit_string().chars().rev().collect())
                .unwrap_or_default(),
        ),
        "stringaddcr" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Str(s.replace('\n', "\r\n"))
        }
        "stringinstr" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let sub = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
            let case = args.get(2).map(|v| v.to_int()).unwrap_or(0);
            let (hay, needle) = if case == 0 {
                (s.to_lowercase(), sub.to_lowercase())
            } else {
                (s.clone(), sub.clone())
            };
            match hay.find(&needle) {
                Some(byte_idx) => {
                    let char_idx = hay[..byte_idx].chars().count() as i64 + 1;
                    Value::Int(char_idx)
                }
                None => Value::Int(0),
            }
        }
        "stringreplace" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let sub = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
            let rep = args.get(2).map(|v| v.to_autoit_string()).unwrap_or_default();
            let count = args.get(3).map(|v| v.to_int()).unwrap_or(-1);
            if sub.is_empty() {
                Value::Str(s)
            } else if count < 0 {
                Value::Str(s.replace(&sub, &rep))
            } else {
                Value::Str(s.replacen(&sub, &rep, count as usize))
            }
        }
        "stringformat" => {
            // Support the `%s` / `%d` subset the obfuscator uses, including
            // the `%0Nd` zero-padded integer form.
            let fmt = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Str(format_like(&fmt, &args[1.min(args.len())..]))
        }
        "stringcompare" => {
            let a = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let b = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
            let case = args.get(2).map(|v| v.to_int()).unwrap_or(0);
            let (x, y) = if case == 0 {
                (a.to_lowercase(), b.to_lowercase())
            } else {
                (a, b)
            };
            Value::Int(match x.cmp(&y) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        }
        "stringsplit" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let delims = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
            let parts: Vec<Value> = if delims.is_empty() {
                vec![Value::Str(s.clone())]
            } else {
                s.split(|c| delims.contains(c))
                    .map(|p| Value::Str(p.to_string()))
                    .collect()
            };
            // AutoIt returns a 1-based array whose [0] is the element count.
            let mut out = vec![Value::Int(parts.len() as i64)];
            out.extend(parts);
            Value::array(out)
        }
        "stringisint" | "stringisdigit" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        }
        "stringisfloat" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Bool(s.parse::<f64>().is_ok())
        }
        "stringisalnum" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()))
        }
        "stringisalpha" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()))
        }
        "stringisspace" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_whitespace()))
        }

        // ---------------- arrays / maps ----------------
        "ubound" => {
            let a = args.first().cloned().unwrap_or(Value::Null);
            match a {
                Value::Array(a) => Value::Int(a.borrow().len() as i64),
                Value::Map(m) => Value::Int(m.borrow().len() as i64),
                Value::Str(s) => Value::Int(s.chars().count() as i64),
                _ => Value::Int(0),
            }
        }
        "isarray" => Value::Bool(matches!(args.first(), Some(Value::Array(_)))),
        "ismap" => Value::Bool(matches!(args.first(), Some(Value::Map(_)))),
        "map" => {
            let mut m: BTreeMap<String, Value> = BTreeMap::new();
            let mut i = 0;
            while i + 1 < args.len() {
                m.insert(args[i].to_autoit_string(), args[i + 1].clone());
                i += 2;
            }
            Value::Map(Rc::new(std::cell::RefCell::new(m)))
        }
        "mapexists" => {
            let (m, k) = (args.first().cloned(), args.get(1).cloned());
            match (m, k) {
                (Some(Value::Map(m)), Some(k)) => {
                    Value::Bool(m.borrow().contains_key(&k.to_autoit_string()))
                }
                _ => Value::Bool(false),
            }
        }
        "mapkeys" => {
            let a = args.first().cloned().unwrap_or(Value::Null);
            match a {
                Value::Map(m) => {
                    let keys: Vec<Value> =
                        m.borrow().keys().map(|k| Value::Str(k.clone())).collect();
                    let mut out = vec![Value::Int(keys.len() as i64)];
                    out.extend(keys);
                    Value::array(out)
                }
                _ => Value::array(vec![Value::Int(0)]),
            }
        }
        "mapremove" => {
            let (m, k) = (args.first().cloned(), args.get(1).cloned());
            if let (Some(Value::Map(m)), Some(k)) = (m, k) {
                m.borrow_mut().remove(&k.to_autoit_string());
            }
            Value::Int(1)
        }

        // ---------------- binary ----------------
        "binary" => {
            let raw = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Binary(Rc::new(raw.into_bytes()))
        }
        "binarytostring" => {
            let b = args.first().cloned().unwrap_or(Value::Null);
            match b {
                Value::Binary(b) => Value::Str(String::from_utf8_lossy(&b).to_string()),
                other => Value::Str(other.to_autoit_string()),
            }
        }
        "stringtobinary" => {
            let raw = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Binary(Rc::new(raw.into_bytes()))
        }

        // ---------------- type predicates ----------------
        "isnumber" => Value::Bool(args.first().map(|v| v.is_number()).unwrap_or(false)),
        "isint" => Value::Bool(matches!(
            args.first(),
            Some(Value::Int(_)) | Some(Value::Bool(_))
        )),
        "isstring" => Value::Bool(matches!(args.first(), Some(Value::Str(_)))),
        "isbinary" => Value::Bool(matches!(args.first(), Some(Value::Binary(_)))),
        "isptr" | "ishwnd" => Value::Bool(false),
        "iskeyword" => Value::Bool(matches!(
            args.first(),
            Some(Value::Default) | Some(Value::Null)
        )),
        "isfunc" => {
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Bool(rt.has_function(&s))
        }
        "binarylen" => match args.first() {
            Some(Value::Binary(b)) => Value::Int(b.len() as i64),
            Some(other) => Value::Int(other.to_autoit_string().len() as i64),
            None => Value::Int(0),
        },

        // ---------------- interpreter-aware ----------------
        "seterror" => {
            let e = args.first().map(|v| v.to_int()).unwrap_or(0);
            let x = args.get(1).map(|v| v.to_int()).unwrap_or(0);
            let r = args.get(2).cloned().unwrap_or(Value::Null);
            rt.set_error_value(e, x);
            r
        }
        "setextended" => {
            let x = args.first().map(|v| v.to_int()).unwrap_or(0);
            let r = args.get(1).cloned().unwrap_or(Value::Null);
            rt.set_error_value(rt.error(), x);
            r
        }
        "execute" => {
            let src = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            rt.execute_source(&src, span)?
        }
        "call" => {
            let fname = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let rest = args[1.min(args.len())..].to_vec();
            rt.call_named(&fname, rest, span)?
        }

        // ---------------- benign no-ops ----------------
        // These appear in the obfuscator's helpers but only matter for side
        // effects this interpreter does not model; returning a neutral value
        // keeps table evaluation going.
        "opt" | "autoitsetoption" => Value::Int(1),
        "sleep" => Value::Int(0),
        "consolewrite" | "filewrite" | "fileflush" | "fileclose" | "dircreate" => Value::Int(1),
        "dllcall" | "dllstructcreate" | "dllstructgetdata" | "dllstructsetdata"
        | "dllstructgetsize" | "isdllstruct" | "guictrlread" | "guictrlcreatepic"
        | "guictrlcreatebutton" | "guictrlcreategraphic" | "guictrlcreateinput"
        | "guictrlcreatelistview" | "guictrlsendmsg" | "guictrlsetimage"
        | "guictrlsetstate" | "guictrlsetcolor" | "guictrlsetdata" | "guictrldelete"
        | "guictrlgetstate" | "guictrlgetpos" | "guisetstate" | "guisetbkcolor"
        | "guicreate" | "guigetmsg" => Value::Int(0),
        "regread" => Value::Str(String::new()),
        "filegetsize" => Value::Int(0),
        "filegetversion" => Value::Str("0.0.0.0".into()),
        "filegetattrib" => Value::Str(String::new()),
        "fileexists" => Value::Int(0),
        "processclose" | "processexists" => Value::Int(0),
        "clipget" => Value::Str(String::new()),
        "cliput" | "clipput" => Value::Int(1),
        "stdoutread" => Value::Str(String::new()),

        _ => return Ok(None),
    };
    Ok(Some(v))
}

/// Minimal `StringFormat` supporting `%s`, `%d`, `%i`, `%u`, `%x`, `%X`,
/// `%f`, `%c`, `%%` and the `%0N` / `%-N` width/flag forms.
fn format_like(fmt: &str, args: &[Value]) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut ai = 0usize;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // Flags and width.
        let mut spec = String::new();
        while let Some(&c) = chars.peek() {
            if c == '-' || c == '0' || c == '+' || c == ' ' || c.is_ascii_digit() || c == '.' {
                spec.push(c);
                chars.next();
            } else {
                break;
            }
        }
        let Some(conv) = chars.next() else { break };
        let arg = args.get(ai);
        if conv != '%' {
            ai += 1;
        }
        let width: Option<usize> = {
            let digits: String =
                spec.chars().filter(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        };
        let zero_pad = spec.contains('0') && !spec.contains('-');
        let left = spec.contains('-');
        let body = match conv {
            '%' => "%".to_string(),
            's' => arg.map(|a| a.to_autoit_string()).unwrap_or_default(),
            'd' | 'i' | 'u' => arg.map(|a| a.to_int()).unwrap_or(0).to_string(),
            'x' => format!("{:x}", arg.map(|a| a.to_int()).unwrap_or(0)),
            'X' => format!("{:X}", arg.map(|a| a.to_int()).unwrap_or(0)),
            'f' => {
                let prec = spec
                    .split('.')
                    .nth(1)
                    .and_then(|p| p.parse::<usize>().ok())
                    .unwrap_or(6);
                format!("{:.*}", prec, arg.map(|a| a.to_f64()).unwrap_or(0.0))
            }
            'c' => char::from_u32(arg.map(|a| a.to_int()).unwrap_or(0) as u32)
                .map(|c| c.to_string())
                .unwrap_or_default(),
            other => format!("%{spec}{other}"),
        };
        match width {
            Some(w) if body.chars().count() < w => {
                let pad = w - body.chars().count();
                let fill = if zero_pad { '0' } else { ' ' };
                if left {
                    out.push_str(&body);
                    out.extend(std::iter::repeat(fill).take(pad));
                } else {
                    out.extend(std::iter::repeat(fill).take(pad));
                    out.push_str(&body);
                }
            }
            _ => out.push_str(&body),
        }
    }
    out
}

/// Exposed for the `format_float` re-export used by tests.
#[allow(dead_code)]
pub(crate) fn float_to_string(f: f64) -> String {
    format_float(f)
}