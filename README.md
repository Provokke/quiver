<div align="center">

# quiver

A fast Python linter, formatter and import sorter written from scratch in Rust. No Python parser crates: quiver has its own zero-copy lexer, recursive-descent parser, arena-allocated AST, semantic model, rule engine and formatter.

![License](https://img.shields.io/badge/LICENSE-MIT-blue?style=for-the-badge)  ![Language](https://img.shields.io/badge/RUST-dea584?style=for-the-badge&logo=rust&logoColor=white)

Refer to [LICENSE](LICENSE) for the legal bits.

</div>

## 🏗️ Architecture

```
          files (.gitignore-aware walk, rayon par_iter, mmap)
                              |
                              v
  +-----------+   tokens (&str spans into the mmap)   +-------------------------+
  |  lexer.rs | ------------------------------------> |  parser.rs              |
  | INDENT/   |   comments (side list)                |  recursive descent +    |
  | DEDENT,   |                                       |  precedence levels,     |
  | f-strings |                                       |  f-string sub-parsing   |
  +-----------+                                       +-----------+-------------+
                                                                  | &'a AST in bumpalo arena
                    +---------------------+-----------------------+---------------------+
                    v                     v                       v                     v
            checker.rs             physical.rs              isort.rs              formatter.rs
    scopes, bindings, deferred   line/token rules       sections, sorting      AST printer + comment
    function bodies, imports     (E501, W29x, E70x)     wrapping (I001)        re-attachment, line
    usage, AST rules (F/E/B/T)                                                  splitting
                    \                     |                       /
                     +---------> linter.rs: noqa filtering, fix loop  <---- rules.rs
                                 (apply non-overlapping edits,             define_rules! macro
                                  re-lint up to 10x, never emit            registry (code, name,
                                  unparseable code)                        fixable, description)
                                          |
                                          v
                                 main.rs: text / json / github output
```

| module | role |
|---|---|
| `lexer.rs` | Byte-level tokenizer; tokens hold a `Span` and a `&str` into the source. Handles indentation stack, implicit line joining, backslash continuations, all numeric/string literal forms (prefixes, triple quotes, bytes, raw) and PEP 701 f-strings (nested quotes, nested replacement fields, format specs, `=`/`!r`). |
| `parser.rs` | Python 3.12/3.13 grammar: all statements incl. `match` (soft keyword with backtracking), `type` aliases, PEP 695 type params, `except*`, parenthesized `with`, decorators, async, walrus, lambda, comprehensions, star-expressions, slices. Depth-limited (no stack overflow). |
| `ast.rs` | Arena AST (`bumpalo`): `&'a Expr<'a>`, slices `&'a [Stmt<'a>]`, identifiers borrow the source. |
| `dump.rs` | Span-free canonical dump used to compare ASTs (formatter verification). |
| `checker.rs` | Semantic model: module/class/function/comprehension scopes, bindings, pyflakes-style deferred function bodies, branch "forks" (for redefinitions in `if`/`try`), `global`, `__all__`, `del`, `except ... as`, submodule imports. |
| `rules.rs` | `define_rules!` declarative registry generating the `Rule` enum and tables. |
| `formatter.rs` | Re-prints statements from the AST with minimal parentheses by precedence, normalizes quotes, blank lines, splits long lines at the outermost bracket (magic trailing comma style) or wraps boolean/binary chains in parens. Statements that contain interior comments are emitted verbatim (logical lines re-indented) — this is what makes it safe. |

## 🚀 Quick start

```
quiver check [PATHS...] [--fix] [--output-format text|json|github] [--select CODES] [--ignore CODES]
             [--line-length N] [--statistics] [--no-config]
quiver format [PATHS...] [--check] [--line-length N]
quiver rules                     # list all rules
quiver parse-stats [PATHS...]    # parse every file, report % success and failures
quiver verify-format [PATHS...]  # format -> reparse -> compare AST -> format again (idempotency)
quiver dump-ast FILE
```

`pyproject.toml`:

```toml
[tool.quiver]
line-length = 100
select = ["E", "F", "W", "B"]   # code prefixes; "ALL" selects everything
ignore = ["E501"]
exclude = ["build/*"]
known-first-party = ["myproject"]
```

Default selection is every rule except `T201`/`T203`. `# noqa` and `# noqa: F401,E5` are honored.

### Sample output (real)

```
$ quiver check --no-config sample.py
sample.py:3:8: F401 `sys` imported but unused [*]
sample.py:3:13: F401 `os` imported but unused [*]
sample.py:4:8: F401 `json` imported but unused [*]
sample.py:5:20: F401 `typing.List` imported but unused [*]
sample.py:8:12: B006 Do not use mutable data structures for argument defaults
sample.py:10:5: F841 Local variable `x` is assigned to but never used
sample.py:11:13: E711 Comparison to `None` should be `cond is None` [*]
sample.py:12:15: F541 f-string without any placeholders [*]
sample.py:13:10: E713 Test for membership should be `not in` [*]
sample.py:16:17: F601 Dictionary key `'a'` repeated
sample.py:16:25: F601 Dictionary key `'a'` repeated
sample.py:18:9: F841 Local variable `y` is assigned to but never used
sample.py:19:5: E722 Do not use bare `except`
sample.py:21:5: F841 Local variable `l` is assigned to but never used
sample.py:21:5: E731 Do not assign a `lambda` expression, use a `def`
sample.py:21:5: E741 Ambiguous variable name: `l`
sample.py:23:5: F821 Undefined name `unreachable`
sample.py:23:5: PLW0101 Unreachable code
sample.py:24:10: E701 Multiple statements on one line (colon)
Found 19 errors in 1 files.
[*] 7 fixable with `quiver check --fix`.
```

## Rules

`quiver rules` prints the full table. Implemented: E999, F401*, F403, F541*, F601, F631, F632*, F701, F702, F704,
F706, F811, F821, F822, F841, F901*, E402, E501, E701, E702, E703*, E711*, E712, E713*, E714*, E721, E722, E731, E741,
W191, W291*, W292*, W293*, W605, B006, B011*, B015, B018, T201, T203, PLW0101, I001* (`*` = autofix).

## 🧪 Testing

One command runs the core suite (55 tests, all passing):

```
cargo test --workspace
```

| suite | what it checks |
|---|---|
| `src/*` unit tests (18) | lexer (indent/dedent, all literal forms, f-string structure, continuation lines, errors), rule registry consistency, selectors, config parsing |
| `tests/parser.rs` (10) | precedence/associativity, trailers, comprehensions, f-strings, every statement kind, `match`, type params, byte-accurate spans, 12 syntax-error cases, deep nesting (no stack overflow) |
| `tests/rules.rs` (7) | **per-rule snapshot tests**: each of the 42 rules has `tests/fixtures/rules/<CODE>.py` with positive lines (marked `# ERR`) and negative lines; the test asserts diagnostics land exactly on marked lines and compares output + `--fix` result with `tests/snapshots/<CODE>.snap` (`UPDATE_SNAPSHOTS=1` to regenerate). Also: fixes produce parseable code, noqa, config select/ignore, `__init__.py` handling, E999 |
| `tests/formatter.rs` (7) | exact expected output, required parentheses kept, blank lines, comment preservation, line splitting, compound statements, idempotency + AST equality on all fixtures |
| `tests/fuzz.rs` (6) | proptest: lexer/parser/linter/formatter never panic on token soup (2000 cases), arbitrary strings and bytes; generated programs round-trip through the formatter with identical AST and idempotency (500 cases); `--fix` never breaks parsing; a guard that >50% of generated programs parse (247/300) |
| `tests/stdlib.rs` (1) | parses, lints and format-round-trips every file of the local CPython stdlib |
| `tests/cli.rs` (6) | end-to-end binary: exit codes, text/json/github output, `--fix` rewrites, `format --check`, pyproject config, `.gitignore` respected |

### Corpus results (measured on this machine)

| corpus | files | parse success | formatter round-trip (AST-equal + idempotent + same comment count) |
|---|---|---|---|
| CPython 3.13 stdlib (`/usr/local/lib/python3.13`, no site-packages) | 540 | **540/540 (100%)** | 540/540 |
| CPython 3.13 stdlib **+ dist-packages** (`.py` + `.pyi`) | 11,328 | **11,328/11,328 (100%)** | 11,328/11,328 |
| CPython 3.12 stdlib (`/usr/lib/python3.12`) | 572 | 572/572 | 572/572 |
| CPython 3.11 stdlib (`/usr/lib/python3.11`) | 670 | 670/670 | – |

Cross-check against CPython's own parser (`scripts/verify_format_with_python_ast.py`): for 5,421 files
(all parseable `.py` under `/usr/local/lib/python3.13`, incl. dist-packages), `ast.dump(ast.parse(original)) ==
ast.dump(ast.parse(quiver_formatted))` holds for **5,421/5,421**. Since the formatter prints parentheses purely from
quiver's own precedence model, this also validates the parser's tree shape.

Note: this Python install ships without the `test/` package, so "the stdlib" here is 540 files / 278k lines.

### Differential testing vs pyflakes 4.1.0 / pycodestyle 2.15 (`scripts/differential.py`, stdlib, by (file, line, code))

| tool | both report | quiver only | other only | agreement |
|---|---|---|---|---|
| pyflakes (F401, F403, F541, F601, F631, F632, F70x, F811, F821, F822, F841, F901) | 412 | 29 | 27 | 88.0% |
| pycodestyle (E402, E501, E70x, E71x, E72x, E731, E741, W191, W29x, W605) | 3,786 | 13 | 0 | 99.7% |

Per rule: F821 100% (164/164), F403/F541/F601 100%, F841 89%, F401 67%, F811 50%, E501 99.9%, E701/E702/E722/E731/E402 100%.
Known remaining differences: pyflakes reports unused `from x import *` as F401 and some submodule-import
redefinitions as F811 that quiver does not; quiver flags `not a[0] in b` (E713) and `l` in loop targets (E741) where
pycodestyle's regexes do not.

### Benchmarks (hyperfine, 4-core shared VM, `--no-cache`, real numbers)

Corpus: CPython 3.13 stdlib, 540 files / 278k lines.

| command | mean |
|---|---|
| `quiver check` (all default rules) | **91.9 ms ± 3.4** |
| `quiver check --select F` | 81.9 ms ± 1.6 |
| `ruff check --select E,F,W` (ruff 0.16.9) | 162.4 ms ± 12.2 |
| `pyflakes` 4.1.0 | 2,829 ms ± 88 |
| `pycodestyle` 2.15 | 9,365 ms (1 run) |
| `quiver parse-stats` (parse only) | 48.7 ms ± 3.9 |
| `quiver format --check` | 81.1 ms ± 7.1 |
| `ruff format --check` | 733.1 ms ± 37.7 |

Larger corpus (all 5,421 `.py` files, 2.24M lines, stdlib + dist-packages): `quiver check` 1.065 s ± 0.029,
`ruff check --select E,F,W` 1.410 s ± 0.015.

Caveat: these are not apples-to-apples — ruff implements many more rules and checks per rule, and `ruff format` is a
complete Black-compatible formatter whereas quiver's formatter is a smaller style subset. pyflakes/pycodestyle are
single-threaded Python. Machine was shared with other jobs.

### Other checks

* `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`: clean.
* CI (`.github/workflows/ci.yml`) runs all of the above plus the stdlib, CPython-ast and differential scripts.

## ⚠️ Honest limits

* Formatter is a Black-*like* subset, not Black-compatible: no magic-trailing-comma detection from the source, no
  string-prefix normalization, no backslash/`return (...)` paren management, no splitting of long subscripts/lambdas
  beyond the outermost bracket, comments are never reflowed. Statements with comments *inside* them are kept verbatim
  (only re-indented), so such code is not restyled. Explicit parentheses around tuples are kept as written.
* Parser accepts some programs CPython rejects (it does not validate assignment targets, `nonlocal` placement,
  `await` outside async, etc.) and stops at the first syntax error (no error recovery).
* f-string support follows PEP 701 for nesting, but string literal escapes are not decoded (string values are
  compared by source text); `t`-strings (3.14) are not supported.
* Semantic model approximations: no flow analysis (e.g. use-before-assignment), string annotations are not parsed,
  `from __future__ import annotations` defers all annotations, conditional imports are handled via a simple fork model.
* No caching; no `cargo-fuzz` target (proptest-based fuzzing only; cargo-fuzz needs nightly).
* Import sorter skips blocks that contain comments or `;`, and only sorts top-level import blocks.
* Fixes are applied only where they are safe and local; F401 fixes are not offered in `__init__.py`.
