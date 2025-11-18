//! Settings, loaded from `[tool.quiver]` in `pyproject.toml`.
//!
//! A tiny TOML subset parser is used: `key = "str" | int | bool | [array of strings]`
//! (arrays may span lines). That is all the configuration needs.

use crate::isort::IsortSettings;
use crate::rules::RuleSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Settings {
    pub line_length: usize,
    pub select: Vec<String>,
    pub ignore: Vec<String>,
    pub exclude: Vec<String>,
    pub rules: RuleSet,
    pub isort: IsortSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::build(88, vec![], vec![], vec![], vec![])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<String>),
}

impl Settings {
    pub fn build(
        line_length: usize,
        select: Vec<String>,
        ignore: Vec<String>,
        exclude: Vec<String>,
        first_party: Vec<String>,
    ) -> Settings {
        let rules = RuleSet::from_selectors(&select, &ignore);
        Settings {
            line_length,
            select,
            ignore,
            exclude,
            rules,
            isort: IsortSettings {
                known_first_party: first_party,
                line_length,
            },
        }
    }

    pub fn from_pyproject(text: &str) -> Result<Settings, String> {
        let table = parse_tool_table(text, "tool.quiver")?;
        let mut ll = 88;
        let (mut sel, mut ign, mut exc, mut fp) = (vec![], vec![], vec![], vec![]);
        for (k, v) in table {
            match (k.as_str(), v) {
                ("line-length", Value::Int(n)) if n > 0 => ll = n as usize,
                ("select", Value::Array(a)) => sel = a,
                ("ignore", Value::Array(a)) => ign = a,
                ("exclude", Value::Array(a)) => exc = a,
                ("known-first-party", Value::Array(a)) => fp = a,
                (k, v) => return Err(format!("unknown or invalid setting `{k}` = {v:?}")),
            }
        }
        Ok(Settings::build(ll, sel, ign, exc, fp))
    }

    /// Looks for `pyproject.toml` in `start` and its ancestors.
    pub fn discover(start: &Path) -> Result<(Settings, Option<PathBuf>), String> {
        let mut dir = if start.is_file() {
            start.parent().map(Path::to_path_buf)
        } else {
            Some(start.to_path_buf())
        };
        while let Some(d) = dir {
            let p = d.join("pyproject.toml");
            if p.is_file() {
                let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                if text.contains("[tool.quiver]") {
                    return Ok((Settings::from_pyproject(&text)?, Some(p)));
                }
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        Ok((Settings::default(), None))
    }
}

fn parse_value(v: &str) -> Result<Value, String> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or("unterminated array")?;
        let mut out = Vec::new();
        for item in inner.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            match parse_value(item)? {
                Value::Str(s) => out.push(s),
                other => return Err(format!("expected string in array, got {other:?}")),
            }
        }
        return Ok(Value::Array(out));
    }
    if (v.starts_with('"') && v.ends_with('"') || v.starts_with('\'') && v.ends_with('\'')) && v.len() >= 2 {
        return Ok(Value::Str(v[1..v.len() - 1].to_string()));
    }
    match v {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    v.replace('_', "")
        .parse::<i64>()
        .map(Value::Int)
        .map_err(|_| format!("cannot parse value `{v}`"))
}

fn strip_comment(line: &str) -> &str {
    let mut in_str: Option<char> = None;
    for (i, c) in line.char_indices() {
        match (c, in_str) {
            ('"' | '\'', None) => in_str = Some(c),
            (q, Some(s)) if q == s => in_str = None,
            ('#', None) => return &line[..i],
            _ => {}
        }
    }
    line
}

pub fn parse_tool_table(text: &str, name: &str) -> Result<Vec<(String, Value)>, String> {
    let mut out = Vec::new();
    let mut in_table = false;
    let mut pending: Option<(String, String)> = None;
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if let Some((k, mut acc)) = pending.take() {
            acc.push_str(line);
            if line.contains(']') {
                out.push((k, parse_value(&acc)?));
            } else {
                pending = Some((k, acc));
            }
            continue;
        }
        if line.starts_with('[') {
            in_table = line == format!("[{name}]");
            continue;
        }
        if !in_table || line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once('=').ok_or_else(|| format!("invalid line `{line}`"))?;
        let k = k.trim().trim_matches('"').to_string();
        let v = v.trim();
        if v.starts_with('[') && !v.contains(']') {
            pending = Some((k, v.to_string()));
        } else {
            out.push((k, parse_value(v)?));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Rule;

    #[test]
    fn parses_tool_section() {
        let s = Settings::from_pyproject(
            "[project]\nname = \"x\"\n[tool.quiver]\nline-length = 100 # comment\nselect = [\n  \"E\",\n  \"F\", # c\n]\nignore = [\"E501\"]\n[tool.other]\nline-length = 5\n",
        )
        .unwrap();
        assert_eq!(s.line_length, 100);
        assert_eq!(s.select, vec!["E", "F"]);
        assert!(!s.rules.enabled(Rule::LineTooLong));
        assert!(s.rules.enabled(Rule::UnusedImport));
        assert!(!s.rules.enabled(Rule::MutableArgumentDefault));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Settings::from_pyproject("[tool.quiver]\nbogus = 1\n").is_err());
    }
}
