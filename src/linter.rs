//! Lint driver: parse → semantic checks → physical checks → noqa → fixes.

use crate::checker::Checker;
use crate::config::Settings;
use crate::isort;
use crate::lexer::Lexer;
use crate::parser::parse_module;
use crate::physical::{Noqa, check_lines, noqa_map};
use crate::rules::{Diagnostic, Edit, Rule};
use crate::span::{LineIndex, Span};
use bumpalo::Bump;

pub struct LintResult {
    pub diagnostics: Vec<Diagnostic>,
    pub parse_error: bool,
}

pub fn lint_source(src: &str, settings: &Settings, is_init: bool) -> LintResult {
    let bump = Bump::new();
    let rules = &settings.rules;
    let mut diags = Vec::new();
    let (comments, parse_error) = match parse_module(src, &bump) {
        Ok(parsed) => {
            let module = bump.alloc(parsed.module);
            let mut ck = Checker::new(src, rules, is_init);
            ck.check_module(module);
            diags.append(&mut ck.diags);
            check_lines(src, &parsed.tokens, rules, settings.line_length, &mut diags);
            if rules.enabled(Rule::UnsortedImports) {
                isort::check_imports(src, module.body, &parsed.comments, &settings.isort, &mut diags);
            }
            (parsed.comments, false)
        }
        Err(e) => {
            diags.push(Diagnostic::new(
                Rule::SyntaxError,
                e.span,
                format!("SyntaxError: {}", e.msg),
            ));
            let lexed = Lexer::new(src).tokenize();
            check_lines(src, &lexed.tokens, rules, settings.line_length, &mut diags);
            (lexed.comments, true)
        }
    };
    let index = LineIndex::new(src);
    let noqa = noqa_map(src, &comments, &index);
    if !noqa.is_empty() {
        diags.retain(|d| {
            let line = index.line_of(d.span.start);
            !noqa.iter().any(|(l, n)| {
                *l == line
                    && match n {
                        Noqa::All => true,
                        Noqa::Codes(c) => c.iter().any(|c| d.rule.code().starts_with(c.as_str())),
                    }
            })
        });
    }
    diags.sort_by_key(|d| (d.span.start, d.rule));
    LintResult {
        diagnostics: diags,
        parse_error,
    }
}

/// Applies non-overlapping fixes once. Returns the new source and number of fixes applied.
pub fn apply_fixes(src: &str, diags: &[Diagnostic]) -> (String, usize) {
    let mut edits: Vec<(&Edit, usize)> = Vec::new();
    for (i, d) in diags.iter().enumerate() {
        if let Some(f) = &d.fix {
            for e in &f.edits {
                edits.push((e, i));
            }
        }
    }
    edits.sort_by_key(|(e, _)| (e.start, e.end));
    let mut out = String::with_capacity(src.len());
    let mut last = 0u32;
    let mut applied = std::collections::HashSet::new();
    let mut skipped = std::collections::HashSet::new();
    // a fix is applied only if none of its edits overlap with already applied ones
    for (e, i) in &edits {
        if skipped.contains(i) {
            continue;
        }
        if e.start < last {
            skipped.insert(*i);
            continue;
        }
        out.push_str(&src[last as usize..e.start as usize]);
        out.push_str(&e.content);
        last = e.end;
        applied.insert(*i);
    }
    out.push_str(&src[last as usize..]);
    (out, applied.len())
}

pub struct FixResult {
    pub source: String,
    pub fixed: usize,
    pub remaining: Vec<Diagnostic>,
}

/// Lints and fixes repeatedly until no more fixes apply (max 10 passes).
pub fn fix_source(src: &str, settings: &Settings, is_init: bool) -> FixResult {
    let mut cur = src.to_string();
    let mut total = 0;
    for _ in 0..10 {
        let r = lint_source(&cur, settings, is_init);
        if r.parse_error || !r.diagnostics.iter().any(|d| d.fix.is_some()) {
            return FixResult {
                source: cur,
                fixed: total,
                remaining: r.diagnostics,
            };
        }
        let (next, n) = apply_fixes(&cur, &r.diagnostics);
        // never produce code that no longer parses
        let bump = Bump::new();
        if n == 0 || parse_module(&next, &bump).is_err() {
            return FixResult {
                source: cur,
                fixed: total,
                remaining: r.diagnostics,
            };
        }
        total += n;
        cur = next;
    }
    let r = lint_source(&cur, settings, is_init);
    FixResult {
        source: cur,
        fixed: total,
        remaining: r.diagnostics,
    }
}

pub fn span_location(src: &str, index: &LineIndex, s: Span) -> (usize, usize) {
    index.location(src, s.start)
}
