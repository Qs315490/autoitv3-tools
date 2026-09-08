//! Hand-written lexer for AutoIt v3 source text.
//!
//! Produces a flat `Vec<Token>`. The parser consumes these tokens.
//! This module is deliberately state-free (a single `Lexer` struct) so it
//! is easy to extend with new token kinds later.

use crate::span::{Pos, Span};
use crate::token::{Token, TokenKind};

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
}

pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let mut lexer = Lexer {
        src: src.as_bytes(),
        idx: 0,
        line: 1,
        col: 1,
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


    fn eat_ws_and_comments(&mut self) {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r')) {
                self.bump();
            }
            // `;` comments extend to end of line (including `\r`).
            if self.peek() == Some(b';') {
                while let Some(c) = self.peek() {
                    if c == b'\n' {
                        break;
                    }
                    self.bump();
                }
                continue; // allow trailing ws before a newline
            }
            break;
        }
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        self.eat_ws_and_comments();

        let start = self.pos();
        let Some(c) = self.peek() else {
            return Ok(Token::new(TokenKind::Eof, Span::new(start, start)));
        };

        let kind = match c {
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
            b'#' => self.preproc(start),
            b'$' => self.var(start),
            b'@' => self.r#macro(start),
            b'"' => self.str(start)?,
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
                    msg: format!("unexpected character '{}'", c as char),
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
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                s.push(c as char);
                self.bump();
            } else {
                break;
            }
        }
        // Keep the rest of the directive line verbatim (arguments such as
        // `<file.au3>`, `Icon\app.ico`, `=value`). AutoIt directives are
        // line-oriented: everything up to the newline belongs to the directive.
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            s.push(c as char);
            self.bump();
        }
        TokenKind::Preproc(s.trim_end().to_string())
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

    fn str(&mut self, start: Pos) -> Result<TokenKind, LexError> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(LexError {
                        msg: "unterminated string literal".into(),
                        pos: start,
                    })
                }
                Some(b'"') => {
                    self.bump();
                    // AutoIt escapes a quote by doubling it: `""`.
                    if self.peek() == Some(b'"') {
                        self.bump();
                        s.push('"');
                        continue;
                    }
                    break;
                }
                Some(_) => {
                    s.push(self.bump().unwrap() as char);
                }
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
fn keyword(s: &str) -> Option<TokenKind> {
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
        "exit" => Exit,
        "with" => With,
        "endwith" => EndWith,
        "and" => And,
        "or" => Or,
        "not" => Not,
        "in" => In,
        "enum" => Enum,
        "true" => True,
        "false" => False,
        "default" => Default,
        "null" => Null,
        _ => return None,
    })
}
