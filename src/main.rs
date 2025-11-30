use memmap2::Mmap;
use quiver::config::Settings;
use quiver::linter::{fix_source, lint_source};
use quiver::rules::{Diagnostic, Rule, RuleSet};
use quiver::span::LineIndex;
use rayon::prelude::*;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

const USAGE: &str = "quiver - a fast Python linter, formatter and import sorter

USAGE:
    quiver check [PATHS...] [--fix] [--output-format text|json|github] [--select CODES] [--ignore CODES]
                 [--line-length N] [--statistics] [--no-config]
    quiver format [PATHS...] [--check] [--line-length N]
    quiver rules
    quiver parse-stats [PATHS...]     parse every file, report success rate and failures
    quiver dump-ast FILE              print the span-free AST dump";

struct Opts {
    paths: Vec<PathBuf>,
    fix: bool,
    check: bool,
    format: String,
    select: Option<Vec<String>>,
    ignore: Option<Vec<String>>,
    line_length: Option<usize>,
    statistics: bool,
    no_config: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        paths: vec![],
        fix: false,
        check: false,
        format: "text".into(),
        select: None,
        ignore: None,
        line_length: None,
        statistics: false,
        no_config: false,
    };
    let mut i = 0;
    let list = |s: &str| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
    };
    while i < args.len() {
        let a = &args[i];
        let mut val = || -> Result<String, String> {
            i += 1;
            args.get(i).cloned().ok_or(format!("missing value for {a}"))
        };
        match a.as_str() {
            "--fix" => o.fix = true,
            "--check" => o.check = true,
            "--statistics" => o.statistics = true,
            "--no-config" => o.no_config = true,
            "--output-format" | "--format" => o.format = val()?,
            "--select" => o.select = Some(list(&val()?)),
            "--ignore" => o.ignore = Some(list(&val()?)),
            "--line-length" => o.line_length = Some(val()?.parse().map_err(|_| "invalid --line-length")?),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => o.paths.push(PathBuf::from(a)),
        }
        i += 1;
    }
    if o.paths.is_empty() {
        o.paths.push(PathBuf::from("."));
    }
    if !matches!(o.format.as_str(), "text" | "json" | "github") {
        return Err(format!("unknown output format {}", o.format));
    }
    Ok(o)
}

fn settings_for(o: &Opts) -> Result<Settings, String> {
    let mut s = if o.no_config {
        Settings::default()
    } else {
        Settings::discover(&o.paths[0])?.0
    };
    if o.select.is_some() || o.ignore.is_some() || o.line_length.is_some() {
        let select = o.select.clone().unwrap_or(s.select.clone());
        let mut ignore = s.ignore.clone();
        ignore.extend(o.ignore.clone().unwrap_or_default());
        let ll = o.line_length.unwrap_or(s.line_length);
        s = Settings::build(ll, select, ignore, s.exclude.clone(), s.isort.known_first_party.clone());
    }
    Ok(s)
}

/// Parallel-friendly file discovery honoring .gitignore.
fn discover(paths: &[PathBuf], exclude: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_file() {
            out.push(p.clone());
            continue;
        }
        let mut wb = ignore::WalkBuilder::new(p);
        wb.hidden(true).git_ignore(true).require_git(false);
        if !exclude.is_empty() {
            let mut ob = ignore::overrides::OverrideBuilder::new(p);
            for e in exclude {
                let _ = ob.add(&format!("!{e}"));
            }
            if let Ok(ov) = ob.build() {
                wb.overrides(ov);
            }
        }
        for e in wb.build().flatten() {
            let path = e.path();
            if e.file_type().is_some_and(|t| t.is_file()) && path.extension().is_some_and(|x| x == "py" || x == "pyi") {
                out.push(path.to_path_buf());
            }
        }
    }
    out.sort();
    out
}

enum Source {
    Mapped(Mmap),
    Empty,
}

fn read(path: &Path) -> Result<Source, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if f.metadata().map_err(|e| e.to_string())?.len() == 0 {
        return Ok(Source::Empty);
    }
    // SAFETY: the file is only read; concurrent modification by another
    // process is the documented caveat of memory mapping.
    let m = unsafe { Mmap::map(&f) }.map_err(|e| e.to_string())?;
    Ok(Source::Mapped(m))
}

fn with_source<T>(path: &Path, f: impl FnOnce(&str) -> T) -> Result<T, String> {
    match read(path)? {
        Source::Empty => Ok(f("")),
        Source::Mapped(m) => {
            let s = std::str::from_utf8(&m).map_err(|_| "file is not valid UTF-8".to_string())?;
            Ok(f(s))
        }
    }
}

fn is_init(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == "__init__.py")
}

/// A diagnostic with its 1-based start and end (line, column).
type Located = (Diagnostic, (usize, usize), (usize, usize));

struct FileReport {
    path: PathBuf,
    diags: Vec<Located>,
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

fn cmd_check(o: &Opts) -> Result<ExitCode, String> {
    let settings = settings_for(o)?;
    let files = discover(&o.paths, &settings.exclude);
    let fixed_total = AtomicUsize::new(0);
    let reports: Vec<FileReport> = files
        .par_iter()
        .map(|path| {
            let res = with_source(path, |src| {
                let (src_after, diags) = if o.fix {
                    let r = fix_source(src, &settings, is_init(path));
                    if r.fixed > 0 {
                        fixed_total.fetch_add(r.fixed, Ordering::Relaxed);
                        (Some(r.source), r.remaining)
                    } else {
                        (None, r.remaining)
                    }
                } else {
                    (None, lint_source(src, &settings, is_init(path)).diagnostics)
                };
                let text = src_after.as_deref().unwrap_or(src);
                let idx = LineIndex::new(text);
                let diags = diags.into_iter().map(|d| {
                    let s = idx.location(text, d.span.start);
                    let e = idx.location(text, d.span.end);
                    (d, s, e)
                });
                let diags = diags.collect::<Vec<_>>();
                (src_after, diags)
            });
            match res {
                Ok((new_src, diags)) => {
                    if let Some(ns) = new_src
                        && let Err(e) = std::fs::write(path, ns)
                    {
                        eprintln!("error: failed to write {}: {e}", path.display());
                    }
                    FileReport {
                        path: path.clone(),
                        diags,
                    }
                }
                Err(e) => {
                    eprintln!("warning: skipping {}: {e}", path.display());
                    FileReport {
                        path: path.clone(),
                        diags: vec![],
                    }
                }
            }
        })
        .collect();
    let total: usize = reports.iter().map(|r| r.diags.len()).sum();
    let mut out = String::new();
    match o.format.as_str() {
        "json" => {
            out.push('[');
            let mut first = true;
            for r in &reports {
                for (d, s, e) in &r.diags {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    let _ = write!(
                        out,
                        "\n  {{\"code\": \"{}\", \"name\": \"{}\", \"message\": \"{}\", \"filename\": \"{}\", \"location\": {{\"row\": {}, \"column\": {}}}, \"end_location\": {{\"row\": {}, \"column\": {}}}, \"fixable\": {}}}",
                        d.rule.code(),
                        d.rule.name(),
                        json_escape(&d.message),
                        json_escape(&r.path.display().to_string()),
                        s.0,
                        s.1,
                        e.0,
                        e.1,
                        d.fix.is_some()
                    );
                }
            }
            out.push_str("\n]\n");
        }
        "github" => {
            for r in &reports {
                for (d, s, e) in &r.diags {
                    let _ = writeln!(
                        out,
                        "::error title=quiver ({}),file={},line={},col={},endLine={},endColumn={}::{}:{}:{}: {} {}",
                        d.rule.code(),
                        r.path.display(),
                        s.0,
                        s.1,
                        e.0,
                        e.1,
                        r.path.display(),
                        s.0,
                        s.1,
                        d.rule.code(),
                        d.message
                    );
                }
            }
        }
        _ => {
            for r in &reports {
                for (d, s, _) in &r.diags {
                    let _ = writeln!(
                        out,
                        "{}:{}:{}: {} {}{}",
                        r.path.display(),
                        s.0,
                        s.1,
                        d.rule.code(),
                        d.message,
                        if d.fix.is_some() { " [*]" } else { "" }
                    );
                }
            }
            if o.statistics {
                let mut counts: std::collections::BTreeMap<Rule, usize> = Default::default();
                for r in &reports {
                    for (d, _, _) in &r.diags {
                        *counts.entry(d.rule).or_default() += 1;
                    }
                }
                let mut v: Vec<_> = counts.into_iter().collect();
                v.sort_by_key(|x| std::cmp::Reverse(x.1));
                for (r, n) in v {
                    let _ = writeln!(out, "{n:>6}\t{:<8}{}", r.code(), r.name());
                }
            }
            let fixed = fixed_total.load(Ordering::Relaxed);
            if total == 0 {
                let _ = writeln!(out, "All checks passed! ({} files)", reports.len());
            } else {
                let fixable = reports
                    .iter()
                    .flat_map(|r| &r.diags)
                    .filter(|d| d.0.fix.is_some())
                    .count();
                let _ = writeln!(
                    out,
                    "Found {total} error{} in {} files.",
                    if total == 1 { "" } else { "s" },
                    reports.len()
                );
                if fixable > 0 {
                    let _ = writeln!(out, "[*] {fixable} fixable with `quiver check --fix`.");
                }
            }
            if fixed > 0 {
                let _ = writeln!(out, "Fixed {fixed} error{}.", if fixed == 1 { "" } else { "s" });
            }
        }
    }
    print!("{out}");
    Ok(if total > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_format(o: &Opts) -> Result<ExitCode, String> {
    let settings = settings_for(o)?;
    let files = discover(&o.paths, &settings.exclude);
    let changed = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);
    files.par_iter().for_each(|path| {
        let r = with_source(path, |src| {
            quiver::formatter::format_source(src, settings.line_length).map(|f| (f != src).then_some(f))
        });
        match r {
            Ok(Ok(Some(new))) => {
                changed.fetch_add(1, Ordering::Relaxed);
                if o.check {
                    println!("Would reformat: {}", path.display());
                } else if let Err(e) = std::fs::write(path, new) {
                    eprintln!("error: {}: {e}", path.display());
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => {
                eprintln!("error: cannot format {}: {e}", path.display());
                errors.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                eprintln!("error: {}: {e}", path.display());
                errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    let c = changed.load(Ordering::Relaxed);
    let n = files.len();
    if o.check {
        println!(
            "{c} file{} would be reformatted, {} already formatted",
            if c == 1 { "" } else { "s" },
            n - c
        );
    } else {
        println!(
            "{c} file{} reformatted, {} left unchanged",
            if c == 1 { "" } else { "s" },
            n - c
        );
    }
    let failed = errors.load(Ordering::Relaxed) > 0;
    Ok(if failed || (o.check && c > 0) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_parse_stats(o: &Opts) -> Result<ExitCode, String> {
    let files = discover(&o.paths, &[]);
    let start = std::time::Instant::now();
    let results: Vec<(PathBuf, Option<String>)> = files
        .par_iter()
        .map(|p| {
            let r = with_source(p, |src| {
                let bump = bumpalo::Bump::new();
                quiver::parser::parse_module(src, &bump).err().map(|e| {
                    let idx = LineIndex::new(src);
                    let (l, c) = idx.location(src, e.span.start);
                    format!("{l}:{c}: {}", e.msg)
                })
            });
            (p.clone(), r.unwrap_or_else(Some))
        })
        .collect();
    let failed: Vec<_> = results.iter().filter(|r| r.1.is_some()).collect();
    for (p, e) in &failed {
        println!("FAIL {}:{}", p.display(), e.as_ref().unwrap());
    }
    let ok = results.len() - failed.len();
    println!(
        "parsed {ok}/{} files ({:.2}%) in {:.0?}",
        results.len(),
        if results.is_empty() {
            100.0
        } else {
            ok as f64 * 100.0 / results.len() as f64
        },
        start.elapsed()
    );
    Ok(if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Formats every file and verifies: (1) output parses, (2) AST is unchanged
/// (span-insensitive dump), (3) formatting the output again is a no-op.
fn cmd_verify_format(o: &Opts) -> Result<ExitCode, String> {
    let ll = o.line_length.unwrap_or(88);
    let files = discover(&o.paths, &[]);
    let start = std::time::Instant::now();
    let results: Vec<(PathBuf, Result<bool, String>)> = files
        .par_iter()
        .map(|p| {
            let r = with_source(p, |src| -> Result<bool, String> {
                let bump = bumpalo::Bump::new();
                let Ok(orig) = quiver::parser::parse_module(src, &bump) else {
                    return Ok(false);
                };
                let once = quiver::formatter::format_source(src, ll)?;
                let b2 = bumpalo::Bump::new();
                let re = quiver::parser::parse_module(&once, &b2)
                    .map_err(|e| format!("formatted output does not parse: {} at {}", e.msg, e.span.start))?;
                if quiver::dump::dump_module(&orig.module) != quiver::dump::dump_module(&re.module) {
                    return Err("AST changed by formatting".into());
                }
                let twice = quiver::formatter::format_source(&once, ll)?;
                if twice != once {
                    return Err("formatting is not idempotent".into());
                }
                let n_comments = |s: &str| quiver::lexer::tokenize(s).comments.len();
                if n_comments(src) != n_comments(&once) {
                    return Err("comment count changed".into());
                }
                Ok(true)
            });
            (p.clone(), r.and_then(|x| x))
        })
        .collect();
    let mut ok = 0;
    let mut skipped = 0;
    let mut failed = 0;
    for (p, r) in &results {
        match r {
            Ok(true) => ok += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                failed += 1;
                println!("FAIL {}: {e}", p.display());
            }
        }
    }
    println!(
        "verified {ok} files OK, {failed} failed, {skipped} skipped (unparseable) in {:.0?}",
        start.elapsed()
    );
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn real_main() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        println!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let o = parse_args(&args[1..])?;
    match cmd.as_str() {
        "check" => cmd_check(&o),
        "format" => cmd_format(&o),
        "parse-stats" => cmd_parse_stats(&o),
        "verify-format" => cmd_verify_format(&o),
        "rules" => {
            let all = RuleSet::all();
            let default = RuleSet::from_selectors(&[], &[]);
            for r in Rule::ALL {
                let _ = all.enabled(*r);
                println!(
                    "{:<8} {:<42} {}{}{}",
                    r.code(),
                    r.name(),
                    r.description(),
                    if r.fixable() { " (fixable)" } else { "" },
                    if default.enabled(*r) { "" } else { " (off by default)" }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        "dump-ast" => {
            let p = o.paths.first().ok_or("missing file")?;
            let out = with_source(p, |src| {
                let bump = bumpalo::Bump::new();
                quiver::parser::parse_module(src, &bump)
                    .map(|m| quiver::dump::dump_module(&m.module))
                    .map_err(|e| e.msg)
            })??;
            println!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    // deep (but bounded) recursion in parser/formatter: give workers roomy stacks
    let _ = rayon::ThreadPoolBuilder::new().stack_size(16 << 20).build_global();
    match real_main() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
