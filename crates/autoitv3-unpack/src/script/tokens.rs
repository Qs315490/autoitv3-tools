//! Turning a compiled script's token stream back into `.au3` source.
//!
//! A `>>>AUTOIT SCRIPT<<<` entry is not source and not really bytecode: it is
//! a flat stream of *tokens*, each a one-byte opcode optionally followed by
//! payload. Together they spell the source line by line, with `0x7F` ending a
//! line, so recovering the script is a matter of walking the stream and
//! printing what each token stands for.
//!
//! Two details carry most of the information:
//!
//! * names (variables, functions, macros, strings) are length-prefixed and
//!   XOR-obfuscated with the length itself — see `Tokens::xored_string`;
//! * keywords and built-in functions are *indices* into
//!   [`super::symbols`], so the table has to match the compiler's.
//!
//! Indentation is not stored: it is reconstructed from the keywords, which is
//! why `apply_keyword_indent` exists and why an unknown keyword would need
//! care. This is a port of the MIT-licensed AutoIt-Ripper's `opcodes.py`.

use autoitv3_i18n::{msg, tr};

use super::symbols::{canonical_function, canonical_keyword, canonical_macro, FUNCTIONS};

/// Why a token stream could not be turned into source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenError(pub String);

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TokenError {
    fn new(what: impl Into<String>) -> Self {
        TokenError(what.into())
    }
}

/// Deassemble a token stream into AutoIt source.
///
/// The result uses `\r\n` line endings and tab indentation, matching what the
/// compiler's own output looks like.
pub fn deassemble(data: &[u8]) -> Result<String, TokenError> {
    let mut stream = Tokens::new(data);
    let lines = stream.u32()?;
    let mut out = String::new();
    let mut line: Vec<String> = Vec::new();
    let mut produced = 0u32;

    while produced < lines {
        let opcode = stream.u8()?;
        if opcode == 0x7F {
            produced += 1;
            if stream.indent > 0 {
                out.push_str(&"\t".repeat(stream.indent as usize));
            }
            out.push_str(&line.join(" "));
            out.push_str("\r\n");
            line.clear();
            stream.indent = stream.next_indent;
        } else {
            line.push(stream.item(opcode)?);
        }
    }
    Ok(out)
}

/// A cursor over the token bytes plus the indentation state the keywords
/// drive.
struct Tokens<'a> {
    data: &'a [u8],
    position: usize,
    /// The indentation of the line being built.
    indent: i32,
    /// The indentation the next line starts with.
    next_indent: i32,
}

impl<'a> Tokens<'a> {
    fn new(data: &'a [u8]) -> Self {
        Tokens { data, position: 0, indent: 0, next_indent: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], TokenError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| TokenError::new(tr("the token stream runs off the end")))?;
        let slice = self
            .data
            .get(self.position..end)
            .ok_or_else(|| TokenError::new(tr("the token stream runs off the end")))?;
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, TokenError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, TokenError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, TokenError> {
        Ok(self.u32()? as i32)
    }

    fn i64(&mut self) -> Result<i64, TokenError> {
        Ok(self.u64()? as i64)
    }

    fn u64(&mut self) -> Result<u64, TokenError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn f64(&mut self) -> Result<f64, TokenError> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// The next opcode byte, without consuming it.
    fn peek(&self) -> Option<u8> {
        self.data.get(self.position).copied()
    }

    /// A length-prefixed, XOR-obfuscated UTF-16 string.
    ///
    /// The prefix is a character count; every following `u16` is XORed with
    /// that same count, and the result is UTF-16 little-endian. The reference
    /// rejects a count larger than the *whole* buffer as a cheap sanity check;
    /// here the check is against what is left, which is stricter.
    fn xored_string(&mut self) -> Result<String, TokenError> {
        let key = self.u32()?;
        let bytes = key as usize * 2;
        let raw = self
            .take(bytes)
            .map_err(|_| TokenError::new(msg!("a string of {key} characters runs off the end", key = key)))?;
        let mut units = Vec::with_capacity(key as usize);
        for pair in raw.chunks_exact(2) {
            let word = u16::from_le_bytes([pair[0], pair[1]]);
            units.push(word ^ (key as u16));
        }
        String::from_utf16(&units)
            .map_err(|_| TokenError::new(tr("a string is not valid UTF-16")))
    }

    /// One token, rendered as the text it stands for.
    fn item(&mut self, opcode: u8) -> Result<String, TokenError> {
        Ok(match opcode {
            // Keyword by index. The reference's bound check is off by one and
            // tolerates negatives (Python indexing); neither is worth copying.
            0x00 => {
                let index = self.i32()?;
                let index = usize::try_from(index)
                    .ok()
                    .filter(|i| *i < super::symbols::KEYWORDS.len())
                    .ok_or_else(|| TokenError::new(tr("a keyword index is out of range")))?;
                let keyword = super::symbols::KEYWORDS[index];
                self.apply_keyword_indent(keyword);
                keyword.to_string()
            }
            // Built-in function by index.
            0x01 => {
                let index = self.i32()?;
                let index = usize::try_from(index)
                    .ok()
                    .filter(|i| *i < FUNCTIONS.len())
                    .ok_or_else(|| TokenError::new(tr("a function index is out of range")))?;
                FUNCTIONS[index].to_string()
            }
            // The integer literals are *signed*: `-1` in the source is an
            // Int32 token holding 0xffff_ffff, and printing the raw word turns
            // it into 4294967295 — a number the script never wrote, and one
            // that makes whatever re-parses the text read something else (a
            // real sample's `$n + 0xffff_ffff` became "four billion elements").
            0x05 => self.i32()?.to_string(),
            0x10 => self.i64()?.to_string(),
            0x20 => python_float_repr(self.f64()?),
            // Keyword by name.
            0x30 => {
                let name = self.xored_string()?;
                let shown = format!("{name:?}");
                let keyword = canonical_keyword(&name)
                    .ok_or_else(|| TokenError::new(msg!("unknown keyword {name}", name = shown)))?;
                self.apply_keyword_indent(keyword);
                keyword.to_string()
            }
            // A name that may or may not be a built-in function.
            0x31 => {
                let name = self.xored_string()?;
                canonical_function(&name).unwrap_or(&name).to_string()
            }
            0x32 => {
                let name = self.xored_string()?;
                format!("@{}", canonical_macro(&name).unwrap_or(&name))
            }
            0x33 => format!("${}", self.xored_string()?),
            0x34 => self.xored_string()?,
            0x35 => format!(".{}", self.xored_string()?),
            0x36 => {
                let text = self.xored_string()?;
                format!("\"{}\"", text.replace('"', "\"\""))
            }
            0x37 => self.xored_string()?,
            _ => {
                let shown = format!("{opcode:#04x}");
                operator(opcode)
                    .ok_or_else(|| TokenError::new(msg!("unsupported opcode {opcode}", opcode = shown)))?
                    .to_string()
            }
        })
    }

    /// The reference's `apply_keyword_indent`: keep the indentation level.
    ///
    /// The one indirect case is `Then`, which closes a single-line `If` only
    /// when it is *not* followed by the end-of-line token.
    fn apply_keyword_indent(&mut self, keyword: &str) {
        match keyword {
            "While" | "Do" | "For" | "Select" | "Switch" | "Func" | "If" => {
                self.next_indent += 1;
            }
            "Case" | "Else" | "ElseIf" => self.indent -= 1,
            "WEnd" | "Until" | "Next" | "EndSelect" | "EndSwitch" | "EndFunc" | "EndIf" => {
                self.next_indent -= 1;
                self.indent -= 1;
            }
            _ => {}
        }
        if keyword == "Then" && self.peek() != Some(0x7F) {
            self.next_indent -= 1;
        }
        if keyword == "EndFunc" {
            self.next_indent = 0;
        }
    }
}

/// The fixed spellings of the operator opcodes.
fn operator(opcode: u8) -> Option<&'static str> {
    Some(match opcode {
        0x40 => ",",
        0x41 => "=",
        0x42 => ">",
        0x43 => "<",
        0x44 => "<>",
        0x45 => ">=",
        0x46 => "<=",
        0x47 => "(",
        0x48 => ")",
        0x49 => "+",
        0x4A => "-",
        0x4B => "/",
        0x4C => "*",
        0x4D => "&",
        0x4E => "[",
        0x4F => "]",
        0x50 => "==",
        0x51 => "^",
        0x52 => "+=",
        0x53 => "-=",
        0x54 => "/=",
        0x55 => "*=",
        0x56 => "&=",
        0x57 => "?",
        0x58 => ":",
        _ => return None,
    })
}

/// Format a float the way Python's `repr` would, because that is the spelling
/// the reference implementation emits and the two are not interchangeable.
///
/// Rust's `{:e}` already gives the shortest round-tripping digits; the work
/// left is Python's *presentation* choice: fixed notation while the decimal
/// point sits in `[-3, 16]` and scientific outside it, always with an exponent
/// sign and at least two exponent digits, and a trailing `.0` on an integral
/// fixed value.
fn python_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }

    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("Rust always formats f64 with an exponent in `:e` mode");
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.trim_start_matches('-');
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // `digits` is the significand, `decimal_point` the exponent of its first
    // digit: `0.digits * 10^decimal_point`.
    let decimal_point: i32 = exponent
        .parse::<i32>()
        .expect("the exponent of a `:e` formatted float is an integer")
        + 1;

    let mut body = if decimal_point <= -4 || decimal_point > 16 {
        let mut out = String::new();
        out.push(digits.chars().next().expect("a non-zero float has a digit"));
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let power = decimal_point - 1;
        out.push(if power < 0 { '-' } else { '+' });
        let magnitude = power.unsigned_abs();
        if magnitude < 10 {
            out.push('0');
        }
        out.push_str(&magnitude.to_string());
        out
    } else if decimal_point <= 0 {
        format!("0.{}{}", "0".repeat(-decimal_point as usize), digits)
    } else if decimal_point as usize >= digits.len() {
        format!(
            "{}{}.0",
            digits,
            "0".repeat(decimal_point as usize - digits.len())
        )
    } else {
        format!(
            "{}.{}",
            &digits[..decimal_point as usize],
            &digits[decimal_point as usize..]
        )
    };
    if negative {
        body.insert(0, '-');
    }
    body
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/script_tokens.rs"]
mod tests;
