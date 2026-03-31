//! Corpus tests over the local CPython standard library (skipped with a
//! message if python3 is unavailable). Parses every file, lints it without
//! panicking, and verifies the formatter round-trip on every file.

use bumpalo::Bump;
use quiver::config::Settings;
use quiver::dump::dump_module;
use quiver::formatter::format_source;
use quiver::linter::lint_source;
use quiver::parser::parse_module;
use std::path::PathBuf;

fn stdlib_files() -> Option<Vec<PathBuf>> {
    let out = std::process::Command::new("python3")
        .args(["-c", "import sysconfig;print(sysconfig.get_paths()['stdlib'])"])
        .output()
        .ok()?;
    let dir = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    let mut files = Vec::new();
    for e in ignore::WalkBuilder::new(&dir).standard_filters(false).build().flatten() {
        let p = e.path();
        let s = p.to_string_lossy();
        if s.contains("-packages") || p.extension().is_none_or(|x| x != "py") {
            continue;
        }
        files.push(p.to_path_buf());
    }
    files.sort();
    Some(files)
}

#[test]
fn stdlib_parses_lints_and_formats() {
    let Some(files) = stdlib_files() else {
        eprintln!("python3 not found; skipping stdlib corpus test");
        return;
    };
    assert!(files.len() > 100, "suspiciously few stdlib files: {}", files.len());
    let settings = Settings::default();
    let mut parse_fail = Vec::new();
    let mut fmt_fail = Vec::new();
    let mut n = 0;
    for p in &files {
        let Ok(bytes) = std::fs::read(p) else { continue };
        let Ok(src) = String::from_utf8(bytes) else { continue };
        n += 1;
        let b = Bump::new();
        let parsed = match parse_module(&src, &b) {
            Ok(m) => m,
            Err(e) => {
                parse_fail.push(format!("{}: {}", p.display(), e.msg));
                continue;
            }
        };
        let _ = lint_source(&src, &settings, false);
        let once = match format_source(&src, 88) {
            Ok(s) => s,
            Err(e) => {
                fmt_fail.push(format!("{}: {e}", p.display()));
                continue;
            }
        };
        let b2 = Bump::new();
        match parse_module(&once, &b2) {
            Ok(re) if dump_module(&re.module) == dump_module(&parsed.module) => {}
            _ => {
                fmt_fail.push(format!("{}: AST changed", p.display()));
                continue;
            }
        }
        if format_source(&once, 88).as_deref() != Ok(once.as_str()) {
            fmt_fail.push(format!("{}: not idempotent", p.display()));
        }
    }
    eprintln!(
        "stdlib: {n} files, {} parse failures, {} format failures",
        parse_fail.len(),
        fmt_fail.len()
    );
    assert!(parse_fail.is_empty(), "parse failures:\n{}", parse_fail.join("\n"));
    assert!(fmt_fail.is_empty(), "format failures:\n{}", fmt_fail.join("\n"));
}
