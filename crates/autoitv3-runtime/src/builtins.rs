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

use std::time::Duration;

use autoitv3_ast::span::Span;

use crate::profile::SleepPolicy;

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
            // `Hex` renders a binary (or a string) as upper-case hex digits, and
            // anything else as an integer.
            if let Some(Value::Binary(bytes)) = args.first() {
                return Ok(Some(Value::Str(
                    crate::value::binary_to_hex(bytes)
                        .trim_start_matches("0x")
                        .to_string(),
                )));
            }
            let a = args.first().map(|v| v.to_int()).unwrap_or(0);
            let digits = args.get(1).map(|v| v.to_int()).unwrap_or(8).clamp(1, 16) as usize;
            Value::Str(format!("{:0width$X}", a, width = digits))
        }
        "dec" => {
            // `Dec("1A")` is 26: AutoIt reads the argument as *hexadecimal*, with
            // an optional sign and an optional `0x`, and stops at the first
            // character that is not a hex digit.
            let s = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let t = s.trim();
            let (neg, body) = match t.strip_prefix('-') {
                Some(rest) => (true, rest.trim_start()),
                None => (false, t),
            };
            let body = body
                .strip_prefix("0x")
                .or_else(|| body.strip_prefix("0X"))
                .unwrap_or(body);
            let digits: String = body.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            let v = if digits.is_empty() {
                0
            } else {
                i64::from_str_radix(&digits, 16).unwrap_or(0)
            };
            let v = if neg { -v } else { v };
            match args.get(1).map(|a| a.to_int()) {
                // `Dec($hex, $length)` renders the value with a fixed width.
                Some(width) if width > 0 => Value::Str(format!("{:0width$}", v, width = width as usize)),
                _ => Value::Int(v),
            }
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
            let subj = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let delims = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
            let flag = arg_flag(args, 2);
            // $STR_CHRSPLIT (0) is the default: every character of `delims`
            // splits. $STR_ENTIRESPLIT (1) treats the whole string as one
            // delimiter. $STR_NOCOUNT (2) drops the leading element count.
            let entire = flag & 1 != 0;
            let no_count = flag & 2 != 0;

            let parts: Vec<String> = if delims.is_empty() {
                subj.chars().map(|c| c.to_string()).collect()
            } else if entire {
                subj.split(&delims).map(|p| p.to_string()).collect()
            } else {
                subj.split(|c| delims.contains(c))
                    .map(|p| p.to_string())
                    .collect()
            };

            let mut out: Vec<Value> = Vec::new();
            if !no_count {
                out.push(Value::Int(parts.len() as i64));
            }
            out.extend(parts.into_iter().map(Value::Str));
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

        // ---------------- regular expressions ----------------
        // AutoIt uses PCRE; `crate::regexp` implements the same surface with a
        // pure-Rust engine, so these behave identically on every platform.
        "stringregexp" => {
            let subject = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let pattern = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
            let flag = args.get(2).map(|v| v.to_int()).unwrap_or(0);
            let offset = args.get(3).map(|v| v.to_int()).unwrap_or(1);

            let re = match crate::regexp::compile(&pattern) {
                Ok(re) => re,
                Err(e) => {
                    // 2 = bad pattern, @extended = offset of the error.
                    rt.set_error_value(2, e.offset as i64);
                    return Ok(Some(Value::Int(0)));
                }
            };
            let Some(start) = crate::regexp::char_offset_to_byte(&subject, offset) else {
                rt.set_error_value(1, 0);
                return Ok(Some(Value::Int(0)));
            };
            let tail = &subject[start..];

            match flag {
                // $STR_REGEXPMATCH — "does it match?"
                0 => {
                    let hit = re.is_match(tail);
                    rt.set_error_value(0, 0);
                    Value::Int(i64::from(hit))
                }
                // $STR_REGEXPARRAYMATCH — captured groups of the first match.
                1 => match re.captures(tail) {
                    None => {
                        rt.set_error_value(1, 0);
                        Value::Int(0)
                    }
                    Some(caps) => {
                        let end = caps.get(0).map(|m| m.end()).unwrap_or(0);
                        rt.set_error_value(
                            0,
                            crate::regexp::byte_to_char_offset(tail, end) as i64,
                        );
                        let mut out: Vec<Value> = Vec::new();
                        if re.captures_len() == 1 {
                            // No capturing groups: the match itself is returned.
                            out.push(Value::Str(
                                caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string(),
                            ));
                        } else {
                            for i in 1..re.captures_len() {
                                out.push(Value::Str(
                                    caps.get(i).map(|m| m.as_str()).unwrap_or("").to_string(),
                                ));
                            }
                        }
                        Value::array(out)
                    }
                },
                // $STR_REGEXPARRAYFULLMATCH — full match first, then groups.
                2 => match re.captures(tail) {
                    None => {
                        rt.set_error_value(1, 0);
                        Value::Int(0)
                    }
                    Some(caps) => {
                        let end = caps.get(0).map(|m| m.end()).unwrap_or(0);
                        rt.set_error_value(
                            0,
                            crate::regexp::byte_to_char_offset(tail, end) as i64,
                        );
                        let mut out: Vec<Value> = Vec::new();
                        for i in 0..re.captures_len() {
                            out.push(Value::Str(
                                caps.get(i).map(|m| m.as_str()).unwrap_or("").to_string(),
                            ));
                        }
                        Value::array(out)
                    }
                },
                // $STR_REGEXPARRAYGLOBALMATCH — every match.
                3 => {
                    let all: Vec<Value> = re
                        .find_iter(tail)
                        .map(|m| Value::Str(m.as_str().to_string()))
                        .collect();
                    if all.is_empty() {
                        rt.set_error_value(1, 0);
                        Value::Int(0)
                    } else {
                        rt.set_error_value(0, 0);
                        Value::array(all)
                    }
                }
                // $STR_REGEXPARRAYGLOBALFULLMATCH — every match with groups.
                4 => {
                    let all: Vec<Value> = re
                        .captures_iter(tail)
                        .map(|caps| {
                            let mut inner: Vec<Value> = Vec::new();
                            for i in 0..re.captures_len() {
                                inner.push(Value::Str(
                                    caps.get(i).map(|m| m.as_str()).unwrap_or("").to_string(),
                                ));
                            }
                            Value::array(inner)
                        })
                        .collect();
                    if all.is_empty() {
                        rt.set_error_value(1, 0);
                        Value::Int(0)
                    } else {
                        rt.set_error_value(0, 0);
                        Value::array(all)
                    }
                }
                other => {
                    rt.set_error_value(2, 0);
                    return Err(RuntimeError::Unsupported {
                        what: format!("StringRegExp flag {other} (expected 0..4)"),
                        span: Some(span),
                    });
                }
            }
        }
        "stringregexpreplace" => {
            let subject = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            let pattern = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
            let replacement = args.get(2).map(|v| v.to_autoit_string()).unwrap_or_default();
            let count = args.get(3).map(|v| v.to_int()).unwrap_or(0);

            let re = match crate::regexp::compile(&pattern) {
                Ok(re) => re,
                Err(e) => {
                    rt.set_error_value(2, e.offset as i64);
                    return Ok(Some(Value::Str(subject)));
                }
            };
            let rep = crate::regexp::translate_replacement(&replacement);
            let limit = if count > 0 { count as usize } else { usize::MAX };
            let performed = re.find_iter(&subject).take(limit).count();
            let out = if count > 0 {
                re.replacen(&subject, count as usize, rep.as_str()).to_string()
            } else {
                re.replace_all(&subject, rep.as_str()).to_string()
            };
            // @extended reports how many replacements were made.
            rt.set_error_value(0, performed as i64);
            Value::Str(out)
        }

        // ---------------- arrays / maps ----------------
        "ubound" => Value::Int(ubound(
            args.first().unwrap_or(&Value::Null),
            args.get(1).map(|v| v.to_int()).unwrap_or(1),
        )),
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
            // `Binary("0x00204060")` hex-decodes; a plain string contributes
            // its own bytes; a number its 64-bit little-endian image.
            match args.first() {
                Some(Value::Binary(_)) => args[0].clone(),
                Some(Value::Int(i)) => Value::Binary(Rc::new(i.to_le_bytes().to_vec())),
                Some(Value::Float(f)) => Value::Binary(Rc::new(f.to_le_bytes().to_vec())),
                other => {
                    let raw = other.map(|v| v.to_autoit_string()).unwrap_or_default();
                    match decode_hex_string(&raw) {
                        Some(bytes) => Value::Binary(Rc::new(bytes)),
                        None => Value::Binary(Rc::new(raw.into_bytes())),
                    }
                }
            }
        }
        "binarytostring" => {
            let bytes = match args.first() {
                Some(Value::Binary(b)) => b.as_ref().clone(),
                Some(other) => other.to_autoit_string().into_bytes(),
                None => Vec::new(),
            };
            Value::Str(decode_bytes(&bytes, arg_flag(args, 1)))
        }
        "stringtobinary" => {
            let raw = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
            Value::Binary(Rc::new(encode_bytes(&raw, arg_flag(args, 1))))
        }
        "binarymid" => {
            let bytes = match args.first() {
                Some(Value::Binary(b)) => b.as_ref().clone(),
                _ => Vec::new(),
            };
            let start = args.get(1).map(|v| v.to_int()).unwrap_or(1).max(1) as usize - 1;
            let len = args.get(2).map(|v| v.to_int()).unwrap_or(1).max(0) as usize;
            let end = (start + len).min(bytes.len());
            let slice = if start < bytes.len() { bytes[start..end].to_vec() } else { Vec::new() };
            Value::Binary(Rc::new(slice))
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
        // Only calls that are pure *interpreter state* are neutralised here.
        // Everything with an external effect (files, environment, processes,
        // registry, COM, DllCall, GUI, console) belongs to a `Platform`
        // implementation: a silent stub in this table would both invent a
        // value and shadow the platform that could answer properly.
        "opt" | "autoitsetoption" => Value::Int(1),
        // `Ptr`/`HWnd` only retype a value as a handle; the emulation keeps
        // handles as plain integers, so the conversion is the identity.
        "ptr" | "hwnd" => Value::Int(args.first().map(|a| a.to_int()).unwrap_or(0)),
        "vargettype" => Value::Str(var_get_type(args.first()).to_string()),
        // AutoIt runs these when the process exits; see `Runtime::exit_handlers`.
        "onautoitexitregister" => {
            let name = args.first().map(|a| a.to_autoit_string()).unwrap_or_default();
            if name.is_empty() {
                Value::Int(0)
            } else {
                rt.register_exit_handler(name);
                Value::Int(1)
            }
        }
        // `AdlibRegister`/`AdlibUnRegister` are core language builtins, not an
        // operating-system interface, so they live here rather than in a
        // platform layer. AutoIt calls the callbacks whenever the script goes
        // idle; see `Runtime::adlib_handlers`.
        "adlibregister" => {
            let name = args.first().map(|a| a.to_autoit_string()).unwrap_or_default();
            let interval = args.get(1).map(|a| a.to_int()).unwrap_or(250);
            if name.is_empty() || interval <= 0 || !rt.has_function(&name) {
                Value::Int(0)
            } else {
                Value::Int(i64::from(rt.register_adlib(name, interval)))
            }
        }
        "adlibunregister" => {
            let name = args.first().map(|a| a.to_autoit_string());
            let name = name.as_deref().filter(|n| !n.is_empty());
            Value::Int(i64::from(rt.unregister_adlib(name)))
        }
        "onautoitexitunregister" => {
            let name = args.first().map(|a| a.to_autoit_string()).unwrap_or_default();
            Value::Int(i64::from(rt.unregister_exit_handler(&name)))
        }
        // `Sleep` follows the execution profile: a *faithful* run really waits
        // (AutoIt semantics), while the deterministic deobfuscation profile
        // returns immediately, because nothing in a script's *result* depends
        // on how long it waited.
        "sleep" => {
            let millis = args.first().map(|v| v.to_int()).unwrap_or(0).max(0) as u64;
            match rt.profile().sleep {
                SleepPolicy::Skip => {}
                SleepPolicy::Real => std::thread::sleep(Duration::from_millis(millis)),
                SleepPolicy::Capped(max) => {
                    std::thread::sleep(Duration::from_millis(millis).min(max));
                }
            }
            Value::Int(0)
        }

        _ => return Ok(None),
    };
    Ok(Some(v))
}

/// Argument as a flag integer.
fn arg_flag(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.to_int()).unwrap_or(0)
}

/// Decode AutoIt's `"0x..."` hex literal into bytes, or `None` when the string
/// is not such a literal (an odd length or a non-hex character).
fn decode_hex_string(s: &str) -> Option<Vec<u8>> {
    let t = s.trim();
    let body = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))?;
    if body.is_empty() || body.len() % 2 != 0 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() / 2);
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() + 1 && i + 1 <= bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

/// Bytes -> text: 1 = ANSI, 2 = UTF-16LE, 3 = UTF-16BE, 4 = UTF-8 (default).
fn decode_bytes(bytes: &[u8], flag: i64) -> String {
    match flag {
        1 => bytes.iter().map(|b| *b as char).collect(),
        2 => decode_utf16(bytes, true),
        3 => decode_utf16(bytes, false),
        // AutoIt's default for BinaryToString is UTF-8 in practice for the
        // scripts we evaluate; 4 is explicit UTF-8.
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Text -> bytes, mirroring [`decode_bytes`].
fn encode_bytes(s: &str, flag: i64) -> Vec<u8> {
    match flag {
        1 => s.chars().map(|c| c as u8).collect(),
        2 => s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
        3 => s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => s.as_bytes().to_vec(),
    }
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
/// AutoIt's `VarGetType` name for a value.
///
/// AutoIt distinguishes the two integer widths by what the value fits in, and
/// calls the two "no value" keywords `Keyword`.
fn var_get_type(v: Option<&Value>) -> &'static str {
    match v {
        None => "Keyword",
        Some(Value::Int(i)) => {
            if i32::try_from(*i).is_ok() {
                "Int32"
            } else {
                "Int64"
            }
        }
        Some(Value::Float(_)) => "Double",
        Some(Value::Str(_)) => "String",
        Some(Value::Binary(_)) => "Binary",
        Some(Value::Bool(_)) => "Bool",
        Some(Value::Array(_)) => "Array",
        Some(Value::Map(_)) => "Map",
        Some(Value::FuncRef(_)) => "Function",
        Some(Value::Null) | Some(Value::Default) => "Keyword",
    }
}

/// `UBound($a[, $dim])`: the length of `$dim` (1-based), or — for `$dim` 0 —
/// how many dimensions the array has. AutoIt nests extra dimensions as arrays
/// of arrays, so dimension 2 is the length of a row.
fn ubound(value: &Value, dim: i64) -> i64 {
    let mut cur = value.clone();
    if dim <= 0 {
        let mut count = 0;
        loop {
            match &cur {
                Value::Array(inner) => {
                    count += 1;
                    let first = inner.borrow().first().cloned();
                    match first {
                        Some(v) => cur = v,
                        None => break,
                    }
                }
                _ => break,
            }
        }
        return count;
    }
    for _ in 1..dim {
        cur = match &cur {
            Value::Array(inner) => match inner.borrow().first() {
                Some(first) => first.clone(),
                None => return 0,
            },
            _ => return 0,
        };
    }
    match &cur {
        Value::Array(a) => a.borrow().len() as i64,
        Value::Map(m) => m.borrow().len() as i64,
        Value::Str(s) => s.chars().count() as i64,
        _ => 0,
    }
}
