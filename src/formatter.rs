//! Deterministic, Black-like formatter.
//!
//! Statements are re-printed from the AST. Comments are re-attached by
//! position: own-line comments before a statement, trailing comments after
//! it. Statements (or compound headers) that contain comments *inside* them
//! are emitted verbatim (re-indented on their first line only), which keeps
//! formatting AST-preserving and idempotent. Lines longer than the limit are
//! split at the outermost bracket ("right hand split"), one item per line
//! with a trailing comma.

use crate::ast::*;
use crate::lexer::{Tok, Token};
use crate::parser::parse_module;
use crate::span::Span;
use bumpalo::Bump;

const P_TUPLE: u8 = 0;
const P_LAMBDA: u8 = 2;
const P_IFEXP: u8 = 3;
const P_OR: u8 = 4;
const P_AND: u8 = 5;
const P_NOT: u8 = 6;
const P_CMP: u8 = 7;
const P_BOR: u8 = 8;
const P_UNARY: u8 = 14;
const P_POW: u8 = 15;
const P_AWAIT: u8 = 16;
const P_PRIMARY: u8 = 17;
const P_ATOM: u8 = 18;

pub fn format_source(src: &str, line_length: usize) -> Result<String, String> {
    let bump = Bump::new();
    let parsed = parse_module(src, &bump).map_err(|e| format!("syntax error at byte {}: {}", e.span.start, e.msg))?;
    let mut f = Formatter {
        src,
        comments: parsed.comments,
        next_comment: 0,
        out: String::with_capacity(src.len() + 64),
        ll: line_length,
        tokens: parsed.tokens,
    };
    // shebang / leading comments are handled like any other own-line comments
    f.block(parsed.module.body, 0, src.len() as u32, true);
    f.flush_comments_before(u32::MAX, 0, true, false);
    let mut out = f.out;
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out.trim().is_empty() {
        return Ok(String::new());
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

struct Formatter<'s, 'a> {
    src: &'s str,
    comments: Vec<Span>,
    next_comment: usize,
    out: String,
    ll: usize,
    tokens: Vec<Token<'a>>,
}

fn op_prec(op: Operator) -> u8 {
    match op {
        Operator::BitOr => 8,
        Operator::BitXor => 9,
        Operator::BitAnd => 10,
        Operator::LShift | Operator::RShift => 11,
        Operator::Add | Operator::Sub => 12,
        Operator::Mult | Operator::MatMult | Operator::Div | Operator::Mod | Operator::FloorDiv => 13,
        Operator::Pow => P_POW,
    }
}

fn prec(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Tuple { elts, parenthesized } => {
            if *parenthesized || elts.is_empty() {
                P_ATOM
            } else {
                P_TUPLE
            }
        }
        ExprKind::Yield(_) | ExprKind::YieldFrom(_) => P_TUPLE,
        ExprKind::Named(..) => P_ATOM,
        ExprKind::Lambda(..) => P_LAMBDA,
        ExprKind::IfExp { .. } => P_IFEXP,
        ExprKind::BoolOp(BoolOp::Or, _) => P_OR,
        ExprKind::BoolOp(BoolOp::And, _) => P_AND,
        ExprKind::Unary(UnaryOp::Not, _) => P_NOT,
        ExprKind::Compare(..) => P_CMP,
        ExprKind::BinOp(_, op, _) => op_prec(*op),
        ExprKind::Unary(..) => P_UNARY,
        ExprKind::Await(_) => P_AWAIT,
        ExprKind::Call(..) | ExprKind::Attribute(..) | ExprKind::Subscript(..) => P_PRIMARY,
        ExprKind::Starred(_) => P_BOR,
        _ => P_ATOM,
    }
}

fn line_has_newline(s: &str) -> bool {
    s.contains('\n')
}

/// Prefer double quotes when that needs no escaping changes.
fn normalize_quotes(text: &str) -> String {
    let pre_len = text.find(['\'', '"']).unwrap_or(0);
    let (prefix, rest) = text.split_at(pre_len);
    if !rest.starts_with('\'') {
        return text.to_string();
    }
    let triple = rest.starts_with("'''") && rest.len() >= 6;
    let q = if triple { 3 } else { 1 };
    let body = &rest[q..rest.len() - q];
    if body.contains('"') || body.contains('\\') || (prefix.contains(['f', 'F']) && body.contains('\'')) {
        return text.to_string();
    }
    let quotes = if triple { "\"\"\"" } else { "\"" };
    format!("{prefix}{quotes}{body}{quotes}")
}

impl<'s, 'a> Formatter<'s, 'a> {
    // ------------------------------------------------------------ comments
    fn col_of(&self, pos: u32) -> usize {
        let p = pos as usize;
        let ls = self.src[..p].rfind('\n').map(|i| i + 1).unwrap_or(0);
        p - ls
    }

    fn same_line(&self, a: u32, b: u32) -> bool {
        let (a, b) = (a.min(b) as usize, a.max(b) as usize);
        !self.src[a..b].contains('\n')
    }

    /// Number of blank lines in the source between position `a` (end of
    /// something) and `b` (start of something).
    fn blanks_between(&self, a: u32, b: u32) -> usize {
        if a >= b {
            return 0;
        }
        let s = &self.src[a as usize..b as usize];
        let lines: Vec<&str> = s.split('\n').collect();
        if lines.len() <= 2 {
            return 0;
        }
        lines[1..lines.len() - 1].iter().filter(|l| l.trim().is_empty()).count()
    }

    fn indent(&mut self, level: usize) {
        for _ in 0..level {
            self.out.push_str("    ");
        }
    }

    fn ensure_blank_lines(&mut self, n: usize) {
        if self.out.is_empty() {
            return;
        }
        let existing = self.out.len() - self.out.trim_end_matches('\n').len();
        let have = existing.saturating_sub(1);
        for _ in have..n {
            self.out.push('\n');
        }
    }

    /// Emits own-line comments that start before `limit` (optionally only those at
    /// column >= `min_col`). `prev_end` is used to preserve blank lines.
    fn flush_comments_before(&mut self, limit: u32, level: usize, top: bool, check_col: bool) -> bool {
        self.flush_comments_col(limit, level, top, if check_col { Some(level) } else { None })
    }

    fn flush_comments_col(&mut self, limit: u32, level: usize, top: bool, min_src_col: Option<usize>) -> bool {
        let mut any = false;
        while self.next_comment < self.comments.len() {
            let c = self.comments[self.next_comment];
            if c.start >= limit {
                break;
            }
            if let Some(mc) = min_src_col
                && self.col_of(c.start) < mc
            {
                break;
            }
            let prev = self.prev_source_end(c.start);
            let cap = if top { 2 } else { 1 };
            let blanks = self.blanks_between(prev, c.start).min(cap);
            if !self.out.is_empty() && !self.out.ends_with('\n') {
                // comment trailing previous emitted code on the same source line
                if self.same_line(prev, c.start) {
                    self.out.push_str("  ");
                    self.out.push_str(&self.src[c.range()]);
                    self.next_comment += 1;
                    continue;
                }
                self.out.push('\n');
            }
            if !self.at_block_start() {
                self.ensure_blank_lines(blanks);
            }
            self.indent(level);
            self.out.push_str(self.src[c.range()].trim_end());
            self.out.push('\n');
            self.next_comment += 1;
            any = true;
        }
        any
    }

    fn at_block_start(&self) -> bool {
        self.out.trim_end_matches([' ', '\n']).ends_with(':') || self.out.is_empty()
    }

    /// End of the last non-whitespace source text before `pos`.
    fn prev_source_end(&self, pos: u32) -> u32 {
        let s = &self.src[..pos as usize];
        let t = s.trim_end();
        t.len() as u32
    }

    /// Emits a trailing comment that sits on the same line right after `end`.
    fn trailing_comment(&mut self, end: u32) {
        if self.next_comment < self.comments.len() {
            let c = self.comments[self.next_comment];
            if c.start >= end && self.same_line(end, c.start) {
                self.out.push_str("  ");
                self.out.push_str(self.src[c.range()].trim_end());
                self.next_comment += 1;
            }
        }
    }

    fn has_comment_in(&self, s: Span) -> bool {
        let i = self.comments.partition_point(|c| c.start < s.start);
        i < self.comments.len() && self.comments[i].start < s.end
    }

    fn skip_comments_in(&mut self, s: Span) {
        while self.next_comment < self.comments.len() && self.comments[self.next_comment].start < s.end {
            self.next_comment += 1;
        }
    }

    /// The colon ending the header whose body starts at `body_start`.
    fn header_colon(&self, body_start: u32) -> u32 {
        let i = self.tokens.partition_point(|t| t.span.start < body_start);
        let mut j = i;
        while j > 0 {
            j -= 1;
            if self.tokens[j].kind == Tok::Colon {
                return self.tokens[j].span.start;
            }
            if !matches!(self.tokens[j].kind, Tok::Newline | Tok::Indent | Tok::Dedent) {
                break;
            }
        }
        body_start
    }

    // ------------------------------------------------------------ blocks
    fn is_def(s: &Stmt) -> bool {
        matches!(s.kind, StmtKind::FunctionDef { .. } | StmtKind::ClassDef { .. })
    }

    fn block(&mut self, body: &[Stmt], level: usize, limit: u32, top: bool) {
        let cap = if top { 2 } else { 1 };
        for (i, s) in body.iter().enumerate() {
            let first = i == 0;
            let prev_end = if first { None } else { Some(body[i - 1].span.end) };
            // blank lines before leading comments / the statement
            let lead_start =
                if self.next_comment < self.comments.len() && self.comments[self.next_comment].start < s.span.start {
                    self.comments[self.next_comment].start
                } else {
                    s.span.start
                };
            if let Some(pe) = prev_end {
                let mut want = self
                    .blanks_between(self.prev_source_end(lead_start).max(pe), lead_start)
                    .min(cap);
                if Self::is_def(s) || Self::is_def(&body[i - 1]) {
                    want = if top { 2 } else { 1 };
                }
                if !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
                self.ensure_blank_lines(want);
            }
            if self.flush_comments_before(s.span.start, level, top, false) {
                let b = self
                    .blanks_between(self.prev_source_end(s.span.start), s.span.start)
                    .min(cap);
                self.ensure_blank_lines(b);
            }
            self.stmt(
                s,
                level,
                if i + 1 < body.len() {
                    body[i + 1].span.start
                } else {
                    limit
                },
            );
        }
        // comments at the end of the block that are indented like the block
        if let Some(f) = body.first() {
            let col = self.col_of(f.span.start);
            let min = if level == 0 { 0 } else { col };
            if !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            self.flush_comments_col(limit, level, top, Some(min));
        }
    }

    /// Copies source text verbatim; every logical line inside it (e.g. each
    /// decorator) is re-indented to `level`, continuation lines are untouched.
    fn emit_verbatim(&mut self, span: Span, level: usize) {
        let mut line_starts: Vec<u32> = Vec::new();
        let i = self.tokens.partition_point(|t| t.span.start < span.start);
        let mut after_newline = false;
        for t in &self.tokens[i..] {
            if t.span.start >= span.end {
                break;
            }
            match t.kind {
                Tok::Newline => after_newline = true,
                Tok::Indent | Tok::Dedent => {}
                _ => {
                    if after_newline {
                        line_starts.push(t.span.start);
                    }
                    after_newline = false;
                }
            }
        }
        self.indent(level);
        let mut last = span.start as usize;
        for ls in line_starts {
            let ls = ls as usize;
            let line_begin = self.src[..ls].rfind('\n').map(|p| p + 1).unwrap_or(0);
            if line_begin < last {
                continue;
            }
            self.out.push_str(&self.src[last..line_begin]);
            self.indent(level);
            last = ls;
        }
        self.out.push_str(&self.src[last..span.end as usize]);
        self.skip_comments_in(span);
    }

    fn stmt(&mut self, s: &Stmt, level: usize, limit: u32) {
        let compound_body: Option<&[Stmt]> = match &s.kind {
            StmtKind::FunctionDef { body, .. }
            | StmtKind::ClassDef { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::While { body, .. }
            | StmtKind::If { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::Try { body, .. } => Some(body),
            StmtKind::Match { cases, .. } => Some(cases[0].body),
            _ => None,
        };
        let Some(first_body) = compound_body else {
            // simple statement
            if self.has_comment_in(s.span) {
                self.emit_verbatim(s.span, level);
            } else {
                let text = self.simple_stmt(s, level);
                self.indent(level);
                self.out.push_str(&text);
            }
            self.trailing_comment(s.span.end);
            self.out.push('\n');
            return;
        };
        let _ = first_body;
        match &s.kind {
            StmtKind::If { test, body, orelse, .. } => {
                let h = format!("if {}:", self.split(test, level, "if ".len(), 1, P_LAMBDA));
                self.header(s.span.start, &h, body, level);
                self.suite(body, level, self.clause_limit(body, orelse, limit));
                let mut orelse = *orelse;
                loop {
                    if orelse.is_empty() {
                        break;
                    }
                    if orelse.len() == 1
                        && let StmtKind::If {
                            test,
                            body,
                            orelse: o2,
                            elif: true,
                        } = &orelse[0].kind
                    {
                        self.flush_comments_before(orelse[0].span.start, level, false, false);
                        let h = format!("elif {}:", self.split(test, level, "elif ".len(), 1, P_LAMBDA));
                        self.header(orelse[0].span.start, &h, body, level);
                        self.suite(body, level, self.clause_limit(body, o2, limit));
                        orelse = o2;
                        continue;
                    }
                    self.else_clause("else:", orelse, level, limit);
                    break;
                }
            }
            StmtKind::While { test, body, orelse } => {
                let h = format!("while {}:", self.split(test, level, 6, 1, P_LAMBDA));
                self.header(s.span.start, &h, body, level);
                self.suite(body, level, self.clause_limit(body, orelse, limit));
                if !orelse.is_empty() {
                    self.else_clause("else:", orelse, level, limit);
                }
            }
            StmtKind::For {
                is_async,
                target,
                iter,
                body,
                orelse,
            } => {
                let kw = if *is_async { "async for " } else { "for " };
                let t = self.p(target, P_TUPLE);
                let h = format!(
                    "{kw}{t} in {}:",
                    self.split(iter, level, kw.len() + t.len() + 4, 1, P_TUPLE)
                );
                self.header(s.span.start, &h, body, level);
                self.suite(body, level, self.clause_limit(body, orelse, limit));
                if !orelse.is_empty() {
                    self.else_clause("else:", orelse, level, limit);
                }
            }
            StmtKind::With { is_async, items, body } => {
                let kw = if *is_async { "async with " } else { "with " };
                let flat: Vec<String> = items.iter().map(|i| self.with_item(i)).collect();
                let one = format!("{kw}{}:", flat.join(", "));
                let h = if one.len() + 4 * level <= self.ll || (items.len() == 1 && items[0].vars.is_none()) {
                    one
                } else {
                    let mut h = format!("{kw}(\n");
                    for it in &flat {
                        h.push_str(&"    ".repeat(level + 1));
                        h.push_str(it);
                        h.push_str(",\n");
                    }
                    h.push_str(&"    ".repeat(level));
                    h.push_str("):");
                    h
                };
                self.header(s.span.start, &h, body, level);
                self.suite(body, level, limit);
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
                star,
            } => {
                self.header(s.span.start, "try:", body, level);
                let next = handlers
                    .first()
                    .map(|h| h.span.start)
                    .or(orelse.first().map(|x| x.span.start))
                    .or(finalbody.first().map(|x| x.span.start))
                    .unwrap_or(limit);
                self.suite(body, level, self.keyword_before(next));
                for (i, h) in handlers.iter().enumerate() {
                    self.flush_comments_before(h.span.start, level, false, false);
                    let mut hd = String::from(if *star { "except*" } else { "except" });
                    if let Some(t) = h.typ {
                        hd.push(' ');
                        hd.push_str(&self.p(t, P_TUPLE));
                    }
                    if let Some(n) = h.name {
                        hd.push_str(" as ");
                        hd.push_str(n.name);
                    }
                    hd.push(':');
                    self.header(h.span.start, &hd, h.body, level);
                    let next = handlers
                        .get(i + 1)
                        .map(|h| h.span.start)
                        .or(orelse.first().map(|x| x.span.start))
                        .or(finalbody.first().map(|x| x.span.start))
                        .unwrap_or(limit);
                    self.suite(h.body, level, self.keyword_before(next));
                }
                if !orelse.is_empty() {
                    let nl = finalbody
                        .first()
                        .map(|x| self.keyword_before(x.span.start))
                        .unwrap_or(limit);
                    self.else_clause("else:", orelse, level, nl);
                }
                if !finalbody.is_empty() {
                    self.else_clause("finally:", finalbody, level, limit);
                }
            }
            StmtKind::FunctionDef { decorators, .. } | StmtKind::ClassDef { decorators, .. } => {
                let body = first_body;
                let header = self.def_header(s, level);
                let colon = self.header_colon(body[0].span.start);
                let hspan = Span::new(s.span.start, colon + 1);
                let _ = decorators;
                if self.has_comment_in(hspan) {
                    self.emit_verbatim(hspan, level);
                } else {
                    self.indent(level);
                    self.out.push_str(&header);
                }
                self.trailing_comment(colon + 1);
                self.out.push('\n');
                self.suite(body, level, limit);
            }
            StmtKind::Match { subject, cases } => {
                let h = format!("match {}:", self.p(subject, P_TUPLE));
                let colon = self.header_colon(cases[0].span.start);
                let hspan = Span::new(s.span.start, colon + 1);
                if self.has_comment_in(hspan) {
                    self.emit_verbatim(hspan, level);
                } else {
                    self.indent(level);
                    self.out.push_str(&h);
                }
                self.trailing_comment(colon + 1);
                self.out.push('\n');
                for (i, c) in cases.iter().enumerate() {
                    self.flush_comments_before(c.span.start, level + 1, false, false);
                    let mut hd = format!("case {}", self.pattern(&c.pattern, true));
                    if let Some(g) = c.guard {
                        hd.push_str(" if ");
                        hd.push_str(&self.p(g, P_LAMBDA));
                    }
                    hd.push(':');
                    self.header(c.span.start, &hd, c.body, level + 1);
                    let next = cases.get(i + 1).map(|c| c.span.start).unwrap_or(limit);
                    self.suite(c.body, level + 1, next);
                }
            }
            _ => unreachable!(),
        }
    }

    /// Start of the keyword token (`else`, `finally`, ...) that precedes `pos`.
    fn keyword_before(&self, pos: u32) -> u32 {
        let i = self.tokens.partition_point(|t| t.span.start < pos);
        let mut j = i;
        let mut steps = 0;
        while j > 0 && steps < 6 {
            j -= 1;
            steps += 1;
            match self.tokens[j].kind {
                Tok::Else | Tok::Finally | Tok::Elif => return self.tokens[j].span.start,
                Tok::Newline | Tok::Indent | Tok::Dedent | Tok::Colon => continue,
                _ => break,
            }
        }
        pos
    }

    fn clause_limit(&self, _body: &[Stmt], orelse: &[Stmt], limit: u32) -> u32 {
        match orelse.first() {
            Some(o) if matches!(o.kind, StmtKind::If { elif: true, .. }) => o.span.start,
            Some(o) => self.keyword_before(o.span.start),
            None => limit,
        }
    }

    fn else_clause(&mut self, kw: &str, body: &[Stmt], level: usize, limit: u32) {
        let kwpos = self.keyword_before(body[0].span.start);
        self.flush_comments_before(kwpos, level, false, false);
        self.header(kwpos, kw, body, level);
        self.suite(body, level, limit);
    }

    /// Emits a compound-statement header line (verbatim if comments are inside it).
    fn header(&mut self, start: u32, text: &str, body: &[Stmt], level: usize) {
        let colon = self.header_colon(body[0].span.start);
        let hspan = Span::new(start, colon + 1);
        if self.has_comment_in(hspan) {
            self.emit_verbatim(hspan, level);
        } else {
            self.indent(level);
            self.out.push_str(text);
        }
        self.trailing_comment(colon + 1);
        self.out.push('\n');
    }

    fn suite(&mut self, body: &[Stmt], level: usize, limit: u32) {
        self.block(body, level + 1, limit, false);
    }

    // ------------------------------------------------------------ statements
    fn simple_stmt(&self, s: &Stmt, level: usize) -> String {
        match &s.kind {
            StmtKind::Expr(e) => self.split(e, level, 0, 0, P_TUPLE),
            StmtKind::Assign { targets, value } => {
                let mut head = String::new();
                for t in *targets {
                    head.push_str(&self.p(t, P_TUPLE));
                    head.push_str(" = ");
                }
                let v = self.split(value, level, head.len(), 0, P_TUPLE);
                head + &v
            }
            StmtKind::AugAssign { target, op, value } => {
                let head = format!("{} {}= ", self.p(target, P_TUPLE), op.as_str());
                let v = self.split(value, level, head.len(), 0, P_TUPLE);
                head + &v
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
            } => {
                let t = if target.parens > 0 {
                    format!("({})", self.p(target, P_TUPLE))
                } else {
                    self.p(target, P_TUPLE)
                };
                let mut head = format!("{t}: {}", self.p(annotation, P_LAMBDA));
                if let Some(v) = value {
                    head.push_str(" = ");
                    let v = self.split(v, level, head.len(), 0, P_TUPLE);
                    head.push_str(&v);
                }
                head
            }
            StmtKind::Return(v) => match v {
                Some(v) if matches!(v.kind, ExprKind::Yield(_) | ExprKind::YieldFrom(_)) => {
                    format!("return ({})", self.flat(v))
                }
                Some(v) => format!("return {}", self.split(v, level, 7, 0, P_TUPLE)),
                None => "return".into(),
            },
            StmtKind::Delete(ts) => format!(
                "del {}",
                ts.iter().map(|t| self.p(t, P_BOR)).collect::<Vec<_>>().join(", ")
            ),
            StmtKind::Raise { exc, cause } => {
                let mut r = String::from("raise");
                if let Some(e) = exc {
                    r.push(' ');
                    r.push_str(&self.split(e, level, 6, 0, P_LAMBDA));
                }
                if let Some(c) = cause {
                    r.push_str(" from ");
                    r.push_str(&self.p(c, P_LAMBDA));
                }
                r
            }
            StmtKind::Assert { test, msg } => {
                let mut r = format!("assert {}", self.p(test, P_LAMBDA));
                if let Some(m) = msg {
                    r.push_str(", ");
                    r.push_str(&self.p(m, P_LAMBDA));
                }
                r
            }
            StmtKind::Import(names) => {
                let parts: Vec<String> = names.iter().map(|a| self.alias(a)).collect();
                format!("import {}", parts.join(", "))
            }
            StmtKind::ImportFrom {
                module,
                level: lv,
                names,
            } => {
                let head = format!(
                    "from {}{} import ",
                    ".".repeat(*lv as usize),
                    module
                        .map(|m| m.name.split_whitespace().collect::<String>())
                        .unwrap_or_default()
                );
                let parts: Vec<String> = names.iter().map(|a| self.alias(a)).collect();
                let one = format!("{head}{}", parts.join(", "));
                if one.len() + 4 * level <= self.ll || parts.len() == 1 && parts[0] == "*" {
                    one
                } else {
                    let ind = "    ".repeat(level + 1);
                    let mut r = format!("{head}(\n");
                    for p in parts {
                        r.push_str(&format!("{ind}{p},\n"));
                    }
                    r.push_str(&"    ".repeat(level));
                    r.push(')');
                    r
                }
            }
            StmtKind::Global(n) => format!("global {}", n.iter().map(|n| n.name).collect::<Vec<_>>().join(", ")),
            StmtKind::Nonlocal(n) => format!("nonlocal {}", n.iter().map(|n| n.name).collect::<Vec<_>>().join(", ")),
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => format!(
                "type {}{} = {}",
                self.p(name, P_ATOM),
                self.tparams(type_params),
                self.p(value, P_LAMBDA)
            ),
            StmtKind::Pass => "pass".into(),
            StmtKind::Break => "break".into(),
            StmtKind::Continue => "continue".into(),
            _ => unreachable!(),
        }
    }

    fn alias(&self, a: &Alias) -> String {
        let n: String = a.name.name.split_whitespace().collect();
        match a.asname {
            Some(x) => format!("{n} as {}", x.name),
            None => n,
        }
    }

    fn with_item(&self, i: &WithItem) -> String {
        let mut s = self.p(i.context, P_LAMBDA);
        if let Some(v) = i.vars {
            s.push_str(" as ");
            s.push_str(&self.p(v, P_BOR));
        }
        s
    }

    fn tparams(&self, tps: &[TypeParam]) -> String {
        if tps.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = tps
            .iter()
            .map(|t| {
                let mut s = match t.kind {
                    TypeParamKind::TypeVar => String::new(),
                    TypeParamKind::TypeVarTuple => "*".into(),
                    TypeParamKind::ParamSpec => "**".into(),
                };
                s.push_str(t.name.name);
                if let Some(b) = t.bound {
                    s.push_str(": ");
                    s.push_str(&self.p(b, P_LAMBDA));
                }
                if let Some(d) = t.default {
                    s.push_str(" = ");
                    s.push_str(&self.p(d, P_LAMBDA));
                }
                s
            })
            .collect();
        format!("[{}]", parts.join(", "))
    }

    fn param_list(&self, p: &Parameters, annotations: bool) -> Vec<String> {
        let one = |x: &Param| -> String {
            let mut s = x.name.name.to_string();
            if annotations && let Some(a) = x.annotation {
                s.push_str(": ");
                s.push_str(&self.p(a, P_LAMBDA));
            }
            if let Some(d) = x.default {
                if annotations && x.annotation.is_some() {
                    s.push_str(" = ");
                } else {
                    s.push('=');
                }
                s.push_str(&self.p(d, P_LAMBDA));
            }
            s
        };
        let mut v = Vec::new();
        for x in p.posonly {
            v.push(one(x));
        }
        if !p.posonly.is_empty() {
            v.push("/".into());
        }
        for x in p.args {
            v.push(one(x));
        }
        if let Some(x) = &p.vararg {
            v.push(format!("*{}", one(x)));
        } else if p.bare_star {
            v.push("*".into());
        }
        for x in p.kwonly {
            v.push(one(x));
        }
        if let Some(x) = &p.kwarg {
            v.push(format!("**{}", one(x)));
        }
        v
    }

    fn def_header(&self, s: &Stmt, level: usize) -> String {
        let ind = "    ".repeat(level);
        let mut out = String::new();
        let (decorators, line, items, tail): (&[Ex], String, Vec<String>, String) = match &s.kind {
            StmtKind::FunctionDef {
                name,
                decorators,
                type_params,
                params,
                returns,
                is_async,
                ..
            } => {
                let head = format!(
                    "{}def {}{}(",
                    if *is_async { "async " } else { "" },
                    name.name,
                    self.tparams(type_params)
                );
                let tail = match returns {
                    Some(r) => format!(") -> {}:", self.p(r, P_LAMBDA)),
                    None => "):".into(),
                };
                (decorators, head, self.param_list(params, true), tail)
            }
            StmtKind::ClassDef {
                name,
                decorators,
                type_params,
                bases,
                ..
            } => {
                let head = format!("class {}{}", name.name, self.tparams(type_params));
                if bases.is_empty() {
                    (decorators, head, vec![], ":".into())
                } else {
                    (
                        decorators,
                        head + "(",
                        bases.iter().map(|a| self.arg(a, bases.len())).collect(),
                        "):".into(),
                    )
                }
            }
            _ => unreachable!(),
        };
        for d in decorators {
            out.push('@');
            out.push_str(&self.p(d, P_LAMBDA));
            out.push('\n');
            out.push_str(&ind);
        }
        let flat = format!("{line}{}{tail}", items.join(", "));
        if flat.len() + ind.len() <= self.ll || items.is_empty() {
            out.push_str(&flat);
        } else {
            out.push_str(&line);
            out.push('\n');
            let ind2 = "    ".repeat(level + 1);
            for it in &items {
                out.push_str(&ind2);
                out.push_str(it);
                // a trailing comma after `*args`/`**kwargs` is fine in 3.6+, but not after a bare `*`/`/`? (both fine)
                out.push_str(",\n");
            }
            out.push_str(&ind);
            out.push_str(&tail);
        }
        out
    }

    // ------------------------------------------------------------ expressions
    fn p(&self, e: &Expr, min: u8) -> String {
        let s = self.flat(e);
        if prec(e) < min { format!("({s})") } else { s }
    }

    fn arg(&self, a: &Arg, n: usize) -> String {
        match a {
            Arg::Positional(e) => {
                if n == 1
                    && let ExprKind::Generator(el, g) = e.kind
                {
                    return format!("{}{}", self.p(el, P_LAMBDA), self.comps(g));
                }
                self.p(e, P_LAMBDA)
            }
            Arg::Starred(e) => format!("*{}", self.p(e, P_BOR)),
            Arg::Keyword(n, e) => format!("{}={}", n.name, self.p(e, P_LAMBDA)),
            Arg::DoubleStarred(e) => format!("**{}", self.p(e, P_BOR)),
        }
    }

    fn elt(&self, e: &Expr) -> String {
        match e.kind {
            ExprKind::Starred(v) => format!("*{}", self.p(v, P_BOR)),
            _ => self.p(e, P_LAMBDA),
        }
    }

    fn comps(&self, gens: &[Comprehension]) -> String {
        let mut s = String::new();
        for g in gens {
            s.push_str(if g.is_async { " async for " } else { " for " });
            s.push_str(&self.target(g.target));
            s.push_str(" in ");
            s.push_str(&self.p(g.iter, P_OR));
            for c in g.ifs {
                s.push_str(" if ");
                s.push_str(&self.p(c, P_OR));
            }
        }
        s
    }

    fn target(&self, t: &Expr) -> String {
        match t.kind {
            ExprKind::Tuple {
                elts,
                parenthesized: false,
            } if !elts.is_empty() => {
                let v: Vec<String> = elts.iter().map(|e| self.elt_target(e)).collect();
                if v.len() == 1 {
                    format!("{},", v[0])
                } else {
                    v.join(", ")
                }
            }
            _ => self.elt_target(t),
        }
    }

    fn elt_target(&self, e: &Expr) -> String {
        match e.kind {
            ExprKind::Starred(v) => format!("*{}", self.p(v, P_BOR)),
            _ => self.p(e, P_BOR),
        }
    }

    fn slice_part(&self, e: &Expr) -> String {
        match e.kind {
            ExprKind::Slice(a, b, c) => {
                let f = |x: Option<Ex>| x.map(|x| self.p(x, P_IFEXP)).unwrap_or_default();
                let mut s = format!("{}:{}", f(a), f(b));
                if let Some(c) = c {
                    s.push(':');
                    s.push_str(&self.p(c, P_IFEXP));
                }
                s
            }
            ExprKind::Starred(v) => format!("*{}", self.p(v, P_BOR)),
            _ => self.p(e, P_LAMBDA),
        }
    }

    fn slice(&self, e: &Expr) -> String {
        match e.kind {
            ExprKind::Tuple {
                elts,
                parenthesized: false,
            } if !elts.is_empty() => {
                let v: Vec<String> = elts.iter().map(|e| self.slice_part(e)).collect();
                if v.len() == 1 {
                    format!("{},", v[0])
                } else {
                    v.join(", ")
                }
            }
            _ => self.slice_part(e),
        }
    }

    fn flat(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Name(n) => n.to_string(),
            ExprKind::Constant(c) => match c {
                Constant::None => "None".into(),
                Constant::True => "True".into(),
                Constant::False => "False".into(),
                Constant::Ellipsis => "...".into(),
                Constant::Int(s) | Constant::Float(s) | Constant::Imag(s) => s.to_string(),
            },
            ExprKind::Str(parts) => parts
                .iter()
                .map(|p| normalize_quotes(p.text))
                .collect::<Vec<_>>()
                .join(" "),
            ExprKind::BoolOp(op, vs) => {
                let (kw, pr) = match op {
                    BoolOp::Or => (" or ", P_OR),
                    BoolOp::And => (" and ", P_AND),
                };
                vs.iter().map(|v| self.p(v, pr + 1)).collect::<Vec<_>>().join(kw)
            }
            ExprKind::Named(t, v) => format!("({} := {})", self.p(t, P_ATOM), self.p(v, P_LAMBDA)),
            ExprKind::BinOp(l, op, r) => {
                let pr = op_prec(*op);
                if *op == Operator::Pow {
                    let simple = |x: &Expr| {
                        matches!(
                            x.kind,
                            ExprKind::Name(_) | ExprKind::Constant(_) | ExprKind::Attribute(..)
                        ) || matches!(x.kind, ExprKind::Unary(UnaryOp::USub | UnaryOp::UAdd, v) if matches!(v.kind, ExprKind::Constant(_) | ExprKind::Name(_)))
                    };
                    let sep = if simple(l) && simple(r) { "**" } else { " ** " };
                    format!("{}{sep}{}", self.p(l, P_AWAIT), self.p(r, P_UNARY))
                } else {
                    format!("{} {} {}", self.p(l, pr), op.as_str(), self.p(r, pr + 1))
                }
            }
            ExprKind::Unary(op, v) => match op {
                UnaryOp::Not => format!("not {}", self.p(v, P_NOT)),
                UnaryOp::USub => format!("-{}", self.p(v, P_UNARY)),
                UnaryOp::UAdd => format!("+{}", self.p(v, P_UNARY)),
                UnaryOp::Invert => format!("~{}", self.p(v, P_UNARY)),
            },
            ExprKind::Lambda(params, body) => {
                let ps = self.param_list(params, false);
                if ps.is_empty() {
                    format!("lambda: {}", self.p(body, P_LAMBDA))
                } else {
                    format!("lambda {}: {}", ps.join(", "), self.p(body, P_LAMBDA))
                }
            }
            ExprKind::IfExp { test, body, orelse } => format!(
                "{} if {} else {}",
                self.p(body, P_OR),
                self.p(test, P_OR),
                self.p(orelse, P_LAMBDA)
            ),
            ExprKind::Dict(items) => {
                let v: Vec<String> = items
                    .iter()
                    .map(|(k, v)| match k {
                        Some(k) => format!("{}: {}", self.p(k, P_IFEXP), self.p(v, P_LAMBDA)),
                        None => format!("**{}", self.p(v, P_BOR)),
                    })
                    .collect();
                format!("{{{}}}", v.join(", "))
            }
            ExprKind::Set(v) => format!("{{{}}}", v.iter().map(|x| self.elt(x)).collect::<Vec<_>>().join(", ")),
            ExprKind::List(v) => format!("[{}]", v.iter().map(|x| self.elt(x)).collect::<Vec<_>>().join(", ")),
            ExprKind::Tuple { elts, parenthesized } => {
                let v: Vec<String> = elts.iter().map(|x| self.elt(x)).collect();
                let inner = if v.len() == 1 {
                    format!("{},", v[0])
                } else {
                    v.join(", ")
                };
                if *parenthesized || elts.is_empty() {
                    format!("({inner})")
                } else {
                    inner
                }
            }
            ExprKind::ListComp(el, g) => format!("[{}{}]", self.p(el, P_LAMBDA), self.comps(g)),
            ExprKind::SetComp(el, g) => format!("{{{}{}}}", self.p(el, P_LAMBDA), self.comps(g)),
            ExprKind::Generator(el, g) => format!("({}{})", self.p(el, P_LAMBDA), self.comps(g)),
            ExprKind::DictComp(k, v, g) => {
                format!("{{{}: {}{}}}", self.p(k, P_IFEXP), self.p(v, P_LAMBDA), self.comps(g))
            }
            ExprKind::Await(v) => format!("await {}", self.p(v, P_PRIMARY)),
            ExprKind::Yield(v) => match v {
                Some(v) => format!("yield {}", self.p(v, P_TUPLE)),
                None => "yield".into(),
            },
            ExprKind::YieldFrom(v) => format!("yield from {}", self.p(v, P_LAMBDA)),
            ExprKind::Compare(l, ops) => {
                let mut s = self.p(l, P_BOR);
                for (op, r) in *ops {
                    s.push(' ');
                    s.push_str(op.as_str());
                    s.push(' ');
                    s.push_str(&self.p(r, P_BOR));
                }
                s
            }
            ExprKind::Call(f, args) => format!(
                "{}({})",
                self.primary(f),
                args.iter()
                    .map(|a| self.arg(a, args.len()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ExprKind::Attribute(v, a) => format!("{}.{}", self.primary(v), a.name),
            ExprKind::Subscript(v, s) => format!("{}[{}]", self.primary(v), self.slice(s)),
            ExprKind::Starred(v) => format!("*{}", self.p(v, P_BOR)),
            ExprKind::Slice(..) => self.slice_part(e),
        }
    }

    fn primary(&self, v: &Expr) -> String {
        if matches!(v.kind, ExprKind::Constant(Constant::Int(_))) {
            return format!("({})", self.flat(v));
        }
        self.p(v, P_PRIMARY)
    }

    /// Formats `e` (in a context requiring precedence `min`) so that it fits in
    /// the line given `prefix` columns before it and `suffix` after. Falls back
    /// to exploding the outermost bracket.
    fn split(&self, e: &Expr, level: usize, prefix: usize, suffix: usize, min: u8) -> String {
        let flat = self.p(e, min);
        let width = 4 * level + prefix + flat.chars().count() + suffix;
        if width <= self.ll
            || line_has_newline(&flat) && !matches!(e.kind, ExprKind::Call(..) | ExprKind::List(_) | ExprKind::Dict(_))
        {
            return flat;
        }
        if prec(e) < min {
            return flat;
        }
        let ind = "    ".repeat(level + 1);
        let close_ind = "    ".repeat(level);
        let explode = |open: &str, items: Vec<String>, close: &str, trailing: bool| -> String {
            let mut s = String::from(open);
            s.push('\n');
            let n = items.len();
            for (i, it) in items.into_iter().enumerate() {
                s.push_str(&ind);
                s.push_str(&it);
                if i + 1 < n || trailing {
                    s.push(',');
                }
                s.push('\n');
            }
            s.push_str(&close_ind);
            s.push_str(close);
            s
        };
        let sub = |x: &Expr, min: u8| self.split(x, level + 1, 0, 1, min);
        match &e.kind {
            ExprKind::Call(f, args) if !args.is_empty() => {
                let head = format!("{}(", self.primary(f));
                let gen_only = args.len() == 1
                    && matches!(args[0], Arg::Positional(x) if matches!(x.kind, ExprKind::Generator(..)));
                let items: Vec<String> = args
                    .iter()
                    .map(|a| match a {
                        Arg::Positional(x) if !gen_only => sub(x, P_LAMBDA),
                        Arg::Keyword(n, x) => {
                            format!("{}={}", n.name, self.split(x, level + 1, n.name.len() + 1, 1, P_LAMBDA))
                        }
                        _ => self.arg(a, args.len()),
                    })
                    .collect();
                explode(&head, items, ")", !gen_only)
            }
            ExprKind::List(v) if !v.is_empty() => explode(
                "[",
                v.iter()
                    .map(|x| {
                        if matches!(x.kind, ExprKind::Starred(_)) {
                            self.elt(x)
                        } else {
                            sub(x, P_LAMBDA)
                        }
                    })
                    .collect(),
                "]",
                true,
            ),
            ExprKind::Set(v) => explode(
                "{",
                v.iter()
                    .map(|x| {
                        if matches!(x.kind, ExprKind::Starred(_)) {
                            self.elt(x)
                        } else {
                            sub(x, P_LAMBDA)
                        }
                    })
                    .collect(),
                "}",
                true,
            ),
            ExprKind::Tuple { elts, .. } if !elts.is_empty() => explode(
                "(",
                elts.iter()
                    .map(|x| {
                        if matches!(x.kind, ExprKind::Starred(_)) {
                            self.elt(x)
                        } else {
                            sub(x, P_LAMBDA)
                        }
                    })
                    .collect(),
                ")",
                true,
            ),
            ExprKind::Dict(items) if !items.is_empty() => explode(
                "{",
                items
                    .iter()
                    .map(|(k, v)| match k {
                        Some(k) => {
                            let ks = self.p(k, P_IFEXP);
                            let vs = self.split(v, level + 1, ks.len() + 2, 1, P_LAMBDA);
                            format!("{ks}: {vs}")
                        }
                        None => format!("**{}", self.p(v, P_BOR)),
                    })
                    .collect(),
                "}",
                true,
            ),
            ExprKind::Subscript(v, s) if !matches!(s.kind, ExprKind::Slice(..)) => {
                let head = format!("{}[", self.primary(v));
                match s.kind {
                    ExprKind::Tuple {
                        elts,
                        parenthesized: false,
                    } if !elts.is_empty() => {
                        explode(&head, elts.iter().map(|x| self.slice_part(x)).collect(), "]", true)
                    }
                    _ => explode(&head, vec![sub(s, P_LAMBDA)], "]", false),
                }
            }
            ExprKind::ListComp(el, g) | ExprKind::SetComp(el, g) | ExprKind::Generator(el, g) => {
                let (o, c) = match e.kind {
                    ExprKind::ListComp(..) => ("[", "]"),
                    ExprKind::SetComp(..) => ("{", "}"),
                    _ => ("(", ")"),
                };
                let mut items = vec![sub(el, P_LAMBDA)];
                for g in *g {
                    items.push(format!(
                        "{}for {} in {}",
                        if g.is_async { "async " } else { "" },
                        self.target(g.target),
                        self.p(g.iter, P_OR)
                    ));
                    for i in g.ifs {
                        items.push(format!("if {}", self.p(i, P_OR)));
                    }
                }
                let mut s = String::from(o);
                s.push('\n');
                for it in items {
                    s.push_str(&ind);
                    s.push_str(&it);
                    s.push('\n');
                }
                s.push_str(&close_ind);
                s.push_str(c);
                s
            }
            ExprKind::BoolOp(..) | ExprKind::BinOp(..) if min <= P_LAMBDA => {
                // wrap in parentheses and break before operators
                let mut operands = Vec::new();
                let mut ops = Vec::new();
                flatten_ops(self, e, &mut operands, &mut ops);
                let mut s = String::from("(\n");
                for (i, o) in operands.iter().enumerate() {
                    s.push_str(&ind);
                    if i > 0 {
                        s.push_str(&ops[i - 1]);
                        s.push(' ');
                    }
                    s.push_str(o);
                    s.push('\n');
                }
                s.push_str(&close_ind);
                s.push(')');
                s
            }
            _ => flat,
        }
    }

    fn pattern(&self, p: &Pattern, top: bool) -> String {
        let _ = top;
        match p {
            Pattern::Value(e) | Pattern::Singleton(e) => self.p(e, P_BOR),
            Pattern::Sequence(ps, _) => format!(
                "[{}]",
                ps.iter().map(|p| self.pattern(p, false)).collect::<Vec<_>>().join(", ")
            ),
            Pattern::Mapping {
                keys, patterns, rest, ..
            } => {
                let mut v: Vec<String> = keys
                    .iter()
                    .zip(patterns.iter())
                    .map(|(k, p)| format!("{}: {}", self.p(k, P_BOR), self.pattern(p, false)))
                    .collect();
                if let Some(r) = rest {
                    v.push(format!("**{}", r.name));
                }
                format!("{{{}}}", v.join(", "))
            }
            Pattern::Class { cls, patterns, kwd, .. } => {
                let mut v: Vec<String> = patterns.iter().map(|p| self.pattern(p, false)).collect();
                for (n, p) in *kwd {
                    v.push(format!("{}={}", n.name, self.pattern(p, false)));
                }
                format!("{}({})", self.p(cls, P_PRIMARY), v.join(", "))
            }
            Pattern::Star(n, _) => format!("*{}", n.map(|n| n.name).unwrap_or("_")),
            Pattern::As(inner, n, _) => match inner {
                None => n.map(|n| n.name).unwrap_or("_").to_string(),
                Some(i) => format!("{} as {}", self.pattern(i, false), n.map(|n| n.name).unwrap_or("_")),
            },
            Pattern::Or(ps, _) => ps
                .iter()
                .map(|p| {
                    let s = self.pattern(p, false);
                    if matches!(p, Pattern::As(Some(_), ..)) {
                        format!("({s})")
                    } else {
                        s
                    }
                })
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }
}

fn flatten_ops(f: &Formatter, e: &Expr, operands: &mut Vec<String>, ops: &mut Vec<String>) {
    match &e.kind {
        ExprKind::BoolOp(op, vs) => {
            let (kw, pr) = match op {
                BoolOp::Or => ("or", P_OR),
                BoolOp::And => ("and", P_AND),
            };
            for (i, v) in vs.iter().enumerate() {
                if i > 0 {
                    ops.push(kw.to_string());
                }
                operands.push(f.p(v, pr + 1));
            }
        }
        ExprKind::BinOp(l, op, r) if *op != Operator::Pow => {
            let pr = op_prec(*op);
            if matches!(&l.kind, ExprKind::BinOp(_, lop, _) if op_prec(*lop) == pr && *lop != Operator::Pow) {
                flatten_ops(f, l, operands, ops);
            } else {
                operands.push(f.p(l, pr));
            }
            ops.push(op.as_str().to_string());
            operands.push(f.p(r, pr + 1));
        }
        _ => operands.push(f.flat(e)),
    }
}
