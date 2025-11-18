//! AST walker + semantic model (scopes, bindings, import usage) and the
//! AST-based lint rules.

use crate::ast::*;
use crate::data::is_builtin;
use crate::dump::dump_expr;
use crate::rules::{Diagnostic, Fix, Rule, RuleSet};
use crate::span::Span;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeKind {
    Module,
    Class,
    Function,
    Comprehension,
}

struct Scope<'a> {
    kind: ScopeKind,
    parent: Option<usize>,
    names: HashMap<&'a str, usize>,
    globals: Vec<&'a str>,
    uses_locals: bool,
    star_import: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BKind {
    Import,
    FutureImport,
    Assignment,
    Plain,
    Argument,
    FunctionDef { overload: bool },
    ClassDef,
    ExceptName,
    Global,
}

impl BKind {
    fn is_definition(self) -> bool {
        matches!(self, BKind::Import | BKind::FunctionDef { .. } | BKind::ClassDef)
    }
}

struct Binding<'a> {
    name: &'a str,
    kind: BKind,
    span: Span,
    used: bool,
    fork: Vec<(u32, u32)>,
    /// for imports: (statement, alias index, qualified name)
    import: Option<(&'a Stmt<'a>, usize, String)>,
    /// earlier submodule import of the same root that this binding shadows
    shadows: Option<usize>,
}

enum Deferred<'a> {
    Function {
        scope: usize,
        params: &'a Parameters<'a>,
        body: Body<'a>,
    },
    Lambda {
        scope: usize,
        params: &'a Parameters<'a>,
        body: Ex<'a>,
    },
    Expr {
        scope: usize,
        expr: Ex<'a>,
    },
    /// report unused bindings of a function scope once nested functions ran
    Finish {
        scope: usize,
    },
}

pub struct Checker<'a> {
    src: &'a str,
    rules: &'a RuleSet,
    pub diags: Vec<Diagnostic>,
    scopes: Vec<Scope<'a>>,
    bindings: Vec<Binding<'a>>,
    scope: usize,
    deferred: Vec<Deferred<'a>>,
    fork: Vec<(u32, u32)>,
    next_fork: u32,
    loop_depth: u32,
    in_function: bool,
    catches_name_error: u32,
    future_annotations: bool,
    is_init: bool,
    all_names: Vec<(&'a str, Span)>,
    sole_in_block: HashMap<u32, bool>,
    seen_non_import: bool,
    in_def_body: std::cell::Cell<bool>,
}

const AMBIGUOUS: [&str; 3] = ["l", "O", "I"];

impl<'a> Checker<'a> {
    pub fn new(src: &'a str, rules: &'a RuleSet, is_init: bool) -> Self {
        Checker {
            src,
            rules,
            diags: Vec::new(),
            scopes: vec![Scope {
                kind: ScopeKind::Module,
                parent: None,
                names: HashMap::new(),
                globals: vec![],
                uses_locals: false,
                star_import: false,
            }],
            bindings: Vec::new(),
            scope: 0,
            deferred: Vec::new(),
            fork: Vec::new(),
            next_fork: 0,
            loop_depth: 0,
            in_function: false,
            catches_name_error: 0,
            future_annotations: false,
            is_init,
            all_names: Vec::new(),
            sole_in_block: HashMap::new(),
            seen_non_import: false,
            in_def_body: std::cell::Cell::new(false),
        }
    }

    fn report(&mut self, rule: Rule, span: Span, msg: impl Into<String>) -> Option<&mut Diagnostic> {
        if !self.rules.enabled(rule) {
            return None;
        }
        self.diags.push(Diagnostic::new(rule, span, msg));
        self.diags.last_mut()
    }

    fn text(&self, s: Span) -> &'a str {
        &self.src[s.range()]
    }

    pub fn check_module(&mut self, m: &'a Module<'a>) {
        self.module_header(m.body);
        self.body(m.body);
        // process deferred function bodies (FIFO, they may add more)
        let mut i = 0;
        while i < self.deferred.len() {
            let d = std::mem::replace(
                &mut self.deferred[i],
                Deferred::Expr {
                    scope: 0,
                    expr: &NONE_EXPR,
                },
            );
            self.run_deferred(d);
            i += 1;
        }
        // __all__ handling
        let all = std::mem::take(&mut self.all_names);
        for (n, span) in all {
            if let Some(&b) = self.scopes[0].names.get(n) {
                self.mark_used(b);
            } else if !self.scopes[0].star_import && !is_builtin(n) {
                self.report(
                    Rule::UndefinedExport,
                    span,
                    format!("Undefined name `{n}` in `__all__`"),
                );
            }
        }
        self.finish_scope(0);
        self.diags.sort_by_key(|d| (d.span.start, d.rule));
    }

    fn module_header(&mut self, body: Body<'a>) {
        for s in body {
            if let StmtKind::ImportFrom {
                module: Some(m), names, ..
            } = &s.kind
                && m.name == "__future__"
                && names.iter().any(|a| a.name.name == "annotations")
            {
                self.future_annotations = true;
            }
        }
    }

    fn run_deferred(&mut self, d: Deferred<'a>) {
        let saved = (
            self.scope,
            self.loop_depth,
            self.in_function,
            std::mem::take(&mut self.fork),
            self.catches_name_error,
        );
        self.loop_depth = 0;
        self.catches_name_error = 0;
        match d {
            Deferred::Function { scope, params, body } => {
                self.in_function = true;
                let s = self.push_scope(ScopeKind::Function, scope);
                self.bind_params(params);
                // one-line `def f(): ...` is E704 in pycodestyle, not E701
                self.in_def_body.set(true);
                self.body(body);
                self.deferred.push(Deferred::Finish { scope: s });
            }
            Deferred::Lambda { scope, params, body } => {
                self.in_function = true;
                let s = self.push_scope(ScopeKind::Function, scope);
                self.bind_params(params);
                self.expr(body);
                self.deferred.push(Deferred::Finish { scope: s });
            }
            Deferred::Expr { scope, expr } => {
                self.scope = scope;
                self.expr(expr);
            }
            Deferred::Finish { scope } => self.finish_scope(scope),
        }
        self.scope = saved.0;
        self.loop_depth = saved.1;
        self.in_function = saved.2;
        self.fork = saved.3;
        self.catches_name_error = saved.4;
    }

    fn push_scope(&mut self, kind: ScopeKind, parent: usize) -> usize {
        self.scopes.push(Scope {
            kind,
            parent: Some(parent),
            names: HashMap::new(),
            globals: vec![],
            uses_locals: false,
            star_import: false,
        });
        self.scope = self.scopes.len() - 1;
        self.scope
    }

    fn bind_params(&mut self, params: &'a Parameters<'a>) {
        for p in params.all() {
            if AMBIGUOUS.contains(&p.name.name) {
                self.report(
                    Rule::AmbiguousVariableName,
                    p.name.span,
                    format!("Ambiguous variable name: `{}`", p.name.name),
                );
            }
            self.add_binding(p.name.name, p.name.span, BKind::Argument, None);
        }
    }

    /// Reports unused imports / variables at the end of a scope.
    fn finish_scope(&mut self, s: usize) {
        let mut names: Vec<usize> = self.scopes[s].names.values().copied().collect();
        names.sort();
        let kind = self.scopes[s].kind;
        let mut unused_imports: Vec<usize> = Vec::new();
        for b in names {
            let bd = &self.bindings[b];
            if bd.used {
                continue;
            }
            match bd.kind {
                // imports in class bodies are public members (as in pyflakes)
                BKind::Import if kind == ScopeKind::Class => {}
                BKind::Import => {
                    let mut cur = Some(b);
                    while let Some(c) = cur {
                        if !self.bindings[c].used {
                            unused_imports.push(c);
                        }
                        cur = self.bindings[c].shadows;
                    }
                }
                BKind::Assignment if kind == ScopeKind::Function => {
                    let sc = &self.scopes[s];
                    if !sc.uses_locals
                        && !sc.globals.contains(&bd.name)
                        && !matches!(
                            bd.name,
                            "_" | "__tracebackhide__" | "__traceback_info__" | "__traceback_supplement__"
                        )
                    {
                        let (n, sp) = (bd.name, bd.span);
                        self.report(
                            Rule::UnusedVariable,
                            sp,
                            format!("Local variable `{n}` is assigned to but never used"),
                        );
                    }
                }
                _ => {}
            }
        }
        if !unused_imports.is_empty() && self.rules.enabled(Rule::UnusedImport) {
            self.report_unused_imports(unused_imports);
        }
    }

    fn report_unused_imports(&mut self, mut ids: Vec<usize>) {
        ids.sort_by_key(|&b| self.bindings[b].span.start);
        // group by statement
        let mut by_stmt: Vec<(&'a Stmt<'a>, Vec<usize>)> = Vec::new();
        for &b in &ids {
            let (stmt, idx, _) = self.bindings[b]
                .import
                .as_ref()
                .map(|(s, i, q)| (*s, *i, q.clone()))
                .unwrap();
            match by_stmt.iter_mut().find(|(s, _)| std::ptr::eq(*s, stmt)) {
                Some((_, v)) => v.push(idx),
                None => by_stmt.push((stmt, vec![idx])),
            }
        }
        for &b in &ids {
            let bd = &self.bindings[b];
            let (stmt, _, q) = bd.import.as_ref().unwrap();
            let span = bd.span;
            let msg = format!("`{q}` imported but unused");
            let fix = if self.is_init {
                None
            } else {
                self.import_fix(stmt, &by_stmt.iter().find(|(s, _)| std::ptr::eq(*s, *stmt)).unwrap().1)
            };
            if let Some(d) = self.report(Rule::UnusedImport, span, msg)
                && let Some(f) = fix
            {
                d.fix = Some(f);
            }
        }
    }

    fn import_fix(&self, stmt: &'a Stmt<'a>, unused: &[usize]) -> Option<Fix> {
        let names = match &stmt.kind {
            StmtKind::Import(n) => *n,
            StmtKind::ImportFrom { names, .. } => *names,
            _ => return None,
        };
        let keep: Vec<&Alias> = names
            .iter()
            .enumerate()
            .filter(|(i, _)| !unused.contains(i))
            .map(|(_, a)| a)
            .collect();
        if keep.is_empty() {
            // delete whole lines if the statement is alone on them
            let bytes = self.src.as_bytes();
            let mut ls = stmt.span.start as usize;
            while ls > 0 && matches!(bytes[ls - 1], b' ' | b'\t') {
                ls -= 1;
            }
            if ls > 0 && bytes[ls - 1] != b'\n' {
                return None;
            }
            let mut le = stmt.span.end as usize;
            while le < bytes.len() && matches!(bytes[le], b' ' | b'\t') {
                le += 1;
            }
            if le < bytes.len() && bytes[le] == b'#' {
                while le < bytes.len() && !matches!(bytes[le], b'\n' | b'\r') {
                    le += 1;
                }
            }
            if le < bytes.len() && bytes[le] == b'\r' {
                le += 1;
            }
            if le < bytes.len() && bytes[le] != b'\n' {
                return None;
            }
            if self.sole_in_block.get(&stmt.span.start).copied().unwrap_or(false) {
                return Some(Fix::replace(stmt.span, "pass"));
            }
            return Some(Fix::delete(Span::new(ls as u32, (le + 1).min(bytes.len()) as u32)));
        }
        let parts: Vec<&str> = keep.iter().map(|a| self.text(a.span)).collect();
        let new = match &stmt.kind {
            StmtKind::Import(_) => format!("import {}", parts.join(", ")),
            StmtKind::ImportFrom { module, level, .. } => {
                format!(
                    "from {}{} import {}",
                    ".".repeat(*level as usize),
                    module.map(|m| m.name).unwrap_or(""),
                    parts.join(", ")
                )
            }
            _ => unreachable!(),
        };
        Some(Fix::replace(stmt.span, new))
    }

    fn mark_used(&mut self, b: usize) {
        let mut cur = Some(b);
        while let Some(c) = cur {
            self.bindings[c].used = true;
            cur = self.bindings[c].shadows;
        }
    }

    fn different_forks(a: &[(u32, u32)], b: &[(u32, u32)]) -> bool {
        for (x, y) in a.iter().zip(b.iter()) {
            if x.0 != y.0 {
                return false;
            }
            if x.1 != y.1 {
                return true;
            }
        }
        false
    }

    fn add_binding(&mut self, name: &'a str, span: Span, kind: BKind, import: Option<(&'a Stmt<'a>, usize, String)>) {
        let mut target = self.scope;
        if self.scopes[self.scope].globals.contains(&name) {
            target = 0;
        }
        let mut shadows = None;
        if let Some(&old) = self.scopes[target].names.get(name) {
            let ob = &self.bindings[old];
            let is_sub = |imp: &Option<(&'a Stmt<'a>, usize, String)>| {
                imp.as_ref().is_some_and(|(st, _, q)| {
                    matches!(st.kind, StmtKind::Import(_)) && q.contains('.') && !q.contains(" as ")
                })
            };
            let submodule_pair =
                kind == BKind::Import && ob.kind == BKind::Import && (is_sub(&import) || is_sub(&ob.import));
            if submodule_pair {
                shadows = Some(old);
            } else if !ob.used
                && ob.kind.is_definition()
                && target == self.scope
                && !Self::different_forks(&self.fork, &ob.fork)
                && (name != "_" || ob.kind == BKind::Import)
                && !matches!(ob.kind, BKind::FunctionDef { overload: true })
            {
                let line = crate::span::LineIndex::new(self.src)
                    .location(self.src, ob.span.start)
                    .0;
                self.report(
                    Rule::RedefinedWhileUnused,
                    span,
                    format!("Redefinition of unused `{name}` from line {line}"),
                );
            }
            if kind == BKind::Global {
                return;
            }
        }
        let mut used = kind == BKind::FutureImport || target != self.scope && kind != BKind::Import;
        // like pyflakes: a rebound name inherits the usage of the previous binding
        // (it may be read again later in a loop, or as a global)
        if let Some(&old) = self.scopes[target].names.get(name)
            && shadows.is_none()
            && self.bindings[old].used
        {
            used = true;
        }
        self.bindings.push(Binding {
            name,
            kind,
            span,
            used,
            fork: self.fork.clone(),
            import,
            shadows,
        });
        let id = self.bindings.len() - 1;
        self.scopes[target].names.insert(name, id);
    }

    fn lookup(&mut self, name: &str, span: Span) {
        let mut sid = Some(self.scope);
        let mut first = true;
        while let Some(s) = sid {
            let sc = &self.scopes[s];
            if (first || sc.kind != ScopeKind::Class)
                && let Some(&b) = sc.names.get(name)
            {
                self.mark_used(b);
                return;
            }
            first = false;
            sid = sc.parent;
        }
        if is_builtin(name) || self.scopes[0].star_import || self.catches_name_error > 0 {
            return;
        }
        if self.scopes[self.scope].kind == ScopeKind::Class
            && matches!(
                name,
                "__module__" | "__qualname__" | "__firstlineno__" | "__classdictcell__"
            )
        {
            return;
        }
        if name == "__class__" && self.in_function {
            return;
        }
        self.report(Rule::UndefinedName, span, format!("Undefined name `{name}`"));
    }

    // ---------------- statements ----------------
    fn body(&mut self, body: Body<'a>) {
        let mut terminated = false;
        let sole = body.len() == 1;
        if let Some(first) = body.first()
            && !self.in_def_body.take()
        {
            let before = &self.src.as_bytes()[..first.span.start as usize];
            let trimmed = before.iter().rposition(|c| !matches!(c, b' ' | b'\t'));
            if trimmed.is_some_and(|i| before[i] == b':') {
                self.report(
                    Rule::MultipleStatementsColon,
                    first.span,
                    "Multiple statements on one line (colon)",
                );
            }
        }
        for s in body {
            if terminated {
                self.report(Rule::UnreachableCode, s.span, "Unreachable code");
                terminated = false;
            }
            if matches!(s.kind, StmtKind::Import(_) | StmtKind::ImportFrom { .. }) {
                self.sole_in_block.insert(s.span.start, sole);
            }
            self.stmt(s);
            if matches!(
                s.kind,
                StmtKind::Return(_) | StmtKind::Raise { .. } | StmtKind::Break | StmtKind::Continue
            ) {
                terminated = true;
            }
        }
    }

    fn forked<F: FnOnce(&mut Self)>(&mut self, id: u32, branch: u32, f: F) {
        self.fork.push((id, branch));
        f(self);
        self.fork.pop();
    }

    fn new_fork(&mut self) -> u32 {
        self.next_fork += 1;
        self.next_fork
    }

    fn check_e402(&mut self, s: &'a Stmt<'a>) {
        if self.scope != 0 || !self.fork.is_empty() {
            return;
        }
        match &s.kind {
            StmtKind::Import(_) | StmtKind::ImportFrom { .. } => {
                if self.seen_non_import {
                    self.report(
                        Rule::ModuleImportNotAtTop,
                        s.span,
                        "Module level import not at top of file",
                    );
                }
            }
            StmtKind::Expr(e) if matches!(e.kind, ExprKind::Str(_)) => {}
            StmtKind::If { .. } | StmtKind::Try { .. } | StmtKind::With { .. } | StmtKind::Pass => {}
            StmtKind::Assign { targets, .. }
                if targets
                    .iter()
                    .all(|t| matches!(t.kind, ExprKind::Name(n) if n.starts_with("__") && n.ends_with("__"))) => {}
            StmtKind::Expr(e) if is_sys_path_call(e) => {}
            _ => self.seen_non_import = true,
        }
    }

    fn stmt(&mut self, s: &'a Stmt<'a>) {
        self.check_e402(s);
        match &s.kind {
            StmtKind::FunctionDef {
                name,
                decorators,
                type_params,
                params,
                returns,
                body,
                ..
            } => {
                for d in *decorators {
                    self.expr(d);
                }
                let overload = decorators.iter().any(|d| {
                    matches!(d.kind, ExprKind::Name("overload"))
                        || matches!(d.kind, ExprKind::Attribute(_, a) if a.name == "overload")
                });
                let outer = self.scope;
                let tp_scope = if type_params.is_empty() {
                    outer
                } else {
                    self.type_param_scope(type_params)
                };
                for p in params.all() {
                    if let Some(d) = p.default {
                        self.check_mutable_default(d);
                        self.expr(d);
                    }
                    if let Some(a) = p.annotation {
                        self.annotation(a);
                    }
                }
                if let Some(r) = returns {
                    self.annotation(r);
                }
                self.scope = outer;
                self.add_binding(name.name, name.span, BKind::FunctionDef { overload }, None);
                self.deferred.push(Deferred::Function {
                    scope: tp_scope,
                    params,
                    body,
                });
            }
            StmtKind::ClassDef {
                name,
                decorators,
                type_params,
                bases,
                body,
                ..
            } => {
                for d in *decorators {
                    self.expr(d);
                }
                let outer = self.scope;
                let tp_scope = if type_params.is_empty() {
                    outer
                } else {
                    self.type_param_scope(type_params)
                };
                for a in *bases {
                    self.expr(a.value());
                }
                let saved_loop = self.loop_depth;
                self.loop_depth = 0;
                let saved_fork = std::mem::take(&mut self.fork);
                let cs = self.push_scope(ScopeKind::Class, tp_scope);
                self.body(body);
                self.finish_scope(cs);
                self.fork = saved_fork;
                self.loop_depth = saved_loop;
                self.scope = outer;
                if AMBIGUOUS.contains(&name.name) {
                    self.report(
                        Rule::AmbiguousVariableName,
                        name.span,
                        format!("Ambiguous class name: `{}`", name.name),
                    );
                }
                self.add_binding(name.name, name.span, BKind::ClassDef, None);
            }
            StmtKind::Return(v) => {
                if !self.in_function {
                    self.report(
                        Rule::ReturnOutsideFunction,
                        s.span,
                        "`return` statement outside of a function/method",
                    );
                }
                if let Some(v) = v {
                    self.expr(v);
                }
            }
            StmtKind::Delete(targets) => {
                for t in *targets {
                    self.delete(t);
                }
            }
            StmtKind::Assign { targets, value } => {
                self.expr(value);
                if targets.len() == 1
                    && matches!(targets[0].kind, ExprKind::Name(_))
                    && matches!(value.kind, ExprKind::Lambda(..))
                    && self.scopes[self.scope].kind != ScopeKind::Class
                {
                    self.report(
                        Rule::LambdaAssignment,
                        s.span,
                        "Do not assign a `lambda` expression, use a `def`",
                    );
                }
                if self.scope == 0 && targets.len() == 1 && targets[0].is_name("__all__") {
                    self.collect_all(value, s.span);
                }
                for t in *targets {
                    self.store(t, true);
                }
            }
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => {
                self.store(name, false);
                let outer = self.scope;
                let sc = if type_params.is_empty() {
                    outer
                } else {
                    self.type_param_scope(type_params)
                };
                self.scope = outer;
                self.deferred.push(Deferred::Expr { scope: sc, expr: value });
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(value);
                if self.scope == 0 && target.is_name("__all__") {
                    self.collect_all(value, s.span);
                }
                match target.kind {
                    ExprKind::Name(n) => {
                        self.lookup(n, target.span);
                        if !self.scopes[self.scope].names.contains_key(n) {
                            self.add_binding(n, target.span, BKind::Plain, None);
                        }
                    }
                    _ => self.expr(target),
                }
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
            } => {
                if let Some(v) = value {
                    self.expr(v);
                }
                self.annotation(annotation);
                if let ExprKind::Name(n) = target.kind {
                    let kind = if value.is_some() {
                        BKind::Assignment
                    } else {
                        BKind::Plain
                    };
                    self.check_ambiguous(n, target.span);
                    self.add_binding(n, target.span, kind, None);
                    if value.is_none() && self.scope != 0 {
                        // bare annotations don't bind at runtime but are fine
                        if let Some(&b) = self.scopes[self.scope].names.get(n) {
                            self.bindings[b].used = true;
                        }
                    }
                } else {
                    self.expr(target);
                }
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
                ..
            } => {
                self.expr(iter);
                self.store(target, false);
                self.loop_depth += 1;
                self.body(body);
                self.loop_depth -= 1;
                self.body(orelse);
            }
            StmtKind::While { test, body, orelse } => {
                self.expr(test);
                self.loop_depth += 1;
                self.body(body);
                self.loop_depth -= 1;
                self.body(orelse);
            }
            StmtKind::If { test, body, orelse, .. } => {
                self.expr(test);
                let id = self.new_fork();
                self.forked(id, 0, |c| c.body(body));
                self.forked(id, 1, |c| c.body(orelse));
            }
            StmtKind::With { items, body, .. } => {
                for i in *items {
                    self.expr(i.context);
                    if let Some(v) = i.vars {
                        self.store(v, true);
                    }
                }
                self.body(body);
            }
            StmtKind::Match { subject, cases } => {
                self.expr(subject);
                let id = self.new_fork();
                for (i, c) in cases.iter().enumerate() {
                    self.forked(id, i as u32, |ch| {
                        ch.pattern(&c.pattern);
                        if let Some(g) = c.guard {
                            ch.expr(g);
                        }
                        ch.body(c.body);
                    });
                }
            }
            StmtKind::Raise { exc, cause } => {
                if let Some(e) = exc {
                    let target = match e.kind {
                        ExprKind::Call(f, _) => f,
                        _ => e,
                    };
                    if target.is_name("NotImplemented") {
                        let fix = Fix::replace(target.span, "NotImplementedError");
                        if let Some(d) = self.report(
                            Rule::RaiseNotImplemented,
                            target.span,
                            "`raise NotImplemented` should be `raise NotImplementedError`",
                        ) {
                            d.fix = Some(fix);
                        }
                    }
                    self.expr(e);
                }
                if let Some(c) = cause {
                    self.expr(c);
                }
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            } => {
                let catches_ne = handlers.iter().any(|h| match h.typ {
                    None => true,
                    Some(t) => names_in(t)
                        .iter()
                        .any(|n| matches!(*n, "NameError" | "Exception" | "BaseException")),
                });
                let id = self.new_fork();
                if catches_ne {
                    self.catches_name_error += 1;
                }
                self.forked(id, 0, |c| c.body(body));
                if catches_ne {
                    self.catches_name_error -= 1;
                }
                for (i, h) in handlers.iter().enumerate() {
                    self.forked(id, i as u32 + 1, |c| c.handler(h));
                }
                self.forked(id, 0, |c| c.body(orelse));
                self.body(finalbody);
            }
            StmtKind::Assert { test, msg } => {
                if let ExprKind::Tuple { elts, .. } = test.kind
                    && !elts.is_empty()
                {
                    self.report(
                        Rule::AssertTuple,
                        s.span,
                        "Assertion is always true, perhaps remove parentheses?",
                    );
                }
                if matches!(test.kind, ExprKind::Constant(Constant::False)) {
                    let m = msg.map(|m| self.text(m.span)).unwrap_or("");
                    let fix = if msg.is_none_or(|m| m.parens == 0) {
                        Some(Fix::replace(s.span, format!("raise AssertionError({m})")))
                    } else {
                        None
                    };
                    if let Some(d) = self.report(
                        Rule::AssertFalse,
                        test.span,
                        "Do not `assert False`, raise `AssertionError()`",
                    ) {
                        d.fix = fix;
                    }
                }
                self.expr(test);
                if let Some(m) = msg {
                    self.expr(m);
                }
            }
            StmtKind::Import(names) => {
                for (i, a) in names.iter().enumerate() {
                    let full = a.name.name;
                    let (bname, span, q) = match a.asname {
                        Some(asn) => (
                            asn.name,
                            a.span,
                            if asn.name == full {
                                full.to_string()
                            } else {
                                format!("{full} as {}", asn.name)
                            },
                        ),
                        None => (full.split('.').next().unwrap().trim(), a.span, full.to_string()),
                    };
                    let explicit_reexport = a.asname.is_some_and(|asn| asn.name == full);
                    self.add_binding(
                        bname,
                        span,
                        if explicit_reexport {
                            BKind::FutureImport
                        } else {
                            BKind::Import
                        },
                        Some((s, i, q)),
                    );
                }
            }
            StmtKind::ImportFrom { module, level, names } => {
                let modname = format!(
                    "{}{}",
                    ".".repeat(*level as usize),
                    module.map(|m| m.name).unwrap_or("")
                );
                let future = modname == "__future__";
                for (i, a) in names.iter().enumerate() {
                    if a.name.name == "*" {
                        self.scopes[self.scope].star_import = true;
                        self.report(
                            Rule::ImportStarUsed,
                            s.span,
                            format!("`from {modname} import *` used; unable to detect undefined names"),
                        );
                        continue;
                    }
                    let bname = a.asname.map(|x| x.name).unwrap_or(a.name.name);
                    let sep = if modname.ends_with('.') { "" } else { "." };
                    let q = match a.asname {
                        Some(x) if x.name != a.name.name => format!("{modname}{sep}{} as {}", a.name.name, x.name),
                        _ => format!("{modname}{sep}{}", a.name.name),
                    };
                    let reexport = a.asname.is_some_and(|x| x.name == a.name.name);
                    let kind = if future || reexport {
                        BKind::FutureImport
                    } else {
                        BKind::Import
                    };
                    self.add_binding(bname, a.span, kind, Some((s, i, q)));
                }
            }
            StmtKind::Global(names) | StmtKind::Nonlocal(names) => {
                let is_global = matches!(s.kind, StmtKind::Global(_));
                for n in *names {
                    self.scopes[self.scope].globals.push(n.name);
                    if is_global {
                        // like pyflakes: the global may be defined by this function, so
                        // forget undefined-name reports already made for it
                        let msg = format!("Undefined name `{}`", n.name);
                        self.diags
                            .retain(|d| !(d.rule == Rule::UndefinedName && d.message == msg));
                    }
                    if is_global && self.scope != 0 {
                        // make the global visible at module level
                        if !self.scopes[0].names.contains_key(n.name) {
                            self.bindings.push(Binding {
                                name: n.name,
                                kind: BKind::Global,
                                span: n.span,
                                used: true,
                                fork: vec![],
                                import: None,
                                shadows: None,
                            });
                            let id = self.bindings.len() - 1;
                            self.scopes[0].names.insert(n.name, id);
                        }
                    }
                }
            }
            StmtKind::Expr(e) => {
                match &e.kind {
                    ExprKind::Compare(..) => {
                        self.report(Rule::UselessComparison, e.span, "Pointless comparison. Did you mean to assign a value? Otherwise, prepend `assert` or remove it.");
                    }
                    ExprKind::Constant(c) if !matches!(c, Constant::Ellipsis) => {
                        self.report(
                            Rule::UselessExpression,
                            e.span,
                            "Found useless expression. Either assign it to a variable or remove it.",
                        );
                    }
                    ExprKind::List(_) | ExprKind::Dict(_) | ExprKind::Set(_) | ExprKind::Tuple { .. } => {
                        self.report(
                            Rule::UselessExpression,
                            e.span,
                            "Found useless expression. Either assign it to a variable or remove it.",
                        );
                    }
                    _ => {}
                }
                self.expr(e);
            }
            StmtKind::Pass => {}
            StmtKind::Break => {
                if self.loop_depth == 0 {
                    self.report(Rule::BreakOutsideLoop, s.span, "`break` outside loop");
                }
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.report(Rule::ContinueOutsideLoop, s.span, "`continue` not properly in loop");
                }
            }
        }
    }

    fn type_param_scope(&mut self, tps: &'a [TypeParam<'a>]) -> usize {
        let s = self.push_scope(ScopeKind::Function, self.scope);
        for t in tps {
            self.add_binding(t.name.name, t.name.span, BKind::Argument, None);
        }
        for t in tps {
            if let Some(b) = t.bound {
                self.deferred.push(Deferred::Expr { scope: s, expr: b });
            }
            if let Some(d) = t.default {
                self.deferred.push(Deferred::Expr { scope: s, expr: d });
            }
        }
        s
    }

    fn handler(&mut self, h: &'a ExceptHandler<'a>) {
        if h.typ.is_none() {
            let sp = Span::new(h.span.start, h.span.start + 6);
            self.report(Rule::BareExcept, sp, "Do not use bare `except`");
        }
        if let Some(t) = h.typ {
            self.expr(t);
        }
        let prev = h.name.and_then(|n| self.scopes[self.scope].names.get(n.name).copied());
        if let Some(n) = h.name {
            self.add_binding(n.name, n.span, BKind::ExceptName, None);
        }
        self.body(h.body);
        if let Some(n) = h.name {
            if let Some(&b) = self.scopes[self.scope].names.get(n.name)
                && self.bindings[b].kind == BKind::ExceptName
                && !self.bindings[b].used
                && !self.scopes[self.scope].uses_locals
            {
                self.report(
                    Rule::UnusedVariable,
                    n.span,
                    format!("Local variable `{}` is assigned to but never used", n.name),
                );
            }
            // the name is unbound after the handler
            match prev {
                Some(p) => {
                    self.scopes[self.scope].names.insert(n.name, p);
                }
                None => {
                    self.scopes[self.scope].names.remove(n.name);
                }
            }
        }
    }

    fn annotation(&mut self, a: Ex<'a>) {
        if self.future_annotations {
            self.deferred.push(Deferred::Expr {
                scope: self.scope,
                expr: a,
            });
        } else {
            self.expr(a);
        }
    }

    fn collect_all(&mut self, v: Ex<'a>, stmt: Span) {
        let elts: &[Ex] = match v.kind {
            ExprKind::List(e) | ExprKind::Tuple { elts: e, .. } => e,
            _ => return,
        };
        for e in elts {
            if let ExprKind::Str(parts) = e.kind
                && parts.len() == 1
                && parts[0].kind == StrKind::Str
            {
                let (_, body, _) = crate::dump::normalize_str_token(parts[0].text);
                self.all_names.push((body, Span::new(stmt.start, stmt.start + 7)));
            }
        }
    }

    fn check_ambiguous(&mut self, n: &str, span: Span) {
        if AMBIGUOUS.contains(&n) {
            self.report(
                Rule::AmbiguousVariableName,
                span,
                format!("Ambiguous variable name: `{n}`"),
            );
        }
    }

    fn store(&mut self, t: Ex<'a>, direct: bool) {
        match t.kind {
            ExprKind::Name(n) => {
                self.check_ambiguous(n, t.span);
                self.add_binding(n, t.span, if direct { BKind::Assignment } else { BKind::Plain }, None);
            }
            ExprKind::Tuple { elts, .. } | ExprKind::List(elts) => {
                for e in elts {
                    self.store(e, false);
                }
            }
            ExprKind::Starred(v) => self.store(v, false),
            _ => self.expr(t),
        }
    }

    fn delete(&mut self, t: Ex<'a>) {
        match t.kind {
            ExprKind::Name(n) => {
                self.lookup(n, t.span);
                if self.fork.is_empty() && self.loop_depth == 0 && self.catches_name_error == 0 {
                    self.scopes[self.scope].names.remove(n);
                }
            }
            ExprKind::Tuple { elts, .. } | ExprKind::List(elts) => {
                for e in elts {
                    self.delete(e);
                }
            }
            _ => self.expr(t),
        }
    }

    fn pattern(&mut self, p: &'a Pattern<'a>) {
        match p {
            Pattern::Value(e) | Pattern::Singleton(e) => self.expr(e),
            Pattern::Sequence(ps, _) | Pattern::Or(ps, _) => {
                for p in *ps {
                    self.pattern(p);
                }
            }
            Pattern::Mapping {
                keys, patterns, rest, ..
            } => {
                for k in *keys {
                    self.expr(k);
                }
                for p in *patterns {
                    self.pattern(p);
                }
                if let Some(r) = rest {
                    self.add_binding(r.name, r.span, BKind::Plain, None);
                }
            }
            Pattern::Class { cls, patterns, kwd, .. } => {
                self.expr(cls);
                for p in *patterns {
                    self.pattern(p);
                }
                for (_, p) in *kwd {
                    self.pattern(p);
                }
            }
            Pattern::Star(n, _) => {
                if let Some(n) = n {
                    self.add_binding(n.name, n.span, BKind::Plain, None);
                }
            }
            Pattern::As(p, n, _) => {
                if let Some(p) = p {
                    self.pattern(p);
                }
                if let Some(n) = n {
                    self.add_binding(n.name, n.span, BKind::Plain, None);
                }
            }
        }
    }

    fn check_mutable_default(&mut self, d: Ex<'a>) {
        let mutable = match d.kind {
            ExprKind::List(_)
            | ExprKind::Dict(_)
            | ExprKind::Set(_)
            | ExprKind::ListComp(..)
            | ExprKind::DictComp(..)
            | ExprKind::SetComp(..) => true,
            ExprKind::Call(f, _) => matches!(f.kind, ExprKind::Name("list" | "dict" | "set" | "bytearray")),
            _ => false,
        };
        if mutable {
            self.report(
                Rule::MutableArgumentDefault,
                d.span,
                "Do not use mutable data structures for argument defaults",
            );
        }
    }

    // ---------------- expressions ----------------
    fn comprehension(&mut self, gens: &'a [Comprehension<'a>], elts: &[Ex<'a>]) {
        if let Some(g) = gens.first() {
            self.expr(g.iter);
        }
        let outer = self.scope;
        let s = self.push_scope(ScopeKind::Comprehension, outer);
        for (i, g) in gens.iter().enumerate() {
            if i > 0 {
                self.expr(g.iter);
            }
            self.store(g.target, false);
            for c in g.ifs {
                self.expr(c);
            }
        }
        for e in elts {
            self.expr(e);
        }
        let _ = s;
        self.scope = outer;
    }

    fn compare_checks(&mut self, e: &'a Expr<'a>, left: Ex<'a>, ops: &'a [(CmpOp, Ex<'a>)]) {
        let mut prev = left;
        for (op, right) in ops {
            let pair = [prev, *right];
            match op {
                CmpOp::Eq | CmpOp::NotEq => {
                    let is_none = |x: &Expr| matches!(x.kind, ExprKind::Constant(Constant::None));
                    let is_bool = |x: &Expr| matches!(x.kind, ExprKind::Constant(Constant::True | Constant::False));
                    let opn = if *op == CmpOp::Eq { "is" } else { "is not" };
                    if pair.iter().any(|x| is_none(x)) {
                        let fix = self.op_fix(prev, right, op.as_str(), opn);
                        let msg = format!("Comparison to `None` should be `cond {opn} None`");
                        if let Some(d) = self.report(
                            Rule::NoneComparison,
                            if is_none(right) { right.span } else { prev.span },
                            msg,
                        ) {
                            d.fix = fix;
                        }
                    } else if pair.iter().any(|x| is_bool(x)) {
                        let v = if pair
                            .iter()
                            .any(|x| matches!(x.kind, ExprKind::Constant(Constant::True)))
                        {
                            "True"
                        } else {
                            "False"
                        };
                        self.report(
                            Rule::TrueFalseComparison,
                            if is_bool(right) { right.span } else { prev.span },
                            format!("Avoid equality comparisons to `{v}`"),
                        );
                    }
                    let is_type_call =
                        |x: &Expr| matches!(x.kind, ExprKind::Call(f, a) if f.is_name("type") && a.len() == 1);
                    if pair.iter().any(|x| is_type_call(x)) {
                        self.report(
                            Rule::TypeComparison,
                            e.span,
                            "Use `is` and `is not` for type comparisons, or `isinstance()` for isinstance checks",
                        );
                    }
                }
                CmpOp::Is | CmpOp::IsNot => {
                    let lit = |x: &Expr| match x.kind {
                        ExprKind::Str(_) => true,
                        ExprKind::Constant(c) => matches!(c, Constant::Int(_) | Constant::Float(_) | Constant::Imag(_)),
                        ExprKind::Tuple { elts, .. } => !elts.is_empty(),
                        _ => false,
                    };
                    if pair.iter().any(|x| lit(x)) {
                        let new = if *op == CmpOp::Is { "==" } else { "!=" };
                        let fix = self.op_fix(prev, right, op.as_str(), new);
                        let msg = format!("Use `{new}` to compare constant literals");
                        if let Some(d) = self.report(Rule::IsLiteral, e.span, msg) {
                            d.fix = fix;
                        }
                    }
                }
                _ => {}
            }
            prev = right;
        }
    }

    /// Fix replacing the operator between two operands, if the text between
    /// them is exactly the operator (plus whitespace).
    fn op_fix(&self, l: Ex<'a>, r: Ex<'a>, old: &str, new: &str) -> Option<Fix> {
        if l.parens > 0 || r.parens > 0 {
            return None;
        }
        let between = &self.src[l.span.end as usize..r.span.start as usize];
        let norm: Vec<&str> = between.split_whitespace().collect();
        if norm.join(" ") != old {
            return None;
        }
        Some(Fix::replace(Span::new(l.span.end, r.span.start), format!(" {new} ")))
    }

    fn check_escapes(&mut self, lit: &StrLit<'a>) {
        if !self.rules.enabled(Rule::InvalidEscapeSequence) {
            return;
        }
        let pre_len = lit.text.find(['\'', '"']).unwrap_or(0);
        let prefix = &lit.text[..pre_len];
        if prefix.contains(['r', 'R']) {
            return;
        }
        let is_bytes = lit.kind == StrKind::Bytes;
        let ranges: Vec<Span> = if lit.kind == StrKind::FString {
            let mut v = Vec::new();
            collect_literal_spans(lit.fparts, &mut v);
            v
        } else {
            vec![lit.span]
        };
        for r in ranges {
            let b = self.src.as_bytes();
            let mut i = r.start as usize;
            let end = r.end as usize;
            if r == lit.span {
                i += pre_len;
            }
            while i + 1 < end {
                if b[i] == b'\\' {
                    let c = b[i + 1];
                    let ok = matches!(
                        c,
                        b'\n' | b'\r' | b'\\' | b'\'' | b'"' | b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v' | b'0'
                            ..=b'7' | b'x'
                    ) || (!is_bytes && matches!(c, b'N' | b'u' | b'U'))
                        || (lit.kind == StrKind::FString && matches!(c, b'{' | b'}'));
                    if !ok {
                        let ch = self.src[i + 1..].chars().next().unwrap_or(' ');
                        self.report(
                            Rule::InvalidEscapeSequence,
                            Span::new(i as u32, (i + 1 + ch.len_utf8()) as u32),
                            format!("Invalid escape sequence: `\\{ch}`"),
                        );
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
    }

    fn expr(&mut self, e: &'a Expr<'a>) {
        match &e.kind {
            ExprKind::Name(n) => self.lookup(n, e.span),
            ExprKind::BoolOp(_, vs) => {
                for v in *vs {
                    self.expr(v);
                }
            }
            ExprKind::Named(t, v) => {
                self.expr(v);
                if let ExprKind::Name(n) = t.kind {
                    // binds in the nearest non-comprehension scope
                    let saved = self.scope;
                    while self.scopes[self.scope].kind == ScopeKind::Comprehension {
                        self.scope = self.scopes[self.scope].parent.unwrap();
                    }
                    self.add_binding(n, t.span, BKind::Plain, None);
                    self.scope = saved;
                }
            }
            ExprKind::BinOp(l, _, r) => {
                self.expr(l);
                self.expr(r);
            }
            ExprKind::Unary(op, v) => {
                if *op == UnaryOp::Not
                    && let ExprKind::Compare(l, ops) = v.kind
                    && ops.len() == 1
                    && v.parens <= 1
                {
                    let (cop, r) = ops[0];
                    let (rule, new) = match cop {
                        CmpOp::In => (Some(Rule::NotInTest), "not in"),
                        CmpOp::Is => (Some(Rule::NotIsTest), "is not"),
                        _ => (None, ""),
                    };
                    if let Some(rule) = rule {
                        let fix = if l.parens == 0 && r.parens == 0 {
                            Some(Fix::replace(
                                e.span,
                                format!("{} {new} {}", self.text(l.span), self.text(r.span)),
                            ))
                        } else {
                            None
                        };
                        let msg = if rule == Rule::NotInTest {
                            "Test for membership should be `not in`"
                        } else {
                            "Test for object identity should be `is not`"
                        };
                        if let Some(d) = self.report(rule, e.span, msg) {
                            d.fix = fix;
                        }
                    }
                }
                self.expr(v);
            }
            ExprKind::Lambda(params, body) => {
                for p in params.all() {
                    if let Some(d) = p.default {
                        self.expr(d);
                    }
                }
                self.deferred.push(Deferred::Lambda {
                    scope: self.scope,
                    params,
                    body,
                });
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.expr(test);
                self.expr(body);
                self.expr(orelse);
            }
            ExprKind::Dict(items) => {
                let mut seen: HashMap<String, (String, Span)> = HashMap::new();
                let mut reported_first: Vec<u32> = Vec::new();
                for (k, v) in *items {
                    if let Some(k) = k {
                        let simple = matches!(k.kind, ExprKind::Str(_) | ExprKind::Constant(_) | ExprKind::Name(_));
                        if simple {
                            let kd = dump_expr(k);
                            let vd = dump_expr(v);
                            if let Some((pv, first_span)) = seen.get(&kd).cloned() {
                                if pv != vd {
                                    let t = self.text(k.span).to_string();
                                    if !reported_first.contains(&first_span.start) {
                                        reported_first.push(first_span.start);
                                        self.report(
                                            Rule::MultiValueRepeatedKey,
                                            first_span,
                                            format!("Dictionary key `{t}` repeated"),
                                        );
                                    }
                                    self.report(
                                        Rule::MultiValueRepeatedKey,
                                        k.span,
                                        format!("Dictionary key `{t}` repeated"),
                                    );
                                }
                            } else {
                                seen.insert(kd, (vd, k.span));
                            }
                        }
                        self.expr(k);
                    }
                    self.expr(v);
                }
            }
            ExprKind::Set(v) | ExprKind::List(v) | ExprKind::Tuple { elts: v, .. } => {
                for x in *v {
                    self.expr(x);
                }
            }
            ExprKind::ListComp(el, g) | ExprKind::SetComp(el, g) | ExprKind::Generator(el, g) => {
                self.comprehension(g, &[el])
            }
            ExprKind::DictComp(k, v, g) => self.comprehension(g, &[k, v]),
            ExprKind::Await(v) | ExprKind::Starred(v) => self.expr(v),
            ExprKind::Yield(v) => {
                if !self.in_function {
                    self.report(
                        Rule::YieldOutsideFunction,
                        e.span,
                        "`yield` statement outside of a function",
                    );
                }
                if let Some(v) = v {
                    self.expr(v);
                }
            }
            ExprKind::YieldFrom(v) => {
                if !self.in_function {
                    self.report(
                        Rule::YieldOutsideFunction,
                        e.span,
                        "`yield from` statement outside of a function",
                    );
                }
                self.expr(v);
            }
            ExprKind::Compare(l, ops) => {
                self.compare_checks(e, l, ops);
                self.expr(l);
                for (_, r) in *ops {
                    self.expr(r);
                }
            }
            ExprKind::Call(f, args) => {
                match f.kind {
                    ExprKind::Name("print") => {
                        self.report(Rule::Print, f.span, "`print` found");
                    }
                    ExprKind::Name("pprint") => {
                        self.report(Rule::PPrint, f.span, "`pprint` found");
                    }
                    ExprKind::Attribute(v, a) if a.name == "pprint" && v.is_name("pprint") => {
                        self.report(Rule::PPrint, f.span, "`pprint` found");
                    }
                    ExprKind::Name("locals") if args.is_empty() => {
                        self.scopes[self.scope].uses_locals = true;
                    }
                    _ => {}
                }
                self.expr(f);
                for a in *args {
                    self.expr(a.value());
                }
            }
            ExprKind::Attribute(v, _) => self.expr(v),
            ExprKind::Subscript(v, s) => {
                self.expr(v);
                self.expr(s);
            }
            ExprKind::Slice(a, b, c) => {
                for x in [a, b, c].into_iter().flatten() {
                    self.expr(x);
                }
            }
            ExprKind::Constant(_) => {}
            ExprKind::Str(parts) => {
                let any_f = parts.iter().any(|p| p.kind == StrKind::FString);
                let has_field = parts
                    .iter()
                    .any(|p| p.fparts.iter().any(|f| matches!(f, FPart::Field { .. })));
                for p in *parts {
                    self.check_escapes(p);
                }
                if any_f && !has_field {
                    for p in parts.iter().filter(|p| p.kind == StrKind::FString) {
                        let pre_len = p.text.find(['\'', '"']).unwrap_or(0);
                        let fix = if p.text.contains(['{', '}']) {
                            None
                        } else {
                            let new_prefix: String =
                                p.text[..pre_len].chars().filter(|c| !matches!(c, 'f' | 'F')).collect();
                            Some(Fix::replace(
                                Span::new(p.span.start, p.span.start + pre_len as u32),
                                new_prefix,
                            ))
                        };
                        if let Some(d) = self.report(
                            Rule::FStringMissingPlaceholders,
                            p.span,
                            "f-string without any placeholders",
                        ) {
                            d.fix = fix;
                        }
                    }
                }
                for p in *parts {
                    self.fparts(p.fparts);
                }
            }
        }
    }

    fn fparts(&mut self, parts: &'a [FPart<'a>]) {
        for p in parts {
            if let FPart::Field { expr, spec, .. } = p {
                self.expr(expr);
                if let Some(s) = spec {
                    self.fparts(s);
                }
            }
        }
    }
}

static NONE_EXPR: Expr<'static> = Expr {
    kind: ExprKind::Constant(Constant::None),
    span: Span::new(0, 0),
    parens: 0,
};

fn collect_literal_spans(parts: &[FPart], out: &mut Vec<Span>) {
    for p in parts {
        match p {
            FPart::Literal(s) => out.push(*s),
            FPart::Field { spec: Some(s), .. } => collect_literal_spans(s, out),
            _ => {}
        }
    }
}

fn names_in<'a>(e: &Expr<'a>) -> Vec<&'a str> {
    match e.kind {
        ExprKind::Name(n) => vec![n],
        ExprKind::Attribute(_, a) => vec![a.name],
        ExprKind::Tuple { elts, .. } => elts.iter().flat_map(|e| names_in(e)).collect(),
        _ => vec![],
    }
}

fn is_sys_path_call(e: &Expr) -> bool {
    if let ExprKind::Call(f, _) = e.kind
        && let ExprKind::Attribute(v, _) = f.kind
        && let ExprKind::Attribute(sys, path) = v.kind
    {
        return sys.is_name("sys") && path.name == "path";
    }
    false
}
