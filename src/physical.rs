//! Physical-line and token-based rules (E501, W291, W293, W191, W292, E702, E703).

use crate::lexer::{Tok, Token};
use crate::rules::{Diagnostic, Fix, Rule, RuleSet};
use crate::span::Span;

pub fn check_lines(src: &str, tokens: &[Token], rules: &RuleSet, line_length: usize, out: &mut Vec<Diagnostic>) {
    // spans of multi-line strings: whitespace fixes inside them would change the AST
    let strings: Vec<Span> = tokens
        .iter()
        .filter(|t| matches!(t.kind, Tok::String | Tok::FString) && t.text.contains('\n'))
        .map(|t| t.span)
        .collect();
    let in_string = |off: u32| {
        let i = strings.partition_point(|s| s.end <= off);
        i < strings.len() && strings[i].start < off
    };
    let mut off = 0usize;
    for raw_line in src.split_inclusive('\n') {
        let line = raw_line.trim_end_matches(['\n', '\r']);
        let start = off as u32;
        off += raw_line.len();
        if rules.enabled(Rule::LineTooLong) {
            let n = line.chars().count();
            if n > line_length && !long_line_exempt(line, line_length, in_string(start)) {
                let cut = line
                    .char_indices()
                    .nth(line_length)
                    .map(|(i, _)| i)
                    .unwrap_or(line.len());
                out.push(Diagnostic::new(
                    Rule::LineTooLong,
                    Span::new(start + cut as u32, start + line.len() as u32),
                    format!("Line too long ({n} > {line_length})"),
                ));
            }
        }
        let trimmed = line.trim_end_matches([' ', '\t', '\x0c']);
        if trimmed.len() != line.len() {
            let ws = Span::new(start + trimmed.len() as u32, start + line.len() as u32);
            let fixable = !in_string(ws.start);
            let d = if trimmed.is_empty() {
                Diagnostic::new(Rule::BlankLineWithWhitespace, ws, "Blank line contains whitespace")
            } else {
                Diagnostic::new(Rule::TrailingWhitespace, ws, "Trailing whitespace")
            };
            if rules.enabled(d.rule) {
                out.push(if fixable { d.with_fix(Fix::delete(ws)) } else { d });
            }
        }
        if rules.enabled(Rule::TabIndentation) {
            let indent: &str = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
            if indent.contains('\t') && !in_string(start) {
                out.push(Diagnostic::new(
                    Rule::TabIndentation,
                    Span::new(start, start + indent.len() as u32),
                    "Indentation contains tabs",
                ));
            }
        }
    }
    if rules.enabled(Rule::MissingNewlineAtEof) && !src.is_empty() && !src.ends_with('\n') {
        let end = src.len() as u32;
        out.push(
            Diagnostic::new(
                Rule::MissingNewlineAtEof,
                Span::new(end, end),
                "No newline at end of file",
            )
            .with_fix(Fix::replace(Span::new(end, end), "\n")),
        );
    }
    // semicolons
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != Tok::Semi {
            continue;
        }
        let next = tokens.get(i + 1).map(|t| t.kind).unwrap_or(Tok::EndMarker);
        if matches!(next, Tok::Newline | Tok::EndMarker) {
            if rules.enabled(Rule::UselessSemicolon) {
                let prev_end = if i > 0 { tokens[i - 1].span.end } else { t.span.start };
                out.push(
                    Diagnostic::new(
                        Rule::UselessSemicolon,
                        t.span,
                        "Statement ends with an unnecessary semicolon",
                    )
                    .with_fix(Fix::delete(Span::new(prev_end, t.span.end))),
                );
            }
        } else if rules.enabled(Rule::MultipleStatementsSemicolon) {
            out.push(Diagnostic::new(
                Rule::MultipleStatementsSemicolon,
                t.span,
                "Multiple statements on one line (semicolon)",
            ));
        }
    }
}

/// pycodestyle's special case: long URLs in comments/strings without spaces.
fn long_line_exempt(line: &str, max: usize, multiline: bool) -> bool {
    let chunks: Vec<&str> = line.split_whitespace().collect();
    if (chunks.len() == 1 && multiline) || (chunks.len() == 2 && chunks[0] == "#") {
        let last = chunks.last().unwrap();
        return line.chars().count() - last.chars().count() < max.saturating_sub(7);
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Noqa {
    All,
    Codes(Vec<String>),
}

/// Parses `# noqa` / `# noqa: E501, F401` directives, keyed by 0-based line.
pub fn noqa_map(src: &str, comments: &[Span], index: &crate::span::LineIndex) -> Vec<(usize, Noqa)> {
    let mut out = Vec::new();
    for c in comments {
        let text = &src[c.range()];
        let lower = text.to_ascii_lowercase();
        let Some(pos) = lower.find("noqa") else { continue };
        let before = lower[..pos].trim_end();
        if !before.ends_with('#') {
            continue;
        }
        let rest = text[pos + 4..].trim_start();
        let line = index.line_of(c.start);
        if let Some(codes) = rest.strip_prefix(':') {
            let codes: Vec<String> = codes
                .split([',', ' ', '\t'])
                .map(|s| s.trim())
                .take_while(|s| s.is_empty() || s.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            if codes.is_empty() {
                out.push((line, Noqa::All));
            } else {
                out.push((line, Noqa::Codes(codes)));
            }
        } else {
            out.push((line, Noqa::All));
        }
    }
    out
}
