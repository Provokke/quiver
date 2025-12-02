//! Hand-written recursive-descent / precedence-climbing parser producing an
//! arena-allocated AST.

use crate::ast::*;
use crate::lexer::{FPartRaw, Lexer, Tok, Token, scan_string, string_prefix};
use crate::span::Span;
use bumpalo::Bump;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub span: Span,
    pub msg: String,
}

pub type PResult<T> = Result<T, ParseError>;

pub struct Parsed<'a> {
    pub module: Module<'a>,
    pub comments: Vec<Span>,
    pub tokens: Vec<Token<'a>>,
}

/// Parses a module. Lexer errors are reported as parse errors.
pub fn parse_module<'a>(src: &'a str, bump: &'a Bump) -> PResult<Parsed<'a>> {
    let lexed = Lexer::new(src).tokenize();
    if let Some(e) = lexed.errors.first() {
        return Err(ParseError {
            span: e.span,
            msg: e.msg.clone(),
        });
    }
    let mut p = Parser {
        src,
        bump,
        toks: lexed.tokens,
        pos: 0,
        depth: 0,
    };
    let body = p.parse_file()?;
    Ok(Parsed {
        module: Module { body },
        comments: lexed.comments,
        tokens: p.toks,
    })
}

const MAX_DEPTH: u32 = 200;

pub struct Parser<'a> {
    src: &'a str,
    bump: &'a Bump,
    toks: Vec<Token<'a>>,
    pos: usize,
    depth: u32,
}

fn aug_op(t: Tok) -> Option<Operator> {
    Some(match t {
        Tok::PlusEqual => Operator::Add,
        Tok::MinEqual => Operator::Sub,
        Tok::StarEqual => Operator::Mult,
        Tok::AtEqual => Operator::MatMult,
        Tok::SlashEqual => Operator::Div,
        Tok::PercentEqual => Operator::Mod,
        Tok::DoubleStarEqual => Operator::Pow,
        Tok::LeftShiftEqual => Operator::LShift,
        Tok::RightShiftEqual => Operator::RShift,
        Tok::VBarEqual => Operator::BitOr,
        Tok::CircumflexEqual => Operator::BitXor,
        Tok::AmperEqual => Operator::BitAnd,
        Tok::DoubleSlashEqual => Operator::FloorDiv,
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    // ---------- token helpers ----------
    fn tok(&self) -> Token<'a> {
        self.toks[self.pos]
    }
    fn peek(&self) -> Tok {
        self.toks[self.pos].kind
    }
    fn peek_at(&self, n: usize) -> Tok {
        self.toks.get(self.pos + n).map(|t| t.kind).unwrap_or(Tok::EndMarker)
    }
    fn at(&self, t: Tok) -> bool {
        self.peek() == t
    }
    fn bump_tok(&mut self) -> Token<'a> {
        let t = self.toks[self.pos];
        if t.kind != Tok::EndMarker {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: Tok) -> bool {
        if self.at(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn err<T>(&self, msg: &str) -> PResult<T> {
        let t = self.tok();
        Err(ParseError {
            span: t.span,
            msg: format!("{msg}, found {:?} {:?}", t.kind, t.text),
        })
    }
    fn expect(&mut self, t: Tok) -> PResult<Token<'a>> {
        if self.at(t) {
            Ok(self.bump_tok())
        } else {
            self.err(&format!("expected {t:?}"))
        }
    }
    fn start(&self) -> u32 {
        self.tok().span.start
    }
    fn prev_end(&self) -> u32 {
        if self.pos == 0 {
            0
        } else {
            self.toks[self.pos - 1].span.end
        }
    }
    fn span_from(&self, start: u32) -> Span {
        Span::new(start, self.prev_end().max(start))
    }
    fn at_soft(&self, kw: &str) -> bool {
        let t = self.tok();
        t.kind == Tok::Name && t.text == kw
    }
    fn ident(&mut self) -> PResult<Ident<'a>> {
        let t = self.expect(Tok::Name)?;
        Ok(Ident {
            name: t.text,
            span: t.span,
        })
    }
    fn alloc(&self, kind: ExprKind<'a>, span: Span) -> Ex<'a> {
        self.bump.alloc(Expr { kind, span, parens: 0 })
    }
    fn slice<T: Copy>(&self, v: Vec<T>) -> &'a [T] {
        self.bump.alloc_slice_copy(&v)
    }
    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("too deeply nested");
        }
        Ok(())
    }

    // ---------- statements ----------
    fn parse_file(&mut self) -> PResult<Body<'a>> {
        let mut out = Vec::new();
        while !self.at(Tok::EndMarker) {
            if self.eat(Tok::Newline) {
                continue;
            }
            self.statement(&mut out)?;
        }
        Ok(self.slice(out))
    }

    fn statement(&mut self, out: &mut Vec<Stmt<'a>>) -> PResult<()> {
        self.enter()?;
        let r = self.statement_inner(out);
        self.depth -= 1;
        r
    }

    fn statement_inner(&mut self, out: &mut Vec<Stmt<'a>>) -> PResult<()> {
        let start = self.start();
        let kind = match self.peek() {
            Tok::Def => self.funcdef(start, &[], false)?,
            Tok::Class => self.classdef(start, &[])?,
            Tok::At => self.decorated()?,
            Tok::If => self.if_stmt(false)?,
            Tok::While => self.while_stmt()?,
            Tok::For => self.for_stmt(false)?,
            Tok::Try => self.try_stmt()?,
            Tok::With => self.with_stmt(false)?,
            Tok::Async => {
                self.bump_tok();
                match self.peek() {
                    Tok::Def => self.funcdef(start, &[], true)?,
                    Tok::For => self.for_stmt(true)?,
                    Tok::With => self.with_stmt(true)?,
                    _ => return self.err("expected def, for or with after async"),
                }
            }
            Tok::Name
                if self.at_soft("match")
                    && !matches!(
                        self.peek_at(1),
                        Tok::Newline | Tok::Equal | Tok::Dot | Tok::Colon | Tok::Comma | Tok::RPar
                    ) =>
            {
                let save = self.pos;
                match self.match_stmt() {
                    Ok(k) => k,
                    Err(e) => {
                        self.pos = save;
                        if let Err(e2) = self.simple_stmts(out) {
                            return Err(if e2.span.start > e.span.start { e2 } else { e });
                        }
                        return Ok(());
                    }
                }
            }
            _ => return self.simple_stmts(out),
        };
        out.push(Stmt {
            kind,
            span: self.span_from(start),
        });
        Ok(())
    }

    fn simple_stmts(&mut self, out: &mut Vec<Stmt<'a>>) -> PResult<()> {
        loop {
            let start = self.start();
            let kind = self.simple_stmt()?;
            out.push(Stmt {
                kind,
                span: self.span_from(start),
            });
            if self.eat(Tok::Semi) {
                if self.eat(Tok::Newline) || self.at(Tok::EndMarker) {
                    return Ok(());
                }
                continue;
            }
            if self.eat(Tok::Newline) || self.at(Tok::EndMarker) {
                return Ok(());
            }
            return self.err("expected newline");
        }
    }

    fn simple_stmt(&mut self) -> PResult<StmtKind<'a>> {
        Ok(match self.peek() {
            Tok::Pass => {
                self.bump_tok();
                StmtKind::Pass
            }
            Tok::Break => {
                self.bump_tok();
                StmtKind::Break
            }
            Tok::Continue => {
                self.bump_tok();
                StmtKind::Continue
            }
            Tok::Return => {
                self.bump_tok();
                let v = if self.at_stmt_end() {
                    None
                } else {
                    Some(self.star_expressions()?)
                };
                StmtKind::Return(v)
            }
            Tok::Raise => {
                self.bump_tok();
                let mut exc = None;
                let mut cause = None;
                if !self.at_stmt_end() {
                    exc = Some(self.expression()?);
                    if self.eat(Tok::From) {
                        cause = Some(self.expression()?);
                    }
                }
                StmtKind::Raise { exc, cause }
            }
            Tok::Global | Tok::Nonlocal => {
                let g = self.bump_tok().kind == Tok::Global;
                let mut names = vec![self.ident()?];
                while self.eat(Tok::Comma) {
                    names.push(self.ident()?);
                }
                let names = self.slice(names);
                if g {
                    StmtKind::Global(names)
                } else {
                    StmtKind::Nonlocal(names)
                }
            }
            Tok::Del => {
                self.bump_tok();
                let t = self.target_list(&[Tok::Newline, Tok::Semi, Tok::EndMarker])?;
                let v = match t.kind {
                    ExprKind::Tuple {
                        elts,
                        parenthesized: false,
                    } => elts,
                    _ => self.slice(vec![t]),
                };
                StmtKind::Delete(v)
            }
            Tok::Assert => {
                self.bump_tok();
                let test = self.expression()?;
                let msg = if self.eat(Tok::Comma) {
                    Some(self.expression()?)
                } else {
                    None
                };
                StmtKind::Assert { test, msg }
            }
            Tok::Import => self.import()?,
            Tok::From => self.import_from()?,
            Tok::Name
                if self.at_soft("type")
                    && self.peek_at(1) == Tok::Name
                    && matches!(self.peek_at(2), Tok::Equal | Tok::LSqb) =>
            {
                self.bump_tok();
                let nt = self.bump_tok();
                let name = self.alloc(ExprKind::Name(nt.text), nt.span);
                let type_params = self.type_params()?;
                self.expect(Tok::Equal)?;
                let value = self.expression()?;
                StmtKind::TypeAlias {
                    name,
                    type_params,
                    value,
                }
            }
            _ => self.expr_stmt()?,
        })
    }

    fn at_stmt_end(&self) -> bool {
        matches!(self.peek(), Tok::Newline | Tok::Semi | Tok::EndMarker)
    }

    fn expr_stmt(&mut self) -> PResult<StmtKind<'a>> {
        let first = self.assign_rhs()?;
        if self.at(Tok::Colon) {
            self.bump_tok();
            let annotation = self.expression()?;
            let value = if self.eat(Tok::Equal) {
                Some(self.assign_rhs()?)
            } else {
                None
            };
            return Ok(StmtKind::AnnAssign {
                target: first,
                annotation,
                value,
            });
        }
        if let Some(op) = aug_op(self.peek()) {
            self.bump_tok();
            let value = self.assign_rhs()?;
            return Ok(StmtKind::AugAssign {
                target: first,
                op,
                value,
            });
        }
        if self.at(Tok::Equal) {
            let mut items = vec![first];
            while self.eat(Tok::Equal) {
                items.push(self.assign_rhs()?);
            }
            let value = items.pop().unwrap();
            return Ok(StmtKind::Assign {
                targets: self.slice(items),
                value,
            });
        }
        Ok(StmtKind::Expr(first))
    }

    fn assign_rhs(&mut self) -> PResult<Ex<'a>> {
        if self.at(Tok::Yield) {
            self.yield_expr()
        } else {
            self.star_expressions()
        }
    }

    fn dotted_name(&mut self) -> PResult<Ident<'a>> {
        let first = self.expect(Tok::Name)?;
        let mut end = first.span.end;
        while self.at(Tok::Dot) && self.peek_at(1) == Tok::Name {
            self.bump_tok();
            end = self.bump_tok().span.end;
        }
        let span = Span::new(first.span.start, end);
        Ok(Ident {
            name: &self.src[span.range()],
            span,
        })
    }

    fn import(&mut self) -> PResult<StmtKind<'a>> {
        self.expect(Tok::Import)?;
        let mut names = Vec::new();
        loop {
            let start = self.start();
            let name = self.dotted_name()?;
            let asname = if self.eat(Tok::As) { Some(self.ident()?) } else { None };
            names.push(Alias {
                name,
                asname,
                span: self.span_from(start),
            });
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        Ok(StmtKind::Import(self.slice(names)))
    }

    fn import_from(&mut self) -> PResult<StmtKind<'a>> {
        self.expect(Tok::From)?;
        let mut level = 0;
        loop {
            match self.peek() {
                Tok::Dot => level += 1,
                Tok::Ellipsis => level += 3,
                _ => break,
            }
            self.bump_tok();
        }
        let module = if self.at(Tok::Name) {
            Some(self.dotted_name()?)
        } else {
            None
        };
        if module.is_none() && level == 0 {
            return self.err("expected module name");
        }
        self.expect(Tok::Import)?;
        let mut names = Vec::new();
        if self.at(Tok::Star) {
            let t = self.bump_tok();
            names.push(Alias {
                name: Ident {
                    name: "*",
                    span: t.span,
                },
                asname: None,
                span: t.span,
            });
        } else {
            let paren = self.eat(Tok::LPar);
            loop {
                let start = self.start();
                let name = self.ident()?;
                let asname = if self.eat(Tok::As) { Some(self.ident()?) } else { None };
                names.push(Alias {
                    name,
                    asname,
                    span: self.span_from(start),
                });
                if !self.eat(Tok::Comma) {
                    break;
                }
                if paren && self.at(Tok::RPar) {
                    break;
                }
            }
            if paren {
                self.expect(Tok::RPar)?;
            }
        }
        Ok(StmtKind::ImportFrom {
            module,
            level,
            names: self.slice(names),
        })
    }

    fn block(&mut self) -> PResult<Body<'a>> {
        self.expect(Tok::Colon)?;
        let mut out = Vec::new();
        if self.eat(Tok::Newline) {
            self.expect(Tok::Indent)?;
            while !self.eat(Tok::Dedent) {
                if self.at(Tok::EndMarker) {
                    return self.err("unexpected end of file");
                }
                self.statement(&mut out)?;
            }
        } else {
            self.simple_stmts(&mut out)?;
        }
        Ok(self.slice(out))
    }

    fn decorated(&mut self) -> PResult<StmtKind<'a>> {
        let start = self.start();
        let mut decos = Vec::new();
        while self.eat(Tok::At) {
            decos.push(self.named_expression()?);
            self.expect(Tok::Newline)?;
        }
        let decos = self.slice(decos);
        match self.peek() {
            Tok::Def => self.funcdef(start, decos, false),
            Tok::Class => self.classdef(start, decos),
            Tok::Async => {
                self.bump_tok();
                self.funcdef(start, decos, true)
            }
            _ => self.err("expected def or class after decorator"),
        }
    }

    fn type_params(&mut self) -> PResult<&'a [TypeParam<'a>]> {
        if !self.eat(Tok::LSqb) {
            return Ok(&[]);
        }
        let mut v = Vec::new();
        while !self.at(Tok::RSqb) {
            let kind = if self.eat(Tok::Star) {
                TypeParamKind::TypeVarTuple
            } else if self.eat(Tok::DoubleStar) {
                TypeParamKind::ParamSpec
            } else {
                TypeParamKind::TypeVar
            };
            let name = self.ident()?;
            let bound = if self.eat(Tok::Colon) {
                Some(self.expression()?)
            } else {
                None
            };
            let default = if self.eat(Tok::Equal) {
                Some(if self.at(Tok::Star) {
                    self.star_expr()?
                } else {
                    self.expression()?
                })
            } else {
                None
            };
            v.push(TypeParam {
                kind,
                name,
                bound,
                default,
            });
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RSqb)?;
        Ok(self.slice(v))
    }

    fn funcdef(&mut self, _start: u32, decorators: &'a [Ex<'a>], is_async: bool) -> PResult<StmtKind<'a>> {
        self.expect(Tok::Def)?;
        let name = self.ident()?;
        let type_params = self.type_params()?;
        self.expect(Tok::LPar)?;
        let params = self.parameters(Tok::RPar, true)?;
        self.expect(Tok::RPar)?;
        let returns = if self.eat(Tok::RArrow) {
            Some(self.expression()?)
        } else {
            None
        };
        let body = self.block()?;
        Ok(StmtKind::FunctionDef {
            name,
            decorators,
            type_params,
            params,
            returns,
            body,
            is_async,
        })
    }

    fn param(&mut self, annotations: bool, star_ann: bool) -> PResult<Param<'a>> {
        let name = self.ident()?;
        let annotation = if annotations && self.eat(Tok::Colon) {
            Some(if star_ann && self.at(Tok::Star) {
                self.star_expr()?
            } else {
                self.expression()?
            })
        } else {
            None
        };
        Ok(Param {
            name,
            annotation,
            default: None,
        })
    }

    fn parameters(&mut self, end: Tok, annotations: bool) -> PResult<&'a Parameters<'a>> {
        let mut posonly = Vec::new();
        let mut args = Vec::new();
        let mut vararg = None;
        let mut bare_star = false;
        let mut kwonly = Vec::new();
        let mut kwarg = None;
        let mut seen_star = false;
        while !self.at(end) {
            if self.eat(Tok::Slash) {
                posonly.append(&mut args);
            } else if self.eat(Tok::Star) {
                seen_star = true;
                if self.at(Tok::Comma) || self.at(end) {
                    bare_star = true;
                } else {
                    vararg = Some(self.param(annotations, true)?);
                }
            } else if self.eat(Tok::DoubleStar) {
                kwarg = Some(self.param(annotations, false)?);
            } else {
                let mut p = self.param(annotations, false)?;
                if self.eat(Tok::Equal) {
                    p.default = Some(self.expression()?);
                }
                if seen_star { kwonly.push(p) } else { args.push(p) }
            }
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        Ok(self.bump.alloc(Parameters {
            posonly: self.slice(posonly),
            args: self.slice(args),
            vararg,
            bare_star,
            kwonly: self.slice(kwonly),
            kwarg,
        }))
    }

    fn classdef(&mut self, _start: u32, decorators: &'a [Ex<'a>]) -> PResult<StmtKind<'a>> {
        self.expect(Tok::Class)?;
        let name = self.ident()?;
        let type_params = self.type_params()?;
        let mut bases: &'a [Arg<'a>] = &[];
        let mut has_parens = false;
        if self.eat(Tok::LPar) {
            has_parens = true;
            bases = self.call_args()?;
            self.expect(Tok::RPar)?;
        }
        let body = self.block()?;
        Ok(StmtKind::ClassDef {
            name,
            decorators,
            type_params,
            bases,
            has_parens,
            body,
        })
    }

    fn if_stmt(&mut self, elif: bool) -> PResult<StmtKind<'a>> {
        self.bump_tok(); // if / elif
        let test = self.named_expression()?;
        let body = self.block()?;
        let orelse = if self.at(Tok::Elif) {
            let start = self.start();
            let k = self.if_stmt(true)?;
            self.slice(vec![Stmt {
                kind: k,
                span: self.span_from(start),
            }])
        } else if self.eat(Tok::Else) {
            self.block()?
        } else {
            &[]
        };
        Ok(StmtKind::If {
            test,
            body,
            orelse,
            elif,
        })
    }

    fn while_stmt(&mut self) -> PResult<StmtKind<'a>> {
        self.expect(Tok::While)?;
        let test = self.named_expression()?;
        let body = self.block()?;
        let orelse = if self.eat(Tok::Else) { self.block()? } else { &[] };
        Ok(StmtKind::While { test, body, orelse })
    }

    fn for_stmt(&mut self, is_async: bool) -> PResult<StmtKind<'a>> {
        self.expect(Tok::For)?;
        let target = self.target_list(&[Tok::In])?;
        self.expect(Tok::In)?;
        let iter = self.star_expressions()?;
        let body = self.block()?;
        let orelse = if self.eat(Tok::Else) { self.block()? } else { &[] };
        Ok(StmtKind::For {
            is_async,
            target,
            iter,
            body,
            orelse,
        })
    }

    fn try_stmt(&mut self) -> PResult<StmtKind<'a>> {
        self.expect(Tok::Try)?;
        let body = self.block()?;
        let mut handlers = Vec::new();
        let mut star = false;
        while self.at(Tok::Except) {
            let start = self.start();
            self.bump_tok();
            if self.eat(Tok::Star) {
                star = true;
            }
            let mut typ = None;
            let mut name = None;
            if !self.at(Tok::Colon) {
                let t = self.expression()?;
                if self.at(Tok::Comma) {
                    // 3.14-style unparenthesized tuple
                    let mut elts = vec![t];
                    while self.eat(Tok::Comma) {
                        elts.push(self.expression()?);
                    }
                    typ = Some(self.alloc(
                        ExprKind::Tuple {
                            elts: self.slice(elts),
                            parenthesized: false,
                        },
                        self.span_from(t.span.start),
                    ));
                } else {
                    typ = Some(t);
                }
                if self.eat(Tok::As) {
                    name = Some(self.ident()?);
                }
            }
            let hbody = self.block()?;
            handlers.push(ExceptHandler {
                typ,
                name,
                body: hbody,
                span: self.span_from(start),
            });
        }
        let orelse = if self.eat(Tok::Else) { self.block()? } else { &[] };
        let finalbody = if self.eat(Tok::Finally) { self.block()? } else { &[] };
        if handlers.is_empty() && finalbody.is_empty() {
            return self.err("expected except or finally");
        }
        Ok(StmtKind::Try {
            body,
            handlers: self.slice(handlers),
            orelse,
            finalbody,
            star,
        })
    }

    fn with_item(&mut self) -> PResult<WithItem<'a>> {
        let context = self.expression()?;
        let vars = if self.eat(Tok::As) {
            Some(self.target(&[Tok::Comma, Tok::Colon, Tok::RPar])?)
        } else {
            None
        };
        Ok(WithItem { context, vars })
    }

    fn with_stmt(&mut self, is_async: bool) -> PResult<StmtKind<'a>> {
        self.expect(Tok::With)?;
        if self.at(Tok::LPar) {
            let save = self.pos;
            let attempt = (|| -> PResult<Vec<WithItem<'a>>> {
                self.bump_tok();
                let mut items = Vec::new();
                loop {
                    items.push(self.with_item()?);
                    if !self.eat(Tok::Comma) || self.at(Tok::RPar) {
                        break;
                    }
                }
                self.expect(Tok::RPar)?;
                if !self.at(Tok::Colon) {
                    return self.err("expected ':'");
                }
                Ok(items)
            })();
            match attempt {
                Ok(items) => {
                    let body = self.block()?;
                    return Ok(StmtKind::With {
                        is_async,
                        items: self.slice(items),
                        body,
                    });
                }
                Err(_) => self.pos = save,
            }
        }
        let mut items = Vec::new();
        loop {
            items.push(self.with_item()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        let body = self.block()?;
        Ok(StmtKind::With {
            is_async,
            items: self.slice(items),
            body,
        })
    }

    // ---------- match ----------
    fn match_stmt(&mut self) -> PResult<StmtKind<'a>> {
        self.bump_tok(); // match
        let subject = self.star_named_expressions_tuple()?;
        self.expect(Tok::Colon)?;
        self.expect(Tok::Newline)?;
        self.expect(Tok::Indent)?;
        let mut cases = Vec::new();
        while !self.eat(Tok::Dedent) {
            let start = self.start();
            if !self.at_soft("case") {
                return self.err("expected 'case'");
            }
            self.bump_tok();
            let pattern = self.open_pattern()?;
            let guard = if self.eat(Tok::If) {
                Some(self.named_expression()?)
            } else {
                None
            };
            let body = self.block()?;
            cases.push(MatchCase {
                pattern,
                guard,
                body,
                span: self.span_from(start),
            });
        }
        if cases.is_empty() {
            return self.err("match without cases");
        }
        Ok(StmtKind::Match {
            subject,
            cases: self.slice(cases),
        })
    }

    fn star_named_expressions_tuple(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let first = if self.at(Tok::Star) {
            self.star_expr()?
        } else {
            self.named_expression()?
        };
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(Tok::Comma) {
            if self.at(Tok::Colon) {
                break;
            }
            elts.push(if self.at(Tok::Star) {
                self.star_expr()?
            } else {
                self.named_expression()?
            });
        }
        Ok(self.alloc(
            ExprKind::Tuple {
                elts: self.slice(elts),
                parenthesized: false,
            },
            self.span_from(start),
        ))
    }

    fn open_pattern(&mut self) -> PResult<Pattern<'a>> {
        let start = self.start();
        let first = self.maybe_star_pattern()?;
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut v = vec![first];
        while self.eat(Tok::Comma) {
            if self.at(Tok::Colon) || self.at(Tok::If) {
                break;
            }
            v.push(self.maybe_star_pattern()?);
        }
        Ok(Pattern::Sequence(self.slice(v), self.span_from(start)))
    }

    fn maybe_star_pattern(&mut self) -> PResult<Pattern<'a>> {
        let start = self.start();
        if self.eat(Tok::Star) {
            let n = self.ident()?;
            let n = if n.name == "_" { None } else { Some(n) };
            return Ok(Pattern::Star(n, self.span_from(start)));
        }
        self.pattern()
    }

    fn pattern(&mut self) -> PResult<Pattern<'a>> {
        self.enter()?;
        let start = self.start();
        let first = self.closed_pattern()?;
        let mut p = if self.at(Tok::VBar) {
            let mut v = vec![first];
            while self.eat(Tok::VBar) {
                v.push(self.closed_pattern()?);
            }
            Pattern::Or(self.slice(v), self.span_from(start))
        } else {
            first
        };
        if self.eat(Tok::As) {
            let n = self.ident()?;
            p = Pattern::As(Some(self.bump.alloc(p)), Some(n), self.span_from(start));
        }
        self.depth -= 1;
        Ok(p)
    }

    fn closed_pattern(&mut self) -> PResult<Pattern<'a>> {
        let start = self.start();
        match self.peek() {
            Tok::LPar | Tok::LSqb => {
                let close = if self.bump_tok().kind == Tok::LPar {
                    Tok::RPar
                } else {
                    Tok::RSqb
                };
                let is_paren = close == Tok::RPar;
                let mut v = Vec::new();
                let mut trailing_comma = false;
                while !self.at(close) {
                    v.push(self.maybe_star_pattern()?);
                    trailing_comma = false;
                    if !self.eat(Tok::Comma) {
                        break;
                    }
                    trailing_comma = true;
                }
                self.expect(close)?;
                if is_paren && v.len() == 1 && !trailing_comma && !matches!(v[0], Pattern::Star(..)) {
                    return Ok(v[0]);
                }
                Ok(Pattern::Sequence(self.slice(v), self.span_from(start)))
            }
            Tok::LBrace => {
                self.bump_tok();
                let mut keys = Vec::new();
                let mut pats = Vec::new();
                let mut rest = None;
                while !self.at(Tok::RBrace) {
                    if self.eat(Tok::DoubleStar) {
                        rest = Some(self.ident()?);
                    } else {
                        let k = self.literal_or_value()?;
                        self.expect(Tok::Colon)?;
                        keys.push(k);
                        pats.push(self.pattern()?);
                    }
                    if !self.eat(Tok::Comma) {
                        break;
                    }
                }
                self.expect(Tok::RBrace)?;
                Ok(Pattern::Mapping {
                    keys: self.slice(keys),
                    patterns: self.slice(pats),
                    rest,
                    span: self.span_from(start),
                })
            }
            Tok::Name => {
                let e = self.literal_or_value()?;
                if self.eat(Tok::LPar) {
                    let mut pats = Vec::new();
                    let mut kwd = Vec::new();
                    while !self.at(Tok::RPar) {
                        if self.at(Tok::Name) && self.peek_at(1) == Tok::Equal {
                            let n = self.ident()?;
                            self.bump_tok();
                            kwd.push((n, self.pattern()?));
                        } else {
                            pats.push(self.pattern()?);
                        }
                        if !self.eat(Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RPar)?;
                    return Ok(Pattern::Class {
                        cls: e,
                        patterns: self.slice(pats),
                        kwd: self.slice(kwd),
                        span: self.span_from(start),
                    });
                }
                if let ExprKind::Name(n) = e.kind {
                    let id = if n == "_" {
                        None
                    } else {
                        Some(Ident { name: n, span: e.span })
                    };
                    return Ok(Pattern::As(None, id, e.span));
                }
                Ok(Pattern::Value(e))
            }
            Tok::None | Tok::True | Tok::False => {
                let e = self.atom()?;
                Ok(Pattern::Singleton(e))
            }
            _ => Ok(Pattern::Value(self.literal_or_value()?)),
        }
    }

    fn literal_or_value(&mut self) -> PResult<Ex<'a>> {
        if self.at(Tok::Name) {
            let start = self.start();
            let t = self.bump_tok();
            let mut e = self.alloc(ExprKind::Name(t.text), t.span);
            while self.eat(Tok::Dot) {
                let attr = self.ident()?;
                e = self.alloc(ExprKind::Attribute(e, attr), self.span_from(start));
            }
            return Ok(e);
        }
        self.arith()
    }

    // ---------- expressions ----------
    /// Target list for `for`/`del`: comma separated, stops before `stops`.
    fn target_list(&mut self, stops: &[Tok]) -> PResult<Ex<'a>> {
        let start = self.start();
        let first = self.target(stops)?;
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(Tok::Comma) {
            if stops.contains(&self.peek()) {
                break;
            }
            elts.push(self.target(stops)?);
        }
        Ok(self.alloc(
            ExprKind::Tuple {
                elts: self.slice(elts),
                parenthesized: false,
            },
            self.span_from(start),
        ))
    }

    fn target(&mut self, _stops: &[Tok]) -> PResult<Ex<'a>> {
        if self.at(Tok::Star) {
            self.star_expr()
        } else {
            self.bitor()
        }
    }

    fn star_expr(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        self.expect(Tok::Star)?;
        let v = self.bitor()?;
        Ok(self.alloc(ExprKind::Starred(v), self.span_from(start)))
    }

    /// `star_expressions`: possibly-unparenthesized tuple, elements may be starred.
    pub fn star_expressions(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let first = if self.at(Tok::Star) {
            self.star_expr()?
        } else {
            self.expression()?
        };
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(Tok::Comma) {
            if !self.starts_expr() {
                break;
            }
            elts.push(if self.at(Tok::Star) {
                self.star_expr()?
            } else {
                self.expression()?
            });
        }
        Ok(self.alloc(
            ExprKind::Tuple {
                elts: self.slice(elts),
                parenthesized: false,
            },
            self.span_from(start),
        ))
    }

    fn starts_expr(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Name
                | Tok::Int
                | Tok::Float
                | Tok::Imag
                | Tok::String
                | Tok::FString
                | Tok::LPar
                | Tok::LSqb
                | Tok::LBrace
                | Tok::Minus
                | Tok::Plus
                | Tok::Tilde
                | Tok::Not
                | Tok::Lambda
                | Tok::Await
                | Tok::None
                | Tok::True
                | Tok::False
                | Tok::Ellipsis
                | Tok::Star
                | Tok::Yield
        )
    }

    pub fn named_expression(&mut self) -> PResult<Ex<'a>> {
        if self.at(Tok::Name) && self.peek_at(1) == Tok::ColonEqual {
            let start = self.start();
            let t = self.bump_tok();
            let target = self.alloc(ExprKind::Name(t.text), t.span);
            self.bump_tok();
            let value = self.expression()?;
            return Ok(self.alloc(ExprKind::Named(target, value), self.span_from(start)));
        }
        self.expression()
    }

    pub fn expression(&mut self) -> PResult<Ex<'a>> {
        self.enter()?;
        let r = self.expression_inner();
        self.depth -= 1;
        r
    }

    fn expression_inner(&mut self) -> PResult<Ex<'a>> {
        if self.at(Tok::Lambda) {
            return self.lambda();
        }
        let start = self.start();
        let body = self.disjunction()?;
        if self.at(Tok::If) {
            // ternary; must have `else`, else it belongs to a comprehension
            let save = self.pos;
            self.bump_tok();
            let test = self.disjunction()?;
            if !self.eat(Tok::Else) {
                self.pos = save;
                return Ok(body);
            }
            let orelse = self.expression()?;
            return Ok(self.alloc(ExprKind::IfExp { test, body, orelse }, self.span_from(start)));
        }
        Ok(body)
    }

    fn lambda(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        self.expect(Tok::Lambda)?;
        let params = self.parameters(Tok::Colon, false)?;
        self.expect(Tok::Colon)?;
        let body = self.expression()?;
        Ok(self.alloc(ExprKind::Lambda(params, body), self.span_from(start)))
    }

    fn disjunction(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let first = self.conjunction()?;
        if !self.at(Tok::Or) {
            return Ok(first);
        }
        let mut v = vec![first];
        while self.eat(Tok::Or) {
            v.push(self.conjunction()?);
        }
        Ok(self.alloc(ExprKind::BoolOp(BoolOp::Or, self.slice(v)), self.span_from(start)))
    }

    fn conjunction(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let first = self.inversion()?;
        if !self.at(Tok::And) {
            return Ok(first);
        }
        let mut v = vec![first];
        while self.eat(Tok::And) {
            v.push(self.inversion()?);
        }
        Ok(self.alloc(ExprKind::BoolOp(BoolOp::And, self.slice(v)), self.span_from(start)))
    }

    fn inversion(&mut self) -> PResult<Ex<'a>> {
        if self.at(Tok::Not) {
            self.enter()?;
            let start = self.start();
            self.bump_tok();
            let v = self.inversion()?;
            self.depth -= 1;
            return Ok(self.alloc(ExprKind::Unary(UnaryOp::Not, v), self.span_from(start)));
        }
        self.comparison()
    }

    fn cmp_op(&mut self) -> Option<CmpOp> {
        let op = match self.peek() {
            Tok::EqEqual => CmpOp::Eq,
            Tok::NotEqual => CmpOp::NotEq,
            Tok::Less => CmpOp::Lt,
            Tok::LessEqual => CmpOp::LtE,
            Tok::Greater => CmpOp::Gt,
            Tok::GreaterEqual => CmpOp::GtE,
            Tok::In => CmpOp::In,
            Tok::Is => {
                if self.peek_at(1) == Tok::Not {
                    self.bump_tok();
                    CmpOp::IsNot
                } else {
                    CmpOp::Is
                }
            }
            Tok::Not if self.peek_at(1) == Tok::In => {
                self.bump_tok();
                CmpOp::NotIn
            }
            _ => return None,
        };
        self.bump_tok();
        Some(op)
    }

    fn comparison(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let left = self.bitor()?;
        let mut ops = Vec::new();
        while let Some(op) = self.cmp_op() {
            ops.push((op, self.bitor()?));
        }
        if ops.is_empty() {
            return Ok(left);
        }
        Ok(self.alloc(ExprKind::Compare(left, self.slice(ops)), self.span_from(start)))
    }

    fn binary(&mut self, level: u8) -> PResult<Ex<'a>> {
        if level == 6 {
            return self.factor();
        }
        let start = self.start();
        let mut left = self.binary(level + 1)?;
        loop {
            let op = match (level, self.peek()) {
                (0, Tok::VBar) => Operator::BitOr,
                (1, Tok::CircumFlex) => Operator::BitXor,
                (2, Tok::Amper) => Operator::BitAnd,
                (3, Tok::LeftShift) => Operator::LShift,
                (3, Tok::RightShift) => Operator::RShift,
                (4, Tok::Plus) => Operator::Add,
                (4, Tok::Minus) => Operator::Sub,
                (5, Tok::Star) => Operator::Mult,
                (5, Tok::Slash) => Operator::Div,
                (5, Tok::DoubleSlash) => Operator::FloorDiv,
                (5, Tok::Percent) => Operator::Mod,
                (5, Tok::At) => Operator::MatMult,
                _ => return Ok(left),
            };
            self.bump_tok();
            let right = self.binary(level + 1)?;
            left = self.alloc(ExprKind::BinOp(left, op, right), self.span_from(start));
        }
    }

    fn bitor(&mut self) -> PResult<Ex<'a>> {
        self.binary(0)
    }
    fn arith(&mut self) -> PResult<Ex<'a>> {
        self.binary(4)
    }

    fn factor(&mut self) -> PResult<Ex<'a>> {
        let op = match self.peek() {
            Tok::Plus => UnaryOp::UAdd,
            Tok::Minus => UnaryOp::USub,
            Tok::Tilde => UnaryOp::Invert,
            _ => return self.power(),
        };
        self.enter()?;
        let start = self.start();
        self.bump_tok();
        let v = self.factor()?;
        self.depth -= 1;
        Ok(self.alloc(ExprKind::Unary(op, v), self.span_from(start)))
    }

    fn power(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let base = if self.at(Tok::Await) {
            self.bump_tok();
            let v = self.primary()?;
            self.alloc(ExprKind::Await(v), self.span_from(start))
        } else {
            self.primary()?
        };
        if self.eat(Tok::DoubleStar) {
            let exp = self.factor()?;
            return Ok(self.alloc(ExprKind::BinOp(base, Operator::Pow, exp), self.span_from(start)));
        }
        Ok(base)
    }

    fn primary(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let mut e = self.atom()?;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.bump_tok();
                    let attr = self.ident()?;
                    e = self.alloc(ExprKind::Attribute(e, attr), self.span_from(start));
                }
                Tok::LPar => {
                    self.bump_tok();
                    let args = self.call_args()?;
                    self.expect(Tok::RPar)?;
                    e = self.alloc(ExprKind::Call(e, args), self.span_from(start));
                }
                Tok::LSqb => {
                    self.bump_tok();
                    let s = self.slices()?;
                    self.expect(Tok::RSqb)?;
                    e = self.alloc(ExprKind::Subscript(e, s), self.span_from(start));
                }
                _ => return Ok(e),
            }
        }
    }

    fn slices(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let first = self.slice_item()?;
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(Tok::Comma) {
            if self.at(Tok::RSqb) {
                break;
            }
            elts.push(self.slice_item()?);
        }
        Ok(self.alloc(
            ExprKind::Tuple {
                elts: self.slice(elts),
                parenthesized: false,
            },
            self.span_from(start),
        ))
    }

    fn slice_item(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        if self.at(Tok::Star) {
            return self.star_expr();
        }
        let lower = if self.at(Tok::Colon) {
            None
        } else {
            Some(self.named_expression()?)
        };
        if !self.at(Tok::Colon) {
            return Ok(lower.unwrap());
        }
        self.bump_tok();
        let upper = if matches!(self.peek(), Tok::Colon | Tok::Comma | Tok::RSqb) {
            None
        } else {
            Some(self.expression()?)
        };
        let step = if self.eat(Tok::Colon) {
            if matches!(self.peek(), Tok::Comma | Tok::RSqb) {
                None
            } else {
                Some(self.expression()?)
            }
        } else {
            None
        };
        Ok(self.alloc(ExprKind::Slice(lower, upper, step), self.span_from(start)))
    }

    fn call_args(&mut self) -> PResult<&'a [Arg<'a>]> {
        let mut args = Vec::new();
        while !self.at(Tok::RPar) {
            let start = self.start();
            if self.eat(Tok::Star) {
                args.push(Arg::Starred(self.expression()?));
            } else if self.eat(Tok::DoubleStar) {
                args.push(Arg::DoubleStarred(self.expression()?));
            } else if self.at(Tok::Name) && self.peek_at(1) == Tok::Equal {
                let n = self.ident()?;
                self.bump_tok();
                args.push(Arg::Keyword(n, self.expression()?));
            } else {
                let e = self.named_expression()?;
                if self.at(Tok::For) || self.at(Tok::Async) {
                    let gens = self.comprehension_clauses()?;
                    args.push(Arg::Positional(
                        self.alloc(ExprKind::Generator(e, gens), self.span_from(start)),
                    ));
                } else {
                    args.push(Arg::Positional(e));
                }
            }
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        Ok(self.slice(args))
    }

    fn comprehension_clauses(&mut self) -> PResult<&'a [Comprehension<'a>]> {
        let mut v = Vec::new();
        while self.at(Tok::For) || (self.at(Tok::Async) && self.peek_at(1) == Tok::For) {
            let is_async = self.eat(Tok::Async);
            self.expect(Tok::For)?;
            let target = self.target_list(&[Tok::In])?;
            self.expect(Tok::In)?;
            let iter = self.disjunction()?;
            let mut ifs = Vec::new();
            while self.eat(Tok::If) {
                ifs.push(self.disjunction()?);
            }
            v.push(Comprehension {
                target,
                iter,
                ifs: self.slice(ifs),
                is_async,
            });
        }
        Ok(self.slice(v))
    }

    fn yield_expr(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        self.expect(Tok::Yield)?;
        if self.eat(Tok::From) {
            let v = self.expression()?;
            return Ok(self.alloc(ExprKind::YieldFrom(v), self.span_from(start)));
        }
        let v = if self.starts_expr() && !self.at(Tok::Yield) {
            Some(self.star_expressions()?)
        } else {
            None
        };
        Ok(self.alloc(ExprKind::Yield(v), self.span_from(start)))
    }

    fn with_parens(&self, e: Ex<'a>) -> Ex<'a> {
        self.bump.alloc(Expr {
            parens: e.parens.saturating_add(1),
            ..*e
        })
    }

    fn atom(&mut self) -> PResult<Ex<'a>> {
        self.enter()?;
        let r = self.atom_inner();
        self.depth -= 1;
        r
    }

    fn atom_inner(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let t = self.tok();
        let c = |k| ExprKind::Constant(k);
        match t.kind {
            Tok::Name => {
                self.bump_tok();
                Ok(self.alloc(ExprKind::Name(t.text), t.span))
            }
            Tok::Int => {
                self.bump_tok();
                Ok(self.alloc(c(Constant::Int(t.text)), t.span))
            }
            Tok::Float => {
                self.bump_tok();
                Ok(self.alloc(c(Constant::Float(t.text)), t.span))
            }
            Tok::Imag => {
                self.bump_tok();
                Ok(self.alloc(c(Constant::Imag(t.text)), t.span))
            }
            Tok::None => {
                self.bump_tok();
                Ok(self.alloc(c(Constant::None), t.span))
            }
            Tok::True => {
                self.bump_tok();
                Ok(self.alloc(c(Constant::True), t.span))
            }
            Tok::False => {
                self.bump_tok();
                Ok(self.alloc(c(Constant::False), t.span))
            }
            Tok::Ellipsis => {
                self.bump_tok();
                Ok(self.alloc(c(Constant::Ellipsis), t.span))
            }
            Tok::String | Tok::FString => self.strings(),
            Tok::LPar => {
                self.bump_tok();
                if self.eat(Tok::RPar) {
                    return Ok(self.alloc(
                        ExprKind::Tuple {
                            elts: &[],
                            parenthesized: true,
                        },
                        self.span_from(start),
                    ));
                }
                if self.at(Tok::Yield) {
                    let y = self.yield_expr()?;
                    self.expect(Tok::RPar)?;
                    return Ok(self.with_parens(y));
                }
                let first = if self.at(Tok::Star) {
                    self.star_expr()?
                } else {
                    self.named_expression()?
                };
                if self.at(Tok::For) || self.at(Tok::Async) {
                    let gens = self.comprehension_clauses()?;
                    self.expect(Tok::RPar)?;
                    return Ok(self.alloc(ExprKind::Generator(first, gens), self.span_from(start)));
                }
                if self.eat(Tok::RPar) {
                    if matches!(first.kind, ExprKind::Starred(_)) {
                        return self.err("cannot use starred expression here");
                    }
                    return Ok(self.with_parens(first));
                }
                let mut elts = vec![first];
                while self.eat(Tok::Comma) {
                    if self.at(Tok::RPar) {
                        break;
                    }
                    elts.push(if self.at(Tok::Star) {
                        self.star_expr()?
                    } else {
                        self.named_expression()?
                    });
                }
                self.expect(Tok::RPar)?;
                Ok(self.alloc(
                    ExprKind::Tuple {
                        elts: self.slice(elts),
                        parenthesized: true,
                    },
                    self.span_from(start),
                ))
            }
            Tok::LSqb => {
                self.bump_tok();
                if self.eat(Tok::RSqb) {
                    return Ok(self.alloc(ExprKind::List(&[]), self.span_from(start)));
                }
                let first = if self.at(Tok::Star) {
                    self.star_expr()?
                } else {
                    self.named_expression()?
                };
                if self.at(Tok::For) || self.at(Tok::Async) {
                    let gens = self.comprehension_clauses()?;
                    self.expect(Tok::RSqb)?;
                    return Ok(self.alloc(ExprKind::ListComp(first, gens), self.span_from(start)));
                }
                let mut elts = vec![first];
                while self.eat(Tok::Comma) {
                    if self.at(Tok::RSqb) {
                        break;
                    }
                    elts.push(if self.at(Tok::Star) {
                        self.star_expr()?
                    } else {
                        self.named_expression()?
                    });
                }
                self.expect(Tok::RSqb)?;
                Ok(self.alloc(ExprKind::List(self.slice(elts)), self.span_from(start)))
            }
            Tok::LBrace => self.dict_or_set(),
            _ => self.err("expected expression"),
        }
    }

    fn dict_or_set(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        self.expect(Tok::LBrace)?;
        if self.eat(Tok::RBrace) {
            return Ok(self.alloc(ExprKind::Dict(&[]), self.span_from(start)));
        }
        let (first_key, first_val, is_dict) = if self.eat(Tok::DoubleStar) {
            (None, self.bitor()?, true)
        } else {
            let k = if self.at(Tok::Star) {
                self.star_expr()?
            } else {
                self.named_expression()?
            };
            if self.eat(Tok::Colon) {
                let v = self.expression()?;
                (Some(k), v, true)
            } else {
                (None, k, false)
            }
        };
        if is_dict {
            if let Some(k) = first_key
                && (self.at(Tok::For) || self.at(Tok::Async))
            {
                let gens = self.comprehension_clauses()?;
                self.expect(Tok::RBrace)?;
                return Ok(self.alloc(ExprKind::DictComp(k, first_val, gens), self.span_from(start)));
            }
            let mut items = vec![(first_key, first_val)];
            while self.eat(Tok::Comma) {
                if self.at(Tok::RBrace) {
                    break;
                }
                if self.eat(Tok::DoubleStar) {
                    items.push((None, self.bitor()?));
                } else {
                    let k = self.expression()?;
                    self.expect(Tok::Colon)?;
                    items.push((Some(k), self.expression()?));
                }
            }
            self.expect(Tok::RBrace)?;
            return Ok(self.alloc(ExprKind::Dict(self.slice(items)), self.span_from(start)));
        }
        if self.at(Tok::For) || self.at(Tok::Async) {
            let gens = self.comprehension_clauses()?;
            self.expect(Tok::RBrace)?;
            return Ok(self.alloc(ExprKind::SetComp(first_val, gens), self.span_from(start)));
        }
        let mut elts = vec![first_val];
        while self.eat(Tok::Comma) {
            if self.at(Tok::RBrace) {
                break;
            }
            elts.push(if self.at(Tok::Star) {
                self.star_expr()?
            } else {
                self.named_expression()?
            });
        }
        self.expect(Tok::RBrace)?;
        Ok(self.alloc(ExprKind::Set(self.slice(elts)), self.span_from(start)))
    }

    fn strings(&mut self) -> PResult<Ex<'a>> {
        let start = self.start();
        let mut parts = Vec::new();
        let mut has_bytes = false;
        let mut has_str = false;
        while matches!(self.peek(), Tok::String | Tok::FString) {
            let t = self.bump_tok();
            let pre_len = t.text.find(['\'', '"']).unwrap_or(0);
            let prefix = &t.text[..pre_len];
            let (is_f, raw, is_b) = string_prefix(prefix).unwrap_or((false, false, false));
            if is_b {
                has_bytes = true
            } else {
                has_str = true
            }
            let kind = if is_b {
                StrKind::Bytes
            } else if is_f {
                StrKind::FString
            } else {
                StrKind::Str
            };
            let fparts = if is_f {
                let mut raw_parts = Vec::new();
                let qpos = t.span.start as usize + pre_len;
                let _ = scan_string(
                    self.src.as_bytes(),
                    qpos,
                    t.span.end as usize,
                    true,
                    raw,
                    Some(&mut raw_parts),
                );
                self.convert_fparts(&raw_parts)?
            } else {
                &[]
            };
            parts.push(StrLit {
                span: t.span,
                text: t.text,
                kind,
                fparts,
            });
        }
        if has_bytes && has_str {
            return Err(ParseError {
                span: self.span_from(start),
                msg: "cannot mix bytes and nonbytes literals".into(),
            });
        }
        Ok(self.alloc(ExprKind::Str(self.slice(parts)), self.span_from(start)))
    }

    fn convert_fparts(&mut self, raw: &[FPartRaw]) -> PResult<&'a [FPart<'a>]> {
        let mut out = Vec::with_capacity(raw.len());
        for p in raw {
            match p {
                FPartRaw::Literal(s) => out.push(FPart::Literal(*s)),
                FPartRaw::Field {
                    expr,
                    debug,
                    conversion,
                    spec,
                    ..
                } => {
                    let e = self.sub_expression(*expr)?;
                    let spec = match spec {
                        Some(s) => Some(self.convert_fparts(s)?),
                        None => None,
                    };
                    out.push(FPart::Field {
                        expr: e,
                        debug: *debug,
                        conversion: *conversion,
                        spec,
                    });
                }
            }
        }
        Ok(self.slice(out))
    }

    fn sub_expression(&mut self, span: Span) -> PResult<Ex<'a>> {
        let lexed = Lexer::new_range(self.src, span.start as usize, span.end as usize, false).tokenize();
        if let Some(e) = lexed.errors.first() {
            return Err(ParseError {
                span: e.span,
                msg: e.msg.clone(),
            });
        }
        let mut sub = Parser {
            src: self.src,
            bump: self.bump,
            toks: lexed.tokens,
            pos: 0,
            depth: self.depth,
        };
        let e = if sub.at(Tok::Yield) {
            sub.yield_expr()?
        } else {
            sub.star_expressions()?
        };
        if !sub.at(Tok::EndMarker) {
            return sub.err("unexpected token in f-string expression");
        }
        Ok(e)
    }
}

/// Parses a single expression (used by tests and tools).
pub fn parse_expression<'a>(src: &'a str, bump: &'a Bump) -> PResult<Ex<'a>> {
    let lexed = Lexer::new_range(src, 0, src.len(), false).tokenize();
    if let Some(e) = lexed.errors.first() {
        return Err(ParseError {
            span: e.span,
            msg: e.msg.clone(),
        });
    }
    let mut p = Parser {
        src,
        bump,
        toks: lexed.tokens,
        pos: 0,
        depth: 0,
    };
    let e = p.star_expressions()?;
    if !p.at(Tok::EndMarker) {
        return p.err("unexpected trailing tokens");
    }
    Ok(e)
}
