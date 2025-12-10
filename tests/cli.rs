//! End-to-end tests of the `quiver` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_quiver"))
}

fn tempdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quiver-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(p: &Path, s: &str) {
    std::fs::write(p, s).unwrap();
}

#[test]
fn check_text_output_and_exit_code() {
    let d = tempdir("text");
    write(&d.join("a.py"), "import os\n");
    write(&d.join("ok.py"), "x = 1\n");
    let out = bin().args(["check", "--no-config"]).arg(&d).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout.contains("a.py:1:8: F401 `os` imported but unused [*]"),
        "{stdout}"
    );
    assert!(stdout.contains("Found 1 error in 2 files."), "{stdout}");
    let ok = bin()
        .args(["check", "--no-config"])
        .arg(d.join("ok.py"))
        .output()
        .unwrap();
    assert_eq!(ok.status.code(), Some(0));
}

#[test]
fn json_and_github_formats() {
    let d = tempdir("fmts");
    write(&d.join("a.py"), "if x == None:\n    pass\n");
    let out = bin()
        .args(["check", "--no-config", "--output-format", "json"])
        .arg(&d)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.trim_start().starts_with('['), "{s}");
    assert!(
        s.contains("\"code\": \"E711\"") && s.contains("\"code\": \"F821\""),
        "{s}"
    );
    assert!(s.contains("\"location\": {\"row\": 1, \"column\": 9}"), "{s}");
    let out = bin()
        .args(["check", "--no-config", "--output-format", "github"])
        .arg(&d)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.lines().all(|l| l.starts_with("::error title=quiver (")), "{s}");
}

#[test]
fn fix_rewrites_files() {
    let d = tempdir("fix");
    let f = d.join("a.py");
    write(&f, "import sys\nimport os\nif os.sep == None:\n    pass\n");
    let out = bin().args(["check", "--no-config", "--fix"]).arg(&d).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("Fixed"), "{s}");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "import os\nif os.sep is None:\n    pass\n"
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn format_check_and_write() {
    let d = tempdir("format");
    let f = d.join("a.py");
    write(&f, "x=[1,2 ,3]\n");
    let out = bin()
        .args(["format", "--check", "--no-config"])
        .arg(&d)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "x=[1,2 ,3]\n");
    let out = bin().args(["format", "--no-config"]).arg(&d).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "x = [1, 2, 3]\n");
    let out = bin()
        .args(["format", "--check", "--no-config"])
        .arg(&d)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn pyproject_config_and_gitignore() {
    let d = tempdir("config");
    write(
        &d.join("pyproject.toml"),
        "[tool.quiver]\nline-length = 10\nselect = [\"E501\", \"F401\"]\nignore = [\"F401\"]\n",
    );
    write(&d.join(".gitignore"), "ignored/\n");
    std::fs::create_dir_all(d.join("ignored")).unwrap();
    write(&d.join("ignored/b.py"), "import os\nxxxxxxxxxxxxxxxxxxxx = 1\n");
    write(&d.join("a.py"), "import os\nyyyyyyyyyyyy = 1\n");
    let out = bin().arg("check").arg(&d).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("a.py:2:11: E501 Line too long (16 > 10)"), "{s}");
    assert!(!s.contains("F401"), "{s}");
    assert!(!s.contains("ignored"), "{s}");
}

#[test]
fn syntax_error_and_rules_listing() {
    let d = tempdir("syntax");
    write(&d.join("bad.py"), "def f(:\n");
    let out = bin().args(["check", "--no-config"]).arg(&d).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("E999 SyntaxError"));
    let out = bin().arg("rules").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).lines().count() >= 40);
}
