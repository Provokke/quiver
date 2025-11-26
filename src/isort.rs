//! isort-style import sorting: sections (future / stdlib / third-party /
//! first-party / local), straight imports before `from` imports, names
//! ordered constants → classes → functions, long lines wrapped vertically.

use crate::ast::*;
use crate::data::is_stdlib;
use crate::rules::{Diagnostic, Fix, Rule};
use crate::span::Span;

#[derive(Debug, Clone, Default)]
pub struct IsortSettings {
    pub known_first_party: Vec<String>,
    pub line_length: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Section {
    Future,
    Stdlib,
    ThirdParty,
    FirstParty,
    Local,
}

fn section(module: &str, level: u32, s: &IsortSettings) -> Section {
    if level > 0 {
        return Section::Local;
    }
    let root = module.split('.').next().unwrap_or("").trim();
    if root == "__future__" {
        Section::Future
    } else if s.known_first_party.iter().any(|k| k == root) {
        Section::FirstParty
    } else if is_stdlib(root) {
        Section::Stdlib
    } else {
        Section::ThirdParty
    }
}

fn module_key(m: &str) -> (String, String) {
    (m.to_ascii_lowercase(), m.to_string())
}

fn name_key(n: &str) -> (u8, String, String) {
    let ty = if n.len() > 1 && n.chars().all(|c| !c.is_ascii_lowercase()) {
        0
    } else if n.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        1
    } else {
        2
    };
    (ty, n.to_ascii_lowercase(), n.to_string())
}

struct Straight {
    module: String,
    asname: Option<String>,
}

struct From {
    module: String,
    level: u32,
    names: Vec<(String, Option<String>)>,
}

/// Generates the sorted text for one block of import statements.
pub fn sort_block(src: &str, block: &[&Stmt], s: &IsortSettings) -> String {
    let mut straight: Vec<(Section, Straight)> = Vec::new();
    let mut froms: Vec<(Section, From)> = Vec::new();
    for st in block {
        match &st.kind {
            StmtKind::Import(names) => {
                for a in *names {
                    let m: String = a.name.name.split_whitespace().collect();
                    straight.push((
                        section(&m, 0, s),
                        Straight {
                            module: m,
                            asname: a.asname.map(|x| x.name.to_string()),
                        },
                    ));
                }
            }
            StmtKind::ImportFrom { module, level, names } => {
                let m: String = module.map(|m| m.name.split_whitespace().collect()).unwrap_or_default();
                let sec = section(&m, *level, s);
                for a in *names {
                    let entry = (a.name.name.to_string(), a.asname.map(|x| x.name.to_string()));
                    let aliased = entry.1.is_some();
                    // merge plain names into an existing statement of the same module
                    if !aliased
                        && let Some((_, f)) = froms
                            .iter_mut()
                            .find(|(_, f)| f.module == m && f.level == *level && f.names.iter().all(|n| n.1.is_none()))
                    {
                        if !f.names.contains(&entry) {
                            f.names.push(entry);
                        }
                        continue;
                    }
                    if aliased
                        && froms
                            .iter()
                            .any(|(_, f)| f.module == m && f.level == *level && f.names.contains(&entry))
                    {
                        continue;
                    }
                    froms.push((
                        sec,
                        From {
                            module: m.clone(),
                            level: *level,
                            names: vec![entry],
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    let _ = src;
    straight
        .sort_by(|a, b| (a.0, module_key(&a.1.module), &a.1.asname).cmp(&(b.0, module_key(&b.1.module), &b.1.asname)));
    straight.dedup_by(|a, b| a.1.module == b.1.module && a.1.asname == b.1.asname);
    for (_, f) in froms.iter_mut() {
        f.names
            .sort_by(|a, b| (name_key(&a.0), &a.1).cmp(&(name_key(&b.0), &b.1)));
    }
    // relative imports: deeper levels first (isort's default "reverse_relative" = false places `.` before `..`)
    froms.sort_by(|a, b| {
        (
            a.0,
            std::cmp::Reverse(a.1.level),
            module_key(&a.1.module),
            name_key(&a.1.names[0].0),
        )
            .cmp(&(
                b.0,
                std::cmp::Reverse(b.1.level),
                module_key(&b.1.module),
                name_key(&b.1.names[0].0),
            ))
    });
    let mut out = String::new();
    let mut sections: Vec<Section> = straight.iter().map(|x| x.0).chain(froms.iter().map(|x| x.0)).collect();
    sections.sort();
    sections.dedup();
    for (i, sec) in sections.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for (_, st) in straight.iter().filter(|x| x.0 == *sec) {
            out.push_str("import ");
            out.push_str(&st.module);
            if let Some(a) = &st.asname {
                out.push_str(" as ");
                out.push_str(a);
            }
            out.push('\n');
        }
        for (_, f) in froms.iter().filter(|x| x.0 == *sec) {
            let head = format!("from {}{} import ", ".".repeat(f.level as usize), f.module);
            let names: Vec<String> = f
                .names
                .iter()
                .map(|(n, a)| match a {
                    Some(a) => format!("{n} as {a}"),
                    None => n.clone(),
                })
                .collect();
            let one = format!("{head}{}", names.join(", "));
            if one.chars().count() <= s.line_length || names.len() == 1 && names[0] == "*" {
                out.push_str(&one);
                out.push('\n');
            } else {
                out.push_str(&head);
                out.push_str("(\n");
                for n in &names {
                    out.push_str("    ");
                    out.push_str(n);
                    out.push_str(",\n");
                }
                out.push_str(")\n");
            }
        }
    }
    out
}

/// Finds top-level import blocks that are not sorted and reports I001 with a fix.
pub fn check_imports(src: &str, body: &[Stmt], comments: &[Span], s: &IsortSettings, out: &mut Vec<Diagnostic>) {
    let mut i = 0;
    while i < body.len() {
        if !is_import(&body[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < body.len() && is_import(&body[j + 1]) {
            j += 1;
        }
        let block: Vec<&Stmt> = body[i..=j].iter().collect();
        i = j + 1;
        let start = block[0].span.start;
        let end = block[block.len() - 1].span.end;
        // conservative: skip blocks with comments, semicolons or that share lines with other code
        if comments
            .iter()
            .any(|c| c.start >= start && c.start <= line_end(src, end))
        {
            continue;
        }
        let line_start = src[..start as usize].rfind('\n').map(|p| p + 1).unwrap_or(0);
        if !src[line_start..start as usize].trim().is_empty() {
            continue;
        }
        let le = line_end(src, end);
        if !src[end as usize..le as usize].trim().is_empty() {
            continue;
        }
        let original = &src[start as usize..le as usize];
        if original.contains(';')
            && block
                .windows(2)
                .any(|w| !src[w[0].span.end as usize..w[1].span.start as usize].contains('\n'))
        {
            continue;
        }
        let mut sorted = sort_block(src, &block, s);
        // keep the original line ending of the last line
        let span = Span::new(start, le);
        let orig_with_nl = if (le as usize) < src.len() {
            &src[start as usize..le as usize + 1]
        } else {
            original
        };
        if !orig_with_nl.ends_with('\n') {
            sorted.pop();
        }
        if orig_with_nl != sorted {
            let fix_span = Span::new(start, start + orig_with_nl.len() as u32);
            out.push(
                Diagnostic::new(Rule::UnsortedImports, span, "Import block is un-sorted or un-formatted")
                    .with_fix(Fix::replace(fix_span, sorted)),
            );
        }
    }
}

fn is_import(s: &Stmt) -> bool {
    matches!(s.kind, StmtKind::Import(_) | StmtKind::ImportFrom { .. })
}

fn line_end(src: &str, pos: u32) -> u32 {
    src[pos as usize..]
        .find('\n')
        .map(|p| pos + p as u32)
        .unwrap_or(src.len() as u32)
}
