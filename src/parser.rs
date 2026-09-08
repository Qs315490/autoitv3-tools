//! Recursive-descent parser for AutoIt v3.
//!
//! Consumes the token stream from the lexer and produces a `Program` AST.
//! The parser is line-oriented where AutoIt requires it (statement
//! separation via newline/colon), but expression parsing is fully
//! recursive-descent with precedence climbing.
//!
//! Design goals:
//! - Clear, small, individually testable parse functions.
//! - Every produced node carries a `Span` for future tooling.
//! - Errors carry the offending `Span` so a debugger can report location.

use crate::ast::*;
use crate::span::{Pos, Span};
use crate::token::Token;
use crate::token::TokenKind;
use crate::token::TokenKind as TK;
use TK::*;

/// A parse error tied to a source location.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub msg: String,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.msg, self.span)
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

/// Convenience: lex then parse a whole program.
pub fn parse(src: &str) -> Result<Program, ParseError> {
    let tokens = crate::lexer::lex(src).map_err(|e| ParseError {
        msg: e.msg,
        span: Span::new(e.pos, e.pos),
    })?;
    let mut p = Parser::new(tokens);
    p.parse_program()
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    // ----- low level helpers -----

    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn peek_n(&self, n: usize) -> &Token {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)]
    }

    fn bump(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn at(&self, k: &TokenKind) -> bool {
        self.peek_kind() == k
    }

    fn at_any(&self, ks: &[&TokenKind]) -> bool {
        ks.contains(&self.peek_kind())
    }

    fn eat(&mut self, k: &TokenKind) -> Option<Token> {
        if self.at(k) {
            Some(self.bump())
        } else {
            None
        }
    }

    fn expect(&mut self, k: &TokenKind, what: &str) -> Result<Token, ParseError> {
        if let Some(t) = self.eat(k) {
            Ok(t)
        } else {
            Err(self.err_here(format!("expected {what}")))
        }
    }

    fn err_here(&self, msg: impl Into<String>) -> ParseError {
        let t = self.peek();
        ParseError {
            msg: msg.into(),
            span: t.span,
        }
    }

    /// True when we are at the start of an expression.
    fn at_expr_start(&self) -> bool {
        matches!(
            self.peek_kind(),
            Number(_) | Str(_) | True | False | Default | Null | Var(_) | Macro(_) | IdentTok(_)
                | Not | Minus | Plus | LParen
        )
    }

    // ----- statement separation -----

    /// Consume statement separators (newlines and colons) up to EOF.
    fn skip_seps(&mut self) {
        loop {
            match self.peek_kind() {
                Newline | Colon => {
                    self.bump();
                }
                _ => break,
            }
        }
    }

    // ----- top level -----

    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut items = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&Eof) {
                break;
            }
            items.push(self.parse_item()?);
        }
        Ok(Program { items })
    }

    fn parse_item(&mut self) -> Result<Item, ParseError> {
        let start = self.peek().span;
        let kind = match self.peek_kind() {
            Preproc(_) => {
                let t = self.bump();
                let TokenKind::Preproc(name) = t.kind else { unreachable!() };
                ItemKind::Directive(name)
            }
            Func => {
                let def = self.parse_func_def()?;
                ItemKind::Func(def)
            }
            _ => ItemKind::Stmt(self.parse_stmt()?),
        };
        let end = self.prev_span();
        Ok(Item {
            kind,
            span: Span::new(start.start, end.end),
        })
    }

    /// The span of the most recently consumed token (or the current one).
    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1).min(self.tokens.len() - 1)].span
    }

    fn parse_func_def(&mut self) -> Result<FuncDef, ParseError> {
        self.expect(&Func, "Func")?;
        let name = self.parse_ident()?;
        // Parameters: `( $a, $b = 1, ByRef $c )` — parentheses are optional.
        let mut params = Vec::new();
        if self.eat(&LParen).is_some() {
            if !self.at(&RParen) {
                loop {
                    params.push(self.parse_param()?);
                    if self.eat(&Comma).is_none() {
                        break;
                    }
                }
            }
            self.expect(&RParen, ")")?;
        }
        let mut body = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&EndFunc) {
                self.bump();
                break;
            }
            if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing EndFunc"));
            }
            body.push(self.parse_stmt()?);
        }
        Ok(FuncDef {
            name: name.clone(),
            params,
            body,
            span: Span::merge(name.span, self.prev_span()),
        })
    }

    fn parse_param(&mut self) -> Result<Param, ParseError> {
        let start = self.peek().span;
        let mut by_ref = false;
        if self.eat(&ByRef).is_some() {
            by_ref = true;
        }
        let name = self.parse_ident()?;
        let default = if self.eat(&Assign).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Param {
            name,
            by_ref,
            default,
            span: Span::new(start.start, self.prev_span().end),
        })
    }

    // ----- statements -----

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.peek().span;
        let kind = match self.peek_kind() {
            Local | Global | Const | Dim | Static | ReDim => self.parse_var_decl_stmt()?,
            Return => {
                self.bump();
                let e = if self.at_expr_start() {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                StmtKind::Return(e)
            }
            Exit => {
                self.bump();
                let e = if self.at_expr_start() {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                StmtKind::Exit(e)
            }
            ExitLoop | ContinueLoop => {
                self.bump();
                let e = if self.at_expr_start() {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                StmtKind::ExitLoop(e)
            }
            If => self.parse_if()?,
            While => self.parse_while()?,
            Do => self.parse_do_until()?,
            For => self.parse_for()?,
            Select => self.parse_select()?,
            Switch => self.parse_switch()?,
            With => self.parse_with()?,
            _ => StmtKind::Expr(self.parse_expr()?),
        };
        Ok(Stmt {
            kind,
            span: Span::new(start.start, self.prev_span().end),
        })
    }

    fn parse_var_decl_stmt(&mut self) -> Result<StmtKind, ParseError> {
        let kw = self.bump();
        let (kind, mut is_const) = match kw.kind {
            Local => (VarKind::Local, false),
            Global => (VarKind::Global, false),
            Dim => (VarKind::Dim, false),
            Static => (VarKind::Static, false),
            Const => (VarKind::Local, true),
            ReDim => (VarKind::Dim, true),
            _ => unreachable!(),
        };
        // `Local Const` / `Global Const` ordering.
        if self.eat(&Const).is_some() {
            is_const = true;
        }
        let mut vars = Vec::new();
        loop {
            vars.push(self.parse_var_decl_item()?);
            if self.eat(&Comma).is_none() {
                break;
            }
        }
        Ok(StmtKind::VarDecl(VarDecl {
            kind,
            is_const,
            vars,
        }))
    }

    fn parse_var_decl_item(&mut self) -> Result<VarDeclItem, ParseError> {
        let start = self.peek().span;
        let name = self.parse_ident()?;
        let mut dims = Vec::new();
        while self.eat(&LBracket).is_some() {
            // Bracket content may be empty (`[]`) or a dimension size.
            if self.at(&RBracket) {
                dims.push(self.empty_expr(self.peek().span));
            } else {
                dims.push(self.parse_expr()?);
            }
            self.expect(&RBracket, "]")?;
        }
        let init = if self.eat(&Assign).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(VarDeclItem {
            name,
            dims,
            init,
            span: Span::new(start.start, self.prev_span().end),
        })
    }

    fn parse_if(&mut self) -> Result<StmtKind, ParseError> {
        self.expect(&If, "If")?;
        let cond = self.parse_expr()?;
        self.expect(&Then, "Then")?;
        // Single-line `If ... Then stmt`.
        let then_stmt = if !self.at_any(&[&Newline, &Colon, &Eof]) && self.at_expr_start() {
            Some(Box::new(self.parse_stmt()?))
        } else {
            None
        };
        self.skip_seps();
        let mut else_ifs = Vec::new();
        let mut else_block = Vec::new();
        loop {
            if self.at(&ElseIf) {
                self.bump();
                let c = self.parse_expr()?;
                self.expect(&Then, "Then")?;
                let mut body = Vec::new();
                self.skip_seps();
                while !self.at_any(&[&ElseIf, &Else, &EndIf, &Eof]) {
                    body.push(self.parse_stmt()?);
                }
                else_ifs.push((c, body));
            } else if self.at(&Else) {
                self.bump();
                self.skip_seps();
                while !self.at(&EndIf) && !self.at(&Eof) {
                    else_block.push(self.parse_stmt()?);
                }
                self.expect(&EndIf, "EndIf")?;
                break;
            } else if self.at(&EndIf) {
                self.bump();
                break;
            } else if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing EndIf"));
            } else {
                // continuation of multi-line Then body
                while !self.at_any(&[&ElseIf, &Else, &EndIf, &Eof]) {
                    // push into else_block as the "then" body
                    else_block.push(self.parse_stmt()?);
                }
            }
        }
        Ok(StmtKind::If(IfStmt {
            cond,
            then_stmt,
            else_ifs,
            else_block,
        }))
    }

    fn parse_while(&mut self) -> Result<StmtKind, ParseError> {
        self.expect(&While, "While")?;
        let cond = self.parse_expr()?;
        let mut body = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&WEnd) {
                self.bump();
                break;
            }
            if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing WEnd"));
            }
            body.push(self.parse_stmt()?);
        }
        Ok(StmtKind::While(WhileStmt { cond, body }))
    }

    fn parse_do_until(&mut self) -> Result<StmtKind, ParseError> {
        self.expect(&Do, "Do")?;
        let mut body = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&Until) {
                break;
            }
            if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing Until"));
            }
            body.push(self.parse_stmt()?);
        }
        self.expect(&Until, "Until")?;
        let cond = self.parse_expr()?;
        Ok(StmtKind::DoUntil(DoUntilStmt { body, cond }))
    }

    fn parse_for(&mut self) -> Result<StmtKind, ParseError> {
        self.expect(&For, "For")?;
        let var = self.parse_ident()?;
        self.expect(&Assign, "=")?;
        let from = self.parse_expr()?;
        self.expect(&To, "To")?;
        let to = self.parse_expr()?;
        let step = if self.eat(&Step).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let mut body = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&Next) {
                self.bump();
                break;
            }
            if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing Next"));
            }
            body.push(self.parse_stmt()?);
        }
        Ok(StmtKind::For(ForStmt {
            var,
            from,
            to,
            step,
            body,
        }))
    }

    fn parse_select(&mut self) -> Result<StmtKind, ParseError> {
        self.expect(&Select, "Select")?;
        let mut cases = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&EndSelect) {
                self.bump();
                break;
            }
            if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing EndSelect"));
            }
            if self.at(&Case) {
                cases.push(self.parse_case(&[&EndSelect])?);
            } else {
                return Err(self.err_here("expected Case or EndSelect"));
            }
        }
        Ok(StmtKind::Select(cases))
    }

    fn parse_switch(&mut self) -> Result<StmtKind, ParseError> {
        self.expect(&Switch, "Switch")?;
        let expr = self.parse_expr()?;
        let mut cases = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&EndSwitch) {
                self.bump();
                break;
            }
            if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing EndSwitch"));
            }
            if self.at(&Case) {
                cases.push(self.parse_case(&[&EndSwitch])?);
            } else {
                return Err(self.err_here("expected Case or EndSwitch"));
            }
        }
        Ok(StmtKind::Switch(SwitchStmt { expr, cases }))
    }

    fn parse_case(&mut self, terminators: &[&TokenKind]) -> Result<CaseClause, ParseError> {
        let start = self.expect(&Case, "Case")?.span;
        let mut values = Vec::new();
        let mut is_else = false;
        if self.at(&Else) {
            self.bump();
            is_else = true;
        } else {
            loop {
                values.push(self.parse_expr()?);
                if self.eat(&Comma).is_none() {
                    break;
                }
            }
        }
        let mut body = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&Case) || self.at(&Eof) || self.at_any(terminators) {
                break;
            }
            body.push(self.parse_stmt()?);
        }
        Ok(CaseClause {
            values,
            is_else,
            body,
            span: Span::new(start.start, self.prev_span().end),
        })
    }

    fn parse_with(&mut self) -> Result<StmtKind, ParseError> {
        self.expect(&With, "With")?;
        let expr = self.parse_expr()?;
        let mut body = Vec::new();
        loop {
            self.skip_seps();
            if self.at(&EndWith) {
                self.bump();
                break;
            }
            if self.at(&Eof) {
                return Err(self.err_here("unexpected EOF: missing EndWith"));
            }
            body.push(self.parse_stmt()?);
        }
        Ok(StmtKind::With(WithStmt { expr, body }))
    }

    // ----- expressions -----

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_or()?;
        if self.at(&Assign) {
            self.bump();
            let rhs = self.parse_assignment()?;
            let span = lhs.span.merge(rhs.span);
            return Ok(Expr {
                kind: ExprKind::Binary(BinaryOp::Assign, Box::new(lhs), Box::new(rhs)),
                span,
            });
        }
        Ok(lhs)
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while self.eat(&Or).is_some() {
            let rhs = self.parse_and()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(BinaryOp::Or, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_bitand()?;
        while self.eat(&And).is_some() {
            let rhs = self.parse_bitand()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(BinaryOp::And, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_bitand(&mut self) -> Result<Expr, ParseError> {
        // `&` is ambiguous: bitwise-and vs string concat. AutoIt treats `&`
        // as concat for strings and bitwise for numbers; for the AST we record
        // a generic `Amp` and let later passes decide. Here we fold as Concat.
        let mut lhs = self.parse_equality()?;
        while self.at(&Amp) {
            self.bump();
            let rhs = self.parse_equality()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(BinaryOp::Concat, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = if self.at(&Eq) {
                self.bump();
                BinaryOp::Eq
            } else if self.at(&NotEq) {
                self.bump();
                BinaryOp::NotEq
            } else {
                break;
            };
            let rhs = self.parse_comparison()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = if self.at(&Lt) {
                self.bump();
                BinaryOp::Lt
            } else if self.at(&Le) {
                self.bump();
                BinaryOp::Le
            } else if self.at(&Gt) {
                self.bump();
                BinaryOp::Gt
            } else if self.at(&Ge) {
                self.bump();
                BinaryOp::Ge
            } else {
                break;
            };
            let rhs = self.parse_additive()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = if self.at(&Plus) {
                self.bump();
                BinaryOp::Add
            } else if self.at(&Minus) {
                self.bump();
                BinaryOp::Sub
            } else {
                break;
            };
            let rhs = self.parse_multiplicative()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = if self.at(&Star) {
                self.bump();
                BinaryOp::Mul
            } else if self.at(&Slash) {
                self.bump();
                BinaryOp::Div
            } else {
                break;
            };
            let rhs = self.parse_unary()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let op = if self.at(&Not) {
            self.bump();
            UnaryOp::Not
        } else if self.at(&Minus) {
            self.bump();
            UnaryOp::Neg
        } else if self.at(&Plus) {
            self.bump();
            UnaryOp::Plus
        } else {
            return self.parse_postfix();
        };
        let inner = self.parse_unary()?;
        let span = inner.span.merge(inner.span);
        Ok(Expr {
            kind: ExprKind::Unary(op, Box::new(inner)),
            span,
        })
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_primary()?;
        // Array indexing: `$a[0][1]`, `Foo()[2]`.
        loop {
            if self.at(&LBracket) {
                self.bump();
                let idx = self.parse_expr()?;
                self.expect(&RBracket, "]")?;
                // Fold into VarExpr if it is a variable, else wrap in Call-like.
                if let ExprKind::Var(v) = &mut e.kind {
                    v.indices.push(idx);
                    e.span = e.span.merge(self.prev_span());
                } else {
                    // Build an indexing expression for non-vars (rare).
                    let span = e.span.merge(self.prev_span());
                    e = Expr {
                        kind: ExprKind::Binary(BinaryOp::Concat, Box::new(e), Box::new(idx)),
                        span,
                    };
                }
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            Number(raw) => {
                self.bump();
                let (kind, span) = self.lit_from_number(&raw, t.span);
                Ok(Expr {
                    kind: ExprKind::Lit(Lit { kind, span }),
                    span: t.span,
                })
            }
            Str(s) => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Lit(Lit {
                        kind: LitKind::Str(s),
                        span: t.span,
                    }),
                    span: t.span,
                })
            }
            True => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Lit(Lit {
                        kind: LitKind::Bool(true),
                        span: t.span,
                    }),
                    span: t.span,
                })
            }
            False => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Lit(Lit {
                        kind: LitKind::Bool(false),
                        span: t.span,
                    }),
                    span: t.span,
                })
            }
            Default => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Lit(Lit {
                        kind: LitKind::Default,
                        span: t.span,
                    }),
                    span: t.span,
                })
            }
            Null => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Lit(Lit {
                        kind: LitKind::Null,
                        span: t.span,
                    }),
                    span: t.span,
                })
            }
            Var(name) => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Var(VarExpr {
                        name: Ident {
                            name,
                            span: t.span,
                        },
                        indices: Vec::new(),
                    }),
                    span: t.span,
                })
            }
            Macro(name) => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Macro(name),
                    span: t.span,
                })
            }
            IdentTok(name) => {
                self.bump();
                // Function call if immediately followed by `(`.
                if self.at(&LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    if !self.at(&RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.eat(&Comma).is_none() {
                                break;
                            }
                        }
                    }
                    self.expect(&RParen, ")")?;
                    let span = Span::new(t.span.start, self.prev_span().end);
                    Ok(Expr {
                        kind: ExprKind::Call(CallExpr {
                            callee: Ident { name, span: t.span },
                            args,
                        }),
                        span,
                    })
                } else {
                    Ok(Expr {
                        kind: ExprKind::Ident(Ident { name, span: t.span }),
                        span: t.span,
                    })
                }
            }
            LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(&RParen, ")")?;
                let span = Span::new(t.span.start, self.prev_span().end);
                Ok(Expr {
                    kind: ExprKind::Paren(Box::new(inner)),
                    span,
                })
            }
            _ => Err(self.err_here("expected expression")),
        }
    }

    fn parse_ident(&mut self) -> Result<Ident, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            Var(name) => {
                self.bump();
                Ok(Ident { name, span: t.span })
            }
            IdentTok(name) => {
                self.bump();
                Ok(Ident { name, span: t.span })
            }
            _ => Err(self.err_here("expected identifier or variable name")),
        }
    }

    /// Build a literal from raw number text, handling hex and decimals.
    fn lit_from_number(&self, raw: &str, span: Span) -> (LitKind, Span) {
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("0x") {
            match i64::from_str_radix(&lower[2..], 16) {
                Ok(v) => (LitKind::Int(v), span),
                Err(_) => (LitKind::Str(raw.to_string()), span),
            }
        } else if raw.contains('.') {
            match raw.parse::<f64>() {
                Ok(v) => (LitKind::Float(v), span),
                Err(_) => (LitKind::Str(raw.to_string()), span),
            }
        } else {
            match raw.parse::<i64>() {
                Ok(v) => (LitKind::Int(v), span),
                Err(_) => (LitKind::Str(raw.to_string()), span),
            }
        }
    }

    /// Create a placeholder empty expression for `[]` dims.
    fn empty_expr(&self, span: Span) -> Expr {
        Expr {
            kind: ExprKind::Lit(Lit {
                kind: LitKind::Null,
                span,
            }),
            span,
        }
    }
}