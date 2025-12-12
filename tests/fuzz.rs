//! Property-based fuzzing (proptest): the lexer, parser, linter and formatter
//! must never panic on arbitrary input, and for generated valid programs the
//! formatter must be idempotent and AST-preserving.

use bumpalo::Bump;
use proptest::prelude::*;
use quiver::config::Settings;
use quiver::dump::dump_module;
use quiver::formatter::format_source;
use quiver::linter::{fix_source, lint_source};
use quiver::parser::parse_module;

const FRAGMENTS: &[&str] = &[
    "def ",
    "class ",
    "if ",
    "elif ",
    "else",
    "for ",
    " in ",
    "while ",
    "try",
    "except",
    "finally",
    "with ",
    " as ",
    "lambda",
    "yield",
    "await ",
    "async ",
    "return",
    "import ",
    "from ",
    "match ",
    "case ",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ":",
    ",",
    ";",
    ".",
    "=",
    "==",
    ":=",
    "+",
    "-",
    "*",
    "**",
    "/",
    "//",
    "@",
    "->",
    "x",
    "y",
    "_",
    "1",
    "0x1f",
    "1.5e3",
    "2j",
    "'s'",
    "\"t\"",
    "f'{x}'",
    "f\"{y!r:>3}\"",
    "'''doc'''",
    "b'b'",
    "r'\\d'",
    "\n",
    "\n    ",
    "\n        ",
    " ",
    "\t",
    "\\\n",
    "# c\n",
    "not ",
    " and ",
    " or ",
    " is ",
    "None",
    "...",
];

fn soup() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..60).prop_map(|v| v.concat())
}

fn leaf() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "a", "b", "c", "1", "2.5", "'s'", "None", "True", "f'{a}'", "x.y", "-1", "()", "[]",
    ])
    .prop_map(String::from)
}

fn expr() -> impl Strategy<Value = String> {
    leaf().prop_recursive(5, 48, 4, |inner| {
        prop_oneof![
            (
                inner.clone(),
                prop::sample::select(vec![
                    "+", "-", "*", "**", "/", "|", "&", "and", "or", "<", "==", "is not", "not in", "%", "@"
                ]),
                inner.clone()
            )
                .prop_map(|(a, op, b)| format!("{a} {op} {b}")),
            inner.clone().prop_map(|a| format!("({a})")),
            inner.clone().prop_map(|a| format!("not {a}")),
            inner.clone().prop_map(|a| format!("-{a}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("f({a}, k={b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("[{a}, {b}]")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{{{a}: {b}}}")),
            (inner.clone(), inner.clone(), inner.clone()).prop_map(|(a, b, c)| format!("{a} if {b} else {c}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("[{a} for z in {b} if z]")),
            inner.clone().prop_map(|a| format!("lambda q: {a}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a}[{b}:]")),
            inner.clone().prop_map(|a| format!("{a}.attr")),
            (inner.clone(), inner).prop_map(|(a, b)| format!("({a}, {b})")),
        ]
    })
}

fn program() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            expr().prop_map(|e| format!("x = {e}\n")),
            expr().prop_map(|e| format!("if {e}:\n    y = {e}\nelse:\n    pass\n")),
            expr().prop_map(|e| format!("def f(a, b=1):\n    return {e}\n")),
            expr().prop_map(|e| format!("for i in {e}:\n    print(i)  # comment\n")),
            expr().prop_map(|e| format!("result = function_name({e}, {e}, keyword_argument={e})\n")),
        ],
        1..6,
    )
    .prop_map(|v| v.concat())
}

fn no_panic(src: &str) {
    let b = Bump::new();
    let _ = parse_module(src, &b);
    let s = Settings::default();
    let _ = lint_source(src, &s, false);
    let _ = format_source(src, 88);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, .. ProptestConfig::default() })]

    #[test]
    fn never_panics_on_token_soup(src in soup()) {
        no_panic(&src);
    }

    #[test]
    fn never_panics_on_arbitrary_strings(src in "\\PC{0,200}") {
        no_panic(&src);
    }

    #[test]
    fn never_panics_on_arbitrary_bytes_as_utf8(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        no_panic(&String::from_utf8_lossy(&bytes));
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, .. ProptestConfig::default() })]

    #[test]
    fn formatter_roundtrip_on_generated_programs(src in program()) {
        let b = Bump::new();
        let Ok(orig) = parse_module(&src, &b) else { return Ok(()) };
        let once = format_source(&src, 88).unwrap();
        let b2 = Bump::new();
        let re = parse_module(&once, &b2).unwrap_or_else(|e| panic!("formatted output fails to parse: {e:?}\n{once}"));
        prop_assert_eq!(dump_module(&orig.module), dump_module(&re.module));
        prop_assert_eq!(format_source(&once, 88).unwrap(), once);
    }

    #[test]
    fn fixes_never_break_parsing(src in program()) {
        let s = Settings::build(88, vec!["ALL".into()], vec![], vec![], vec![]);
        let r = fix_source(&src, &s, false);
        let b = Bump::new();
        prop_assert!(parse_module(&r.source, &b).is_ok() || parse_module(&src, &b).is_err());
    }
}

/// Guards against a vacuous round-trip property: most generated programs must parse.
#[test]
fn generated_programs_mostly_parse() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strat = program();
    let mut ok = 0;
    for _ in 0..300 {
        let src = strat.new_tree(&mut runner).unwrap().current();
        let b = Bump::new();
        if parse_module(&src, &b).is_ok() {
            ok += 1;
        }
    }
    eprintln!("{ok}/300 generated programs parse");
    assert!(ok > 150, "only {ok}/300 generated programs parse");
}
