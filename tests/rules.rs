//! Per-rule snapshot tests. Each rule has `tests/fixtures/rules/<CODE>.py`
//! containing positive cases (lines marked `# ERR`) and negative cases
//! (unmarked lines). The test asserts that diagnostics appear exactly on the
//! marked lines, and compares the full output (plus the `--fix` result for
//! fixable rules) with `tests/snapshots/<CODE>.snap`.
//! Run with `UPDATE_SNAPSHOTS=1` to (re)write snapshots.

use quiver::config::Settings;
use quiver::linter::{fix_source, lint_source};
use quiver::rules::Rule;
use quiver::span::LineIndex;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn settings_for(rule: Rule) -> Settings {
    Settings::build(88, vec![rule.code().to_string()], vec![], vec![], vec![])
}

fn render(rule: Rule, src: &str) -> (String, BTreeSet<usize>) {
    let s = settings_for(rule);
    let diags: Vec<_> = lint_source(src, &s, false)
        .diagnostics
        .into_iter()
        .filter(|d| d.rule == rule)
        .collect();
    let idx = LineIndex::new(src);
    let mut out = String::new();
    let mut lines = BTreeSet::new();
    for d in &diags {
        let (l, c) = idx.location(src, d.span.start);
        lines.insert(l);
        out.push_str(&format!(
            "{l}:{c} {} {}{}\n",
            d.rule.code(),
            d.message,
            if d.fix.is_some() { " [*]" } else { "" }
        ));
    }
    if rule.fixable() {
        let fixed = fix_source(src, &s, false);
        out.push_str(&format!("--- fixed ({} fixes) ---\n", fixed.fixed));
        out.push_str(&fixed.source);
    }
    (out, lines)
}

#[test]
fn every_rule_has_fixture_and_matching_snapshot() {
    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
    let mut failures = Vec::new();
    for &rule in Rule::ALL {
        let fx = root().join("fixtures/rules").join(format!("{}.py", rule.code()));
        let src = std::fs::read_to_string(&fx).unwrap_or_else(|_| panic!("missing fixture {}", fx.display()));
        let (out, lines) = render(rule, &src);
        if lines.is_empty() {
            failures.push(format!("{}: fixture produced no diagnostics", rule.code()));
        }
        let marked: BTreeSet<usize> = src
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("# ERR"))
            .map(|(i, _)| i + 1)
            .collect();
        if !marked.is_empty() && marked != lines {
            failures.push(format!(
                "{}: expected diagnostics on lines {marked:?}, got {lines:?}\n{out}",
                rule.code()
            ));
        }
        let snap = root().join("snapshots").join(format!("{}.snap", rule.code()));
        if update {
            std::fs::write(&snap, &out).unwrap();
        } else {
            let expected = std::fs::read_to_string(&snap).unwrap_or_default();
            if expected != out {
                failures.push(format!(
                    "{}: snapshot mismatch\n--- expected\n{expected}\n--- actual\n{out}",
                    rule.code()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn fixes_produce_parseable_code_that_is_clean_for_the_rule() {
    for &rule in Rule::ALL.iter().filter(|r| r.fixable()) {
        let fx = root().join("fixtures/rules").join(format!("{}.py", rule.code()));
        let src = std::fs::read_to_string(&fx).unwrap();
        let s = settings_for(rule);
        let fixed = fix_source(&src, &s, false);
        assert!(fixed.fixed > 0, "{}: no fixes applied", rule.code());
        let bump = bumpalo::Bump::new();
        assert!(
            quiver::parser::parse_module(&fixed.source, &bump).is_ok(),
            "{}: fixed code does not parse",
            rule.code()
        );
        let remaining = lint_source(&fixed.source, &s, false)
            .diagnostics
            .into_iter()
            .filter(|d| d.rule == rule && d.fix.is_some())
            .count();
        assert_eq!(remaining, 0, "{}: fixable diagnostics remain after --fix", rule.code());
    }
}

#[test]
fn noqa_suppresses_diagnostics() {
    let src = "import os  # noqa\nimport sys  # noqa: F401\nimport re  # noqa: E501\nimport json  # NOQA:F4\n";
    let s = Settings::build(88, vec!["F401".into()], vec![], vec![], vec![]);
    let d = lint_source(src, &s, false).diagnostics;
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(&src[d[0].span.range()], "re");
}

#[test]
fn ignore_and_select_from_config() {
    let s = Settings::from_pyproject("[tool.quiver]\nselect = [\"F\"]\nignore = [\"F401\"]\n").unwrap();
    let d = lint_source("import os\nx = 1\nx == 1\nundefined\n", &s, false).diagnostics;
    let codes: Vec<_> = d.iter().map(|d| d.rule.code()).collect();
    assert_eq!(codes, vec!["F821"]);
}

#[test]
fn init_files_have_no_unused_import_autofix() {
    let s = Settings::build(88, vec!["F401".into()], vec![], vec![], vec![]);
    let d = lint_source("import os\n", &s, true).diagnostics;
    assert_eq!(d.len(), 1);
    assert!(d[0].fix.is_none());
}

#[test]
fn unused_import_fix_handles_partial_and_sole_statements() {
    let s = Settings::build(88, vec!["F401".into()], vec![], vec![], vec![]);
    let src = "from a import b, c, d\nimport x, y\nif True:\n    import z\nprint(c, y)\n";
    let fixed = fix_source(src, &s, false);
    assert_eq!(
        fixed.source,
        "from a import c\nimport y\nif True:\n    pass\nprint(c, y)\n"
    );
}

#[test]
fn syntax_error_reports_e999_and_still_runs_physical_checks() {
    let s = Settings::default();
    let d = lint_source("def f(:\n    pass   \n", &s, false).diagnostics;
    assert!(d.iter().any(|d| d.rule == Rule::SyntaxError));
    assert!(d.iter().any(|d| d.rule == Rule::TrailingWhitespace));
}
