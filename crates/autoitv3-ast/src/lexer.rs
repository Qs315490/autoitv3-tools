//! Hand-written lexer for AutoIt v3 source text.
//!
//! Produces a flat `Vec<Token>`. The parser consumes these tokens.
//! This module is deliberately state-free (a single `Lexer` struct) so it
//! is easy to extend with new token kinds later.

use crate::span::{Pos, Span};
use crate::token::{Token, TokenKind};
use autoitv3_i18n::{msg, tr};

/// A lexical error with the position where it occurred.
#[derive(Debug, Clone)]
pub struct LexError {
    pub msg: String,
    pub pos: Pos,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.msg, self.pos)
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    idx: usize,
    line: u32,
    col: u32,
    /// Set when a `_` line continuation was consumed: the next newline
    /// belongs to the continued statement and must not become a separator.
    pending_continuation: bool,
}

pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let mut lexer = Lexer {
        src: src.as_bytes(),
        idx: 0,
        line: 1,
        col: 1,
        pending_continuation: false,
    };
    let mut out = Vec::new();
    loop {
        let tok = lexer.next_token()?;
        let is_eof = tok.kind == TokenKind::Eof;
        out.push(tok);
        if is_eof {
            break;
        }
    }
    Ok(out)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.idx).copied()
    }


    fn pos(&self) -> Pos {
        Pos::new(self.line, self.col)
    }

    /// Append the character at the cursor to `out`, decoding UTF-8.
    ///
    /// The lexer walks bytes, which is right for keywords and identifiers but
    /// wrong for anything that carries text through to the output: casting a
    /// byte to `char` would turn `济` (`e6 b5 8e`) into `æµ` and re-encode that
    /// as UTF-8, so a comment or directive written in Chinese came out as
    /// mojibake. A byte that starts no valid sequence becomes U+FFFD and costs
    /// one byte, so the cursor always moves.
    fn push_char(&mut self, out: &mut String) {
        let Some(first) = self.peek() else {
            return;
        };
        if first < 0x80 {
            out.push(first as char);
            self.bump();
            return;
        }
        let rest = &self.src[self.idx..];
        let len = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        match rest
            .get(..len)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| text.chars().next())
        {
            Some(ch) => {
                out.push(ch);
                self.idx += len;
                // Columns count bytes, as they did before this decoded a run at
                // a time, so spans do not move.
                self.col += len as u32;
            }
            None => {
                out.push(char::REPLACEMENT_CHARACTER);
                self.bump();
            }
        }
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.idx += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }


    /// Skip spaces, tabs and carriage returns, reporting whether any were
    /// skipped. `;` comments are NOT dropped here; they become `Comment`
    /// tokens in [`Lexer::next_token`] so the parser can preserve them.
    fn eat_ws(&mut self) -> bool {
        let before = self.idx;
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r')) {
            self.bump();
        }
        self.idx != before
    }

    /// True when the `_` at the current position ends the line, i.e. it is a
    /// line continuation. AutoIt requires the `_` to be preceded by a blank
    /// (the caller checks that) and followed by optional blanks, an optional
    /// `;` comment and then the newline.
    fn underscore_continues(&self) -> bool {
        debug_assert_eq!(self.peek(), Some(b'_'));
        let mut i = self.idx + 1;
        while matches!(self.src.get(i), Some(b' ' | b'\t' | b'\r')) {
            i += 1;
        }
        matches!(self.src.get(i), Some(b'\n') | Some(b';'))
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        loop {
            let had_blank = self.eat_ws();

            // A `_` preceded by a blank and followed by end-of-line is a line
            // continuation: it joins the next line to this statement. Consume
            // it and remember to swallow the newline (any `;` comment that
            // trails the underscore is still emitted as a comment token).
            if had_blank && self.peek() == Some(b'_') && self.underscore_continues() {
                self.bump(); // '_'
                self.eat_ws();
                self.pending_continuation = true;
                continue;
            }

            // The newline that terminates a continued line is not a statement
            // separator.
            if self.peek() == Some(b'\n') && self.pending_continuation {
                self.pending_continuation = false;
                self.bump();
                continue;
            }
            break;
        }

        let start = self.pos();
        let Some(c) = self.peek() else {
            return Ok(Token::new(TokenKind::Eof, Span::new(start, start)));
        };

        let kind = match c {
            // `;` comment extends to end of line (including any `\r`). We keep
            // the text after the `;` and let the newline be a separate token.
            b';' => {
                self.bump(); // consume ';'
                let mut s = String::new();
                while let Some(c) = self.peek() {
                    if c == b'\n' {
                        break;
                    }
                    self.push_char(&mut s);
                }
                TokenKind::Comment {
                    text: s.trim_end().to_string(),
                    block: false,
                }
            }
            b'\n' => {
                self.bump();
                TokenKind::Newline
            }
            b':' => {
                self.bump();
                TokenKind::Colon
            }
            b'(' => {
                self.bump();
                TokenKind::LParen
            }
            b')' => {
                self.bump();
                TokenKind::RParen
            }
            b'[' => {
                self.bump();
                TokenKind::LBracket
            }
            b']' => {
                self.bump();
                TokenKind::RBracket
            }
            b',' => {
                self.bump();
                TokenKind::Comma
            }
            b'.' => {
                self.bump();
                TokenKind::Dot
            }
            b'#' => self.preproc(start),
            b'$' => self.var(start),
            b'@' => self.r#macro(start),
            b'"' => self.str(b'"', start)?,
            b'\'' => self.str(b'\'', start)?,
            b'0'..=b'9' => self.number(start),
            b'+' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::PlusAssign
                } else {
                    TokenKind::Plus
                }
            }
            b'-' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::MinusAssign
                } else {
                    TokenKind::Minus
                }
            }
            b'*' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::StarAssign
                } else {
                    TokenKind::Star
                }
            }
            b'/' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::SlashAssign
                } else {
                    TokenKind::Slash
                }
            }
            b'^' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::CaretAssign
                } else {
                    TokenKind::Caret
                }
            }
            b'&' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::AmpAssign
                } else {
                    TokenKind::Amp
                }
            }
            b'?' => {
                self.bump();
                TokenKind::Question
            }
            b'=' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::Eq
                } else {
                    TokenKind::Assign
                }
            }
            b'<' => {
                self.bump();
                if self.peek() == Some(b'>') {
                    self.bump();
                    TokenKind::NotEq
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::Le
                } else {
                    TokenKind::Lt
                }
            }
            b'>' => {
                self.bump();
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::Ge
                } else {
                    TokenKind::Gt
                }
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => self.ident(start),
            _ => {
                return Err(LexError {
                    msg: msg!("unexpected character '{c}'", c = c as char),
                    pos: start,
                })
            }
        };

        let end = self.pos();
        Ok(Token::new(kind, Span::new(start, end)))
    }

    fn preproc(&mut self, _start: Pos) -> TokenKind {
        self.bump(); // '#'
        let mut s = String::new();
        // `-` is part of directive names such as `#comments-start` and
        // `#include-once`; the rest of the line is appended verbatim below
        // either way, so widening this only affects name comparisons.
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                s.push(c as char);
                self.bump();
            } else {
                break;
            }
        }

        // `#cs ... #ce` (long form: `#comments-start ... #comments-end`) is a
        // block comment, not a directive: consume the whole block and hand it
        // to the parser as a single comment token so the pretty-printer can
        // reproduce it verbatim.
        let name = s.to_ascii_lowercase();
        if name == "cs" || name == "comments-start" {
            return self.block_comment(s);
        }

        // Keep the rest of the directive line verbatim (arguments such as
        // `<file.au3>`, `Icon\app.ico`, `=value`). AutoIt directives are
        // line-oriented: everything up to the newline belongs to the directive.
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            self.push_char(&mut s);
        }
        TokenKind::Preproc(s.trim_end().to_string())
    }

    /// Consume a `#cs ... #ce` block, returning it as one block comment token.
    ///
    /// An unterminated block runs to end of file, matching AutoIt's behaviour
    /// of treating the remainder of the script as commented out.
    fn block_comment(&mut self, opener: String) -> TokenKind {
        let mut raw = String::from("#");
        raw.push_str(&opener);
        self.take_line(&mut raw);
        loop {
            if let Some(end) = self.block_closer_end() {
                while self.idx < end {
                    self.push_char(&mut raw);
                }
                self.take_line(&mut raw);
                break;
            }
            if self.peek().is_none() {
                break;
            }
            self.take_line(&mut raw);
        }
        TokenKind::Comment {
            text: raw.trim_end().to_string(),
            block: true,
        }
    }

    /// Index just past a `#ce` / `#comments-end` directive at the start of the
    /// current line, if the cursor is on such a line.
    fn block_closer_end(&self) -> Option<usize> {
        let mut i = self.idx;
        while matches!(self.src.get(i), Some(b' ' | b'\t' | b'\r')) {
            i += 1;
        }
        if self.src.get(i) != Some(&b'#') {
            return None;
        }
        let rest = self.src.get(i + 1..)?;
        for closer in [&b"ce"[..], &b"comments-end"[..]] {
            if rest.len() >= closer.len() && rest[..closer.len()].eq_ignore_ascii_case(closer) {
                let after = rest.get(closer.len()).copied();
                let boundary = !matches!(after, Some(c) if c.is_ascii_alphanumeric() || c == b'_');
                if boundary {
                    return Some(i + 1 + closer.len());
                }
            }
        }
        None
    }

    /// Append the rest of the current line (newline included) to `out`.
    fn take_line(&mut self, out: &mut String) {
        while let Some(c) = self.peek() {
            self.push_char(out);
            if c == b'\n' {
                break;
            }
        }
    }

    fn var(&mut self, _start: Pos) -> TokenKind {
        self.bump(); // '$'
        let mut s = String::from("$");
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                s.push(c as char);
                self.bump();
            } else {
                break;
            }
        }
        TokenKind::Var(s)
    }

    fn r#macro(&mut self, _start: Pos) -> TokenKind {
        self.bump(); // '@'
        let mut s = String::from("@");
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                s.push(c as char);
                self.bump();
            } else {
                break;
            }
        }
        TokenKind::Macro(s)
    }

    /// Read a string literal. AutoIt accepts both `"..."` and `'...'`, and
    /// escapes the active quote by doubling it (`""` or `''`).
    fn str(&mut self, quote: u8, start: Pos) -> Result<TokenKind, LexError> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(LexError {
                        msg: tr("unterminated string literal").into(),
                        pos: start,
                    })
                }
                Some(c) if c == quote => {
                    self.bump();
                    if self.peek() == Some(quote) {
                        self.bump();
                        s.push(quote as char);
                        continue;
                    }
                    break;
                }
                Some(_) => self.push_char(&mut s),
            }
        }
        Ok(TokenKind::Str(s))
    }

    fn number(&mut self, _start: Pos) -> TokenKind {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_hexdigit() || c == b'.' || c == b'x' || c == b'X' {
                s.push(c as char);
                self.bump();
            } else if c == b'f' || c == b'F' {
                // float suffix or the tail of 0x...
                s.push(c as char);
                self.bump();
            } else {
                break;
            }
        }
        TokenKind::Number(s)
    }

    fn ident(&mut self, _start: Pos) -> TokenKind {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                s.push(c as char);
                self.bump();
            } else {
                break;
            }
        }
        match keyword(&s) {
            Some(k) => k,
            None => TokenKind::IdentTok(s),
        }
    }
}

/// Map a lowercase keyword string to its token kind.
///
/// `pub(crate)` so `vocab`'s test can check the two against each other; this
/// match and the vocabulary table are the same list written two ways.
pub(crate) fn keyword(s: &str) -> Option<TokenKind> {
    use TokenKind::*;
    Some(match s.to_ascii_lowercase().as_str() {
        "func" => Func,
        "endfunc" => EndFunc,
        "local" => Local,
        "global" => Global,
        "const" => Const,
        "dim" => Dim,
        "redim" => ReDim,
        "static" => Static,
        "byref" => ByRef,
        "if" => If,
        "elseif" => ElseIf,
        "else" => Else,
        "endif" => EndIf,
        "then" => Then,
        "for" => For,
        "to" => To,
        "step" => Step,
        "next" => Next,
        "while" => While,
        "wend" => WEnd,
        "do" => Do,
        "until" => Until,
        "select" => Select,
        "case" => Case,
        "endselect" => EndSelect,
        "switch" => Switch,
        "endswitch" => EndSwitch,
        "return" => Return,
        "exitloop" => ExitLoop,
        "continueloop" => ContinueLoop,
        "continuecase" => ContinueCase,
        "exit" => Exit,
        "with" => With,
        "endwith" => EndWith,
        "and" => And,
        "or" => Or,
        "not" => Not,
        "in" => In,
        "enum" => Enum,
        "volatile" => Volatile,
        "true" => True,
        "false" => False,
        "default" => Default,
        "null" => Null,
        _ => return None,
    })
}
