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
use crate::span::Span;
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
    /// All `;` comments captured from the token stream, in order.
    comments: Vec<crate::ast::Comment>,
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
    /// Build a parser over `tokens`.
    ///
    /// Comment tokens are filtered out of the stream up front and collected
    /// into [`Parser::comments`]. That way comments are preserved for the
    /// pretty-printer while the grammar never has to special-case them — a
    /// comment may then appear anywhere, including in the middle of an
    /// expression such as a multi-line array literal.
    pub fn new(tokens: Vec<Token>) -> Self {
        let mut code = Vec::with_capacity(tokens.len());
        let mut comments = Vec::new();
        for t in tokens {
            match t.kind {
                TokenKind::Comment { ref text, block } => {
                    comments.push(crate::ast::Comment {
                        text: text.clone(),
                        block,
                        span: t.span,
                    });
                }
                _ => code.push(t),
            }
        }
        Self {
            tokens: code,
            pos: 0,
            comments,
        }
    }

    // ----- low level helpers -----

    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
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

    /// True when we are at the start of a statement that may appear in
    /// single-line `If ... Then stmt` form.
    fn at_stmt_start(&self) -> bool {
        self.at_expr_start()
            || matches!(
                self.peek_kind(),
                Return | Exit | ExitLoop | ContinueLoop | ContinueCase | Local | Global | Const
                    | Dim | Static | ReDim
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
        Ok(Program {
            items,
            comments: std::mem::take(&mut self.comments),
        })
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
            // `Volatile Func Foo()` — the modifier is optional and only
            // meaningful on a function.
            Volatile => {
                self.bump();
                let mut def = self.parse_func_def()?;
                def.is_volatile = true;
                def.span = Span::new(start.start, self.prev_span().end);
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
            is_volatile: false,
            span: Span::merge(name.span, self.prev_span()),
        })
    }

    fn parse_param(&mut self) -> Result<Param, ParseError> {
        let start = self.peek().span;
        let mut by_ref = false;
        // `Const ByRef $x` / `ByRef Const $x` / `Const $x` — any order.
        if self.eat(&Const).is_some() {
            self.eat(&ByRef);
        } else if self.eat(&ByRef).is_some() {
            by_ref = true;
            self.eat(&Const);
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
            Preproc(_) => {
                let t = self.bump();
                match t.kind {
                    Preproc(name) => StmtKind::Directive(name),
                    _ => unreachable!(),
                }
            }
            Local | Global | Const | Dim | Static | ReDim | Enum => self.parse_var_decl_stmt()?,
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
                let is_continue = matches!(self.peek_kind(), ContinueLoop);
                self.bump();
                let e = if self.at_expr_start() {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if is_continue {
                    StmtKind::ContinueLoop(e)
                } else {
                    StmtKind::ExitLoop(e)
                }
            }
            ContinueCase => {
                self.bump();
                StmtKind::ContinueCase
            }
            If => self.parse_if()?,
            While => self.parse_while()?,
            Do => self.parse_do_until()?,
            For => self.parse_for()?,
            Select => self.parse_select()?,
            Switch => self.parse_switch()?,
            With => self.parse_with()?,
            _ => StmtKind::Expr(self.parse_stmt_expr()?),
        };
        Ok(Stmt {
            kind,
            span: Span::new(start.start, self.prev_span().end),
        })
    }

    fn parse_var_decl_stmt(&mut self) -> Result<StmtKind, ParseError> {
        let kw = self.bump();
        // `ReDim` shares the `Dim` scope kind but resizes in place; remember it
        // explicitly so the interpreter can tell `ReDim` from `Dim Const`.
        let is_redim = matches!(kw.kind, ReDim);
        let (mut kind, mut is_const) = match kw.kind {
            Local => (VarKind::Local, false),
            Global => (VarKind::Global, false),
            Dim => (VarKind::Dim, false),
            Static => (VarKind::Static, false),
            Const => (VarKind::Local, true),
            ReDim => (VarKind::Dim, false),
            Enum => (VarKind::Local, true),
            _ => unreachable!(),
        };
        // `Local Const` / `Global Const` / `Global Enum` ordering, plus
        // multiple leading scope keywords such as `Static Local $x`.
        // A leading `Enum` (without `Global`) is already consumed above, so it
        // has to seed `is_enum` here as well.
        let mut is_enum = matches!(kw.kind, Enum);
        // `Enum Step n` / `Enum $A = 1, $B` — captured below, once `Enum` is
        // known to be in play.
        let mut enum_step: Option<Expr> = None;
        // `Static` is the strongest scope modifier and wins over the others,
        // regardless of order (`Static Local $x` == `Local Static $x`).
        let mut saw_static = kind == VarKind::Static;
        loop {
            if self.eat(&Const).is_some() {
                is_const = true;
            } else if self.eat(&Enum).is_some() {
                is_enum = true;
                is_const = true; // enumeration members are constants
                if self.eat(&Step).is_some() {
                    enum_step = Some(self.parse_expr()?);
                }
            } else if self.at(&Local) {
                self.bump();
                if !saw_static {
                    kind = VarKind::Local;
                }
            } else if self.at(&Global) {
                self.bump();
                if !saw_static {
                    kind = VarKind::Global;
                }
            } else if self.at(&Static) {
                self.bump();
                saw_static = true;
                kind = VarKind::Static;
            } else if self.at(&Dim) {
                self.bump();
                if !saw_static {
                    kind = VarKind::Dim;
                }
            } else {
                break;
            }
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
            is_enum,
            is_redim,
            enum_step,
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
        let then_stmt = if !self.at_any(&[&Newline, &Colon, &Eof]) && self.at_stmt_start() {
            Some(Box::new(self.parse_stmt()?))
        } else {
            None
        };
        let mut else_ifs = Vec::new();
        let mut else_block = Vec::new();
        let mut then_block = Vec::new();
        // AutoIt single-line form: `If cond Then stmt` — no EndIf needed.
        if then_stmt.is_some() {
            return Ok(StmtKind::If(IfStmt {
                cond,
                then_stmt,
                else_ifs,
                else_block,
                then_block: Vec::new(),
            }));
        }
        loop {
            self.skip_seps();
            if self.at(&ElseIf) {
                self.bump();
                let c = self.parse_expr()?;
                self.expect(&Then, "Then")?;
                let mut body = Vec::new();
                loop {
                    self.skip_seps();
                    if self.at_any(&[&ElseIf, &Else, &EndIf, &Eof]) {
                        break;
                    }
                    body.push(self.parse_stmt()?);
                }
                else_ifs.push((c, body));
            } else if self.at(&Else) {
                self.bump();
                loop {
                    self.skip_seps();
                    if self.at(&EndIf) || self.at(&Eof) {
                        break;
                    }
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
                loop {
                    self.skip_seps();
                    if self.at_any(&[&ElseIf, &Else, &EndIf, &Eof]) {
                        break;
                    }
                    then_block.push(self.parse_stmt()?);
                }
            }
        }
        Ok(StmtKind::If(IfStmt {
            cond,
            then_stmt,
            then_block,
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
        // `For ... In $arr` iteration form has no from/to/step.
        let iter = if self.eat(&In).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let mut from = self.empty_expr(self.peek().span);
        let mut to = self.empty_expr(self.peek().span);
        let mut step = None;
        if iter.is_none() {
            self.expect(&Assign, "=")?;
            from = self.parse_expr()?;
            self.expect(&To, "To")?;
            to = self.parse_expr()?;
            if self.eat(&Step).is_some() {
                step = Some(self.parse_expr()?);
            }
        }
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
            iter,
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

    /// Parse an expression in *expression* context.
    ///
    /// AutoIt overloads `=`: at statement level it assigns, but inside an
    /// expression it compares. Everything reached from here is an expression,
    /// so `=` becomes [`BinaryOp::EqLoose`]. The statement form (`$x = 1`) is
    /// handled by [`Parser::parse_stmt_expr`].
    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_ternary()
    }

    /// Parse the expression of an expression-statement.
    ///
    /// A leading `lvalue = value` — or any compound form such as `lvalue +=
    /// value` — is an assignment; anything else is an ordinary expression.
    fn parse_stmt_expr(&mut self) -> Result<Expr, ParseError> {
        let save = self.pos;
        if let Some(lhs) = self.try_parse_lvalue() {
            if let Some(op) = assign_op(self.peek_kind()) {
                self.bump();
                let rhs = self.parse_expr()?;
                let span = lhs.span.merge(rhs.span);
                return Ok(Expr {
                    kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                    span,
                });
            }
        }
        // Not an assignment: re-read as an ordinary expression.
        self.pos = save;
        self.parse_expr()
    }

    /// Try to read `$var` with optional `[...]` subscripts. Restores the
    /// cursor and returns `None` when the tokens are not an lvalue.
    fn try_parse_lvalue(&mut self) -> Option<Expr> {
        let save = self.pos;
        match self.parse_postfix() {
            // Members are assignable too (`$obj.Prop = 1`, `.Prop = 1`).
            Ok(e) if matches!(e.kind, ExprKind::Var(_) | ExprKind::Member(..)) => Some(e),
            _ => {
                self.pos = save;
                None
            }
        }
    }

    /// `cond ? a : b` — the ternary conditional. `?` binds looser than the
    /// binary operators but tighter than assignment.
    fn parse_ternary(&mut self) -> Result<Expr, ParseError> {
        let cond = self.parse_or()?;
        if self.at(&Question) {
            self.bump();
            let a = self.parse_ternary()?;
            self.expect(&Colon, ":")?;
            let b = self.parse_ternary()?;
            let span = cond.span.merge(b.span);
            return Ok(Expr {
                kind: ExprKind::Ternary(Box::new(cond), Box::new(a), Box::new(b)),
                span,
            });
        }
        Ok(cond)
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
            } else if self.at(&Assign) {
                // `=` inside an expression is comparison, not assignment.
                self.bump();
                BinaryOp::EqLoose
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
        let mut lhs = self.parse_power()?;
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
            let rhs = self.parse_power()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    /// Power `^` — higher precedence than `*`/`/`, right associative.
    fn parse_power(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_unary()?;
        if self.at(&Caret) {
            self.bump();
            let rhs = self.parse_power()?;
            let span = lhs.span.merge(rhs.span);
            return Ok(Expr {
                kind: ExprKind::Binary(BinaryOp::Pow, Box::new(lhs), Box::new(rhs)),
                span,
            });
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
        // A leading `.` is the implicit `With ... EndWith` subject.
        let mut e = if self.at(&Dot) {
            let span = self.peek().span;
            Expr {
                kind: ExprKind::WithSubject,
                span,
            }
        } else {
            self.parse_primary()?
        };

        // Array indexing (`$a[0][1]`) and member access (`$obj.Prop`,
        // `$obj.Method()`, chains of both).
        loop {
            if self.at(&Dot) {
                self.bump(); // '.'
                let name = self.parse_ident()?;
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
                    let close = self.expect(&RParen, ")")?;
                    let span = Span::new(e.span.start, close.span.end);
                    e = Expr {
                        kind: ExprKind::MethodCall(Box::new(e), name, args),
                        span,
                    };
                } else {
                    let span = Span::new(e.span.start, name.span.end);
                    e = Expr {
                        kind: ExprKind::Member(Box::new(e), name),
                        span,
                    };
                }
            } else if self.at(&LBracket) {
                self.bump();
                let idx = self.parse_expr()?;
                self.expect(&RBracket, "]")?;
                // Subscripts on a variable live on the `VarExpr`; anything else
                // (`DllCall(...)[0]`, `$obj.Items[1]`) gets a `Subscript` node
                // so the base is still evaluated. Chained `[a][b]` accumulates.
                if let ExprKind::Var(v) = &mut e.kind {
                    v.indices.push(idx);
                    e.span = e.span.merge(self.prev_span());
                } else if let ExprKind::Subscript(_, indices) = &mut e.kind {
                    indices.push(idx);
                    e.span = e.span.merge(self.prev_span());
                } else {
                    let span = e.span.merge(self.prev_span());
                    e = Expr {
                        kind: ExprKind::Subscript(Box::new(e), vec![idx]),
                        span,
                    };
                }
            } else if self.at(&LParen) {
                // User-defined array call: `$arr[0](...)` returns a function
                // reference that is then invoked. The brackets and the call
                // form one chain, so `$arr[0](...)[1]` keeps working.
                let ExprKind::Var(base) = e.kind.clone() else {
                    return Err(self.err_here("cannot call non-variable expression"));
                };
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
                let span = Span::new(base.name.span.start, self.prev_span().end);
                e = Expr {
                    kind: ExprKind::IndexCall(base, args),
                    span,
                };
            } else {
                break;
            }
        }

        // A `.` with nothing after it is not a valid expression.
        if matches!(e.kind, ExprKind::WithSubject) {
            return Err(self.err_here("expected a member name after '.'"));
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
            LBracket => {
                self.bump();
                let mut items = Vec::new();
                if !self.at(&RBracket) {
                    loop {
                        items.push(self.parse_expr()?);
                        if self.eat(&Comma).is_none() {
                            break;
                        }
                    }
                }
                self.expect(&RBracket, "]")?;
                let span = Span::new(t.span.start, self.prev_span().end);
                Ok(Expr {
                    kind: ExprKind::ArrayLit(items),
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

/// Map an assignment token to its operator, or `None` for anything else.
fn assign_op(kind: &TokenKind) -> Option<BinaryOp> {
    Some(match kind {
        Assign => BinaryOp::Assign,
        PlusAssign => BinaryOp::PlusAssign,
        MinusAssign => BinaryOp::MinusAssign,
        StarAssign => BinaryOp::StarAssign,
        SlashAssign => BinaryOp::SlashAssign,
        CaretAssign => BinaryOp::CaretAssign,
        AmpAssign => BinaryOp::AmpAssign,
        _ => return None,
    })
}
