//! Span-free textual dump of the AST, used to compare trees for equality
//! (e.g. before/after formatting). Redundant parentheses and string quoting
//! style are ignored, matching what Python's own `ast` considers equal.

use crate::ast::*;
use std::fmt::Write;

pub fn dump_module(m: &Module) -> String {
    let mut d = Dumper { out: String::new() };
    d.body(m.body);
    d.out
}

pub fn dump_expr(e: &Expr) -> String {
    let mut d = Dumper { out: String::new() };
    d.expr(e);
    d.out
}

struct Dumper {
    out: String,
}

/// Normalizes a string token: lowercase prefix (minus `u`), body without quotes.
pub fn normalize_str_token(text: &str) -> (String, &str, bool) {
    let pre_len = text.find(['\'', '"']).unwrap_or(0);
    let prefix: String = text[..pre_len]
        .to_ascii_lowercase()
        .chars()
        .filter(|&c| c != 'u')
        .collect();
    let rest = &text[pre_len..];
    let triple = rest.len() >= 6 && (rest.starts_with("\"\"\"") || rest.starts_with("'''"));
    let q = if triple { 3 } else { 1 };
    let body = if rest.len() >= 2 * q {
        &rest[q..rest.len() - q]
    } else {
        rest
    };
    (prefix, body, triple)
}

impl Dumper {
    fn w(&mut self, s: &str) {
        self.out.push_str(s);
    }
    fn body(&mut self, b: &[Stmt]) {
        self.w("[");
        for s in b {
            self.stmt(s);
            self.w(";");
        }
        self.w("]");
    }
    fn exprs(&mut self, es: &[Ex]) {
        self.w("[");
        for e in es {
            self.expr(e);
            self.w(",");
        }
        self.w("]");
    }
    fn opt(&mut self, e: Option<Ex>) {
        match e {
            Some(e) => self.expr(e),
            None => self.w("_"),
        }
    }
    fn params(&mut self, p: &Parameters) {
        let one = |d: &mut Dumper, tag: &str, ps: &[Param]| {
            d.w(tag);
            d.w("(");
            for p in ps {
                d.w(p.name.name);
                d.w(":");
                d.opt(p.annotation);
                d.w("=");
                d.opt(p.default);
                d.w(",");
            }
            d.w(")");
        };
        one(self, "po", p.posonly);
        one(self, "a", p.args);
        one(self, "va", p.vararg.as_slice());
        one(self, "kw", p.kwonly);
        one(self, "k", p.kwarg.as_slice());
    }
    fn args(&mut self, a: &[Arg]) {
        self.w("(");
        for a in a {
            match a {
                Arg::Positional(e) => self.expr(e),
                Arg::Starred(e) => {
                    self.w("*");
                    self.expr(e)
                }
                Arg::Keyword(n, e) => {
                    self.w(n.name);
                    self.w("=");
                    self.expr(e)
                }
                Arg::DoubleStarred(e) => {
                    self.w("**");
                    self.expr(e)
                }
            }
            self.w(",");
        }
        self.w(")");
    }
    fn tparams(&mut self, t: &[TypeParam]) {
        for t in t {
            let _ = write!(self.out, "tp({:?},{},", t.kind, t.name.name);
            self.opt(t.bound);
            self.opt(t.default);
            self.w(")");
        }
    }
    fn comps(&mut self, c: &[Comprehension]) {
        for c in c {
            self.w(if c.is_async { "afor(" } else { "for(" });
            self.expr(c.target);
            self.expr(c.iter);
            self.exprs(c.ifs);
            self.w(")");
        }
    }
    fn fparts(&mut self, src_parts: &[FPart], text: &str, base: u32) {
        for p in src_parts {
            match p {
                FPart::Literal(s) => {
                    let s = &text[(s.start - base) as usize..(s.end - base) as usize];
                    let _ = write!(self.out, "L{s:?}");
                }
                FPart::Field {
                    expr,
                    debug,
                    conversion,
                    spec,
                } => {
                    self.w("{");
                    self.expr(expr);
                    let _ = write!(self.out, "{debug}{conversion:?}");
                    if let Some(sp) = spec {
                        self.w(":");
                        self.fparts(sp, text, base);
                    }
                    self.w("}");
                }
            }
        }
    }
    fn pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Value(e) => {
                self.w("V");
                self.expr(e)
            }
            Pattern::Singleton(e) => {
                self.w("S");
                self.expr(e)
            }
            Pattern::Sequence(ps, _) => {
                self.w("Seq[");
                for p in *ps {
                    self.pattern(p);
                    self.w(",");
                }
                self.w("]");
            }
            Pattern::Mapping {
                keys, patterns, rest, ..
            } => {
                self.w("Map[");
                for (k, p) in keys.iter().zip(patterns.iter()) {
                    self.expr(k);
                    self.w(":");
                    self.pattern(p);
                }
                let _ = write!(self.out, "**{:?}]", rest.map(|r| r.name));
            }
            Pattern::Class { cls, patterns, kwd, .. } => {
                self.w("Cls(");
                self.expr(cls);
                for p in *patterns {
                    self.pattern(p);
                    self.w(",");
                }
                for (n, p) in *kwd {
                    self.w(n.name);
                    self.w("=");
                    self.pattern(p);
                }
                self.w(")");
            }
            Pattern::Star(n, _) => {
                let _ = write!(self.out, "*{:?}", n.map(|n| n.name));
            }
            Pattern::As(p, n, _) => {
                self.w("As(");
                if let Some(p) = p {
                    self.pattern(p);
                }
                let _ = write!(self.out, "{:?})", n.map(|n| n.name));
            }
            Pattern::Or(ps, _) => {
                self.w("Or[");
                for p in *ps {
                    self.pattern(p);
                    self.w("|");
                }
                self.w("]");
            }
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::FunctionDef {
                name,
                decorators,
                type_params,
                params,
                returns,
                body,
                is_async,
            } => {
                let _ = write!(self.out, "Def({is_async},{},", name.name);
                self.exprs(decorators);
                self.tparams(type_params);
                self.params(params);
                self.opt(*returns);
                self.body(body);
                self.w(")");
            }
            StmtKind::ClassDef {
                name,
                decorators,
                type_params,
                bases,
                body,
                ..
            } => {
                let _ = write!(self.out, "Class({},", name.name);
                self.exprs(decorators);
                self.tparams(type_params);
                self.args(bases);
                self.body(body);
                self.w(")");
            }
            StmtKind::Return(v) => {
                self.w("Return(");
                self.opt(*v);
                self.w(")");
            }
            StmtKind::Delete(t) => {
                self.w("Del");
                self.exprs(t);
            }
            StmtKind::Assign { targets, value } => {
                self.w("Assign(");
                self.exprs(targets);
                self.expr(value);
                self.w(")");
            }
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => {
                self.w("Type(");
                self.expr(name);
                self.tparams(type_params);
                self.expr(value);
                self.w(")");
            }
            StmtKind::AugAssign { target, op, value } => {
                let _ = write!(self.out, "Aug({op:?},");
                self.expr(target);
                self.expr(value);
                self.w(")");
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
            } => {
                self.w("Ann(");
                self.expr(target);
                self.expr(annotation);
                self.opt(*value);
                self.w(")");
            }
            StmtKind::For {
                is_async,
                target,
                iter,
                body,
                orelse,
            } => {
                let _ = write!(self.out, "For({is_async},");
                self.expr(target);
                self.expr(iter);
                self.body(body);
                self.body(orelse);
                self.w(")");
            }
            StmtKind::While { test, body, orelse } => {
                self.w("While(");
                self.expr(test);
                self.body(body);
                self.body(orelse);
                self.w(")");
            }
            StmtKind::If { test, body, orelse, .. } => {
                self.w("If(");
                self.expr(test);
                self.body(body);
                self.body(orelse);
                self.w(")");
            }
            StmtKind::With { is_async, items, body } => {
                let _ = write!(self.out, "With({is_async},");
                for i in *items {
                    self.expr(i.context);
                    self.w(" as ");
                    self.opt(i.vars);
                    self.w(",");
                }
                self.body(body);
                self.w(")");
            }
            StmtKind::Match { subject, cases } => {
                self.w("Match(");
                self.expr(subject);
                for c in *cases {
                    self.w("case(");
                    self.pattern(&c.pattern);
                    self.opt(c.guard);
                    self.body(c.body);
                    self.w(")");
                }
                self.w(")");
            }
            StmtKind::Raise { exc, cause } => {
                self.w("Raise(");
                self.opt(*exc);
                self.opt(*cause);
                self.w(")");
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
                star,
            } => {
                let _ = write!(self.out, "Try({star},");
                self.body(body);
                for h in *handlers {
                    self.w("except(");
                    self.opt(h.typ);
                    let _ = write!(self.out, "{:?}", h.name.map(|n| n.name));
                    self.body(h.body);
                    self.w(")");
                }
                self.body(orelse);
                self.body(finalbody);
                self.w(")");
            }
            StmtKind::Assert { test, msg } => {
                self.w("Assert(");
                self.expr(test);
                self.opt(*msg);
                self.w(")");
            }
            StmtKind::Import(names) => {
                self.w("Import(");
                for a in *names {
                    let _ = write!(
                        self.out,
                        "{} as {:?},",
                        a.name.name.replace(' ', ""),
                        a.asname.map(|n| n.name)
                    );
                }
                self.w(")");
            }
            StmtKind::ImportFrom { module, level, names } => {
                let _ = write!(self.out, "From({level},{:?},", module.map(|m| m.name.replace(' ', "")));
                for a in *names {
                    let _ = write!(self.out, "{} as {:?},", a.name.name, a.asname.map(|n| n.name));
                }
                self.w(")");
            }
            StmtKind::Global(n) | StmtKind::Nonlocal(n) => {
                let tag = if matches!(s.kind, StmtKind::Global(_)) {
                    "Global"
                } else {
                    "Nonlocal"
                };
                let _ = write!(self.out, "{tag}({:?})", n.iter().map(|n| n.name).collect::<Vec<_>>());
            }
            StmtKind::Expr(e) => {
                self.w("Expr(");
                self.expr(e);
                self.w(")");
            }
            StmtKind::Pass => self.w("Pass"),
            StmtKind::Break => self.w("Break"),
            StmtKind::Continue => self.w("Continue"),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::BoolOp(op, vs) => {
                let _ = write!(self.out, "{op:?}");
                self.exprs(vs);
            }
            ExprKind::Named(t, v) => {
                self.w("Walrus(");
                self.expr(t);
                self.expr(v);
                self.w(")");
            }
            ExprKind::BinOp(l, op, r) => {
                let _ = write!(self.out, "Bin({op:?},");
                self.expr(l);
                self.w(",");
                self.expr(r);
                self.w(")");
            }
            ExprKind::Unary(op, v) => {
                let _ = write!(self.out, "Un({op:?},");
                self.expr(v);
                self.w(")");
            }
            ExprKind::Lambda(p, b) => {
                self.w("Lambda(");
                self.params(p);
                self.expr(b);
                self.w(")");
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.w("IfExp(");
                self.expr(test);
                self.expr(body);
                self.expr(orelse);
                self.w(")");
            }
            ExprKind::Dict(items) => {
                self.w("Dict{");
                for (k, v) in *items {
                    self.opt(*k);
                    self.w(":");
                    self.expr(v);
                    self.w(",");
                }
                self.w("}");
            }
            ExprKind::Set(v) => {
                self.w("Set");
                self.exprs(v);
            }
            ExprKind::ListComp(el, c) | ExprKind::SetComp(el, c) | ExprKind::Generator(el, c) => {
                let tag = match e.kind {
                    ExprKind::ListComp(..) => "ListComp(",
                    ExprKind::SetComp(..) => "SetComp(",
                    _ => "Gen(",
                };
                self.w(tag);
                self.expr(el);
                self.comps(c);
                self.w(")");
            }
            ExprKind::DictComp(k, v, c) => {
                self.w("DictComp(");
                self.expr(k);
                self.expr(v);
                self.comps(c);
                self.w(")");
            }
            ExprKind::Await(v) => {
                self.w("Await(");
                self.expr(v);
                self.w(")");
            }
            ExprKind::Yield(v) => {
                self.w("Yield(");
                self.opt(*v);
                self.w(")");
            }
            ExprKind::YieldFrom(v) => {
                self.w("YieldFrom(");
                self.expr(v);
                self.w(")");
            }
            ExprKind::Compare(l, ops) => {
                self.w("Cmp(");
                self.expr(l);
                for (op, r) in *ops {
                    let _ = write!(self.out, " {op:?} ");
                    self.expr(r);
                }
                self.w(")");
            }
            ExprKind::Call(f, a) => {
                self.w("Call(");
                self.expr(f);
                self.args(a);
                self.w(")");
            }
            ExprKind::Attribute(v, a) => {
                self.expr(v);
                self.w(".");
                self.w(a.name);
            }
            ExprKind::Subscript(v, s) => {
                self.expr(v);
                self.w("[");
                self.expr(s);
                self.w("]");
            }
            ExprKind::Starred(v) => {
                self.w("*");
                self.expr(v);
            }
            ExprKind::Name(n) => self.w(n),
            ExprKind::List(v) => {
                self.w("List");
                self.exprs(v);
            }
            ExprKind::Tuple { elts, .. } => {
                self.w("Tuple");
                self.exprs(elts);
            }
            ExprKind::Slice(a, b, c) => {
                self.w("Slice(");
                self.opt(*a);
                self.opt(*b);
                self.opt(*c);
                self.w(")");
            }
            ExprKind::Constant(c) => {
                let _ = write!(self.out, "{c:?}");
            }
            ExprKind::Str(parts) => {
                self.w("Str(");
                for p in *parts {
                    if p.kind == StrKind::FString {
                        self.w("f[");
                        self.fparts(p.fparts, p.text, p.span.start);
                        self.w("]");
                    } else {
                        let (prefix, body, triple) = normalize_str_token(p.text);
                        let _ = write!(self.out, "{prefix}{triple}{body:?}");
                    }
                    self.w(" ");
                }
                self.w(")");
            }
        }
    }
}
