//! Formatter tests: exact expected output for representative inputs, plus
//! idempotency and AST preservation on every rule fixture.

use bumpalo::Bump;
use quiver::dump::dump_module;
use quiver::formatter::format_source;
use quiver::parser::parse_module;

fn fmt(src: &str) -> String {
    format_source(src, 88).unwrap()
}

fn check_invariants(src: &str) {
    let once = format_source(src, 88).unwrap();
    let twice = format_source(&once, 88).unwrap();
    assert_eq!(once, twice, "not idempotent for:\n{src}");
    let (b1, b2) = (Bump::new(), Bump::new());
    let a = dump_module(&parse_module(src, &b1).unwrap().module);
    let b = dump_module(&parse_module(&once, &b2).unwrap().module);
    assert_eq!(a, b, "AST changed for:\n{src}\n---\n{once}");
}

#[test]
fn normalizes_spacing_and_quotes() {
    assert_eq!(
        fmt("x=1+2*  3\ny = {'a':1,'b' :[1,2,]}\n"),
        "x = 1 + 2 * 3\ny = {\"a\": 1, \"b\": [1, 2]}\n"
    );
    assert_eq!(fmt("s = 'it\"s'\n"), "s = 'it\"s'\n");
    assert_eq!(fmt("print( a , b )\n"), "print(a, b)\n");
    assert_eq!(fmt("x = a**2 + b ** -c\n"), "x = a**2 + b**-c\n");
    assert_eq!(fmt("x = (yield)\n"), "x = yield\n");
    assert_eq!(
        fmt("def f(a, *, b=1, **kw): return (a)\n"),
        "def f(a, *, b=1, **kw):\n    return a\n"
    );
}

#[test]
fn keeps_required_parentheses() {
    assert_eq!(fmt("x = (a + b) * c\n"), "x = (a + b) * c\n");
    assert_eq!(fmt("x = (-1) ** 2\n"), "x = (-1)**2\n");
    assert_eq!(fmt("x = (1).real\n"), "x = (1).real\n");
    assert_eq!(fmt("x = (a if b else c).d\n"), "x = (a if b else c).d\n");
    assert_eq!(fmt("x = not (a == b)\n"), "x = not a == b\n");
    assert_eq!(
        fmt("x = (a, b)\nfor (i, j) in z: pass\n"),
        "x = (a, b)\nfor (i, j) in z:\n    pass\n"
    );
    assert_eq!(fmt("f((x for x in y), 1)\n"), "f((x for x in y), 1)\n");
    assert_eq!(
        fmt("def g():\n    return (yield x)\n"),
        "def g():\n    return (yield x)\n"
    );
}

#[test]
fn blank_lines() {
    assert_eq!(
        fmt("import os\ndef f():\n    pass\nx = 1\n"),
        "import os\n\n\ndef f():\n    pass\n\n\nx = 1\n"
    );
    assert_eq!(fmt("x = 1\n\n\n\n\ny = 2\n"), "x = 1\n\n\ny = 2\n");
    assert_eq!(
        fmt("class C:\n\n\n    a = 1\n    def m(self): pass\n"),
        "class C:\n    a = 1\n\n    def m(self):\n        pass\n"
    );
}

#[test]
fn comments_are_preserved() {
    let src = "# header\nimport os  # trailing\n\n# before f\ndef f(  # inside header\n    a,\n):\n    # in body\n    x = [1,  # inside expr\n         2]\n    return x\n    # end of body\n# dedented\n";
    let out = fmt(src);
    for c in [
        "# header",
        "# trailing",
        "# before f",
        "# inside header",
        "# in body",
        "# inside expr",
        "# end of body",
        "# dedented",
    ] {
        assert!(out.contains(c), "lost {c}:\n{out}");
    }
    check_invariants(src);
    let src = "if a:\n    pass\n    # trailing in if\n# before else\nelse:  # on else\n    pass\n";
    let out = fmt(src);
    assert_eq!(
        out,
        "if a:\n    pass\n    # trailing in if\n# before else\nelse:  # on else\n    pass\n"
    );
}

#[test]
fn splits_long_lines() {
    let src = "result = some_function_name(argument_number_one, argument_number_two, argument_number_three)\n";
    assert_eq!(
        fmt(src),
        "result = some_function_name(\n    argument_number_one,\n    argument_number_two,\n    argument_number_three,\n)\n"
    );
    let src =
        "def function_with_long_signature(parameter_one, parameter_two, parameter_three, *args, **kw):\n    pass\n";
    assert_eq!(
        fmt(src),
        "def function_with_long_signature(\n    parameter_one,\n    parameter_two,\n    parameter_three,\n    *args,\n    **kw,\n):\n    pass\n"
    );
    let src = "if some_condition_variable and another_condition_variable or yet_another_condition_var_x:\n    pass\n";
    assert_eq!(
        fmt(src),
        "if (\n    some_condition_variable\n    and another_condition_variable\n    or yet_another_condition_var_x\n):\n    pass\n"
            .replace("    some_condition_variable\n    and another_condition_variable\n    or", "    some_condition_variable and another_condition_variable\n    or")
    );
    check_invariants(src);
}

#[test]
fn compound_statements() {
    let src = "try:\n  x=1\nexcept (A,B) as e:\n  pass\nelse:\n  y=2\nfinally:\n  z=3\nwith open(f) as g, open(h): pass\nwhile x: x -= 1\nmatch p:\n  case [1, *r] if r: pass\n  case {'k': v}: pass\n  case C(a, b=2) | None: pass\n  case _: pass\n";
    check_invariants(src);
    let out = fmt(src);
    assert!(out.contains("except (A, B) as e:\n"));
    assert!(out.contains("    case [1, *r] if r:\n        pass\n"));
}

#[test]
fn fixtures_are_idempotent_and_ast_preserving() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rules");
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let src = std::fs::read_to_string(&p).unwrap();
        let b = Bump::new();
        if parse_module(&src, &b).is_err() {
            continue;
        }
        check_invariants(&src);
        n += 1;
    }
    assert!(n > 30);
}
