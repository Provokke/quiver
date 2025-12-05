//! Declarative rule registry. Every rule is declared once in
//! [`define_rules!`]; the macro generates the `Rule` enum plus lookup tables.

use crate::span::Span;

macro_rules! define_rules {
    ($( $variant:ident => $code:literal, $name:literal, $fix:literal, $desc:literal; )*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Rule { $($variant),* }

        impl Rule {
            pub const ALL: &'static [Rule] = &[$(Rule::$variant),*];
            pub fn code(self) -> &'static str {
                match self { $(Rule::$variant => $code),* }
            }
            pub fn name(self) -> &'static str {
                match self { $(Rule::$variant => $name),* }
            }
            pub fn fixable(self) -> bool {
                match self { $(Rule::$variant => $fix),* }
            }
            pub fn description(self) -> &'static str {
                match self { $(Rule::$variant => $desc),* }
            }
            pub fn from_code(code: &str) -> Option<Rule> {
                match code { $($code => Some(Rule::$variant),)* _ => None }
            }
        }
    };
}

define_rules! {
    SyntaxError => "E999", "syntax-error", false, "File could not be parsed";
    UnusedImport => "F401", "unused-import", true, "Imported name is never used";
    ImportStarUsed => "F403", "import-star", false, "`from module import *` used";
    RedefinedWhileUnused => "F811", "redefined-while-unused", false, "Redefinition of an unused name";
    UndefinedName => "F821", "undefined-name", false, "Name is not defined";
    UndefinedExport => "F822", "undefined-export", false, "Name in `__all__` is not defined";
    UnusedVariable => "F841", "unused-variable", false, "Local variable is assigned but never used";
    FStringMissingPlaceholders => "F541", "f-string-missing-placeholders", true, "f-string has no placeholders";
    MultiValueRepeatedKey => "F601", "multi-value-repeated-key", false, "Dictionary key repeated with different values";
    AssertTuple => "F631", "assert-tuple", false, "Assertion on a non-empty tuple is always true";
    IsLiteral => "F632", "is-literal", true, "Use `==`/`!=` to compare constant literals";
    BreakOutsideLoop => "F701", "break-outside-loop", false, "`break` outside loop";
    ContinueOutsideLoop => "F702", "continue-outside-loop", false, "`continue` outside loop";
    YieldOutsideFunction => "F704", "yield-outside-function", false, "`yield` outside function";
    ReturnOutsideFunction => "F706", "return-outside-function", false, "`return` outside function";
    RaiseNotImplemented => "F901", "raise-not-implemented", true, "`raise NotImplemented` should be `raise NotImplementedError`";
    ModuleImportNotAtTop => "E402", "module-import-not-at-top-of-file", false, "Module level import not at top of file";
    LineTooLong => "E501", "line-too-long", false, "Line too long";
    MultipleStatementsColon => "E701", "multiple-statements-on-one-line-colon", false, "Multiple statements on one line (colon)";
    MultipleStatementsSemicolon => "E702", "multiple-statements-on-one-line-semicolon", false, "Multiple statements on one line (semicolon)";
    UselessSemicolon => "E703", "useless-semicolon", true, "Statement ends with an unnecessary semicolon";
    NoneComparison => "E711", "none-comparison", true, "Comparison to `None` should use `is`/`is not`";
    TrueFalseComparison => "E712", "true-false-comparison", false, "Avoid equality comparisons to `True`/`False`";
    NotInTest => "E713", "not-in-test", true, "Test for membership should be `not in`";
    NotIsTest => "E714", "not-is-test", true, "Test for object identity should be `is not`";
    TypeComparison => "E721", "type-comparison", false, "Do not compare types, use `isinstance()`";
    BareExcept => "E722", "bare-except", false, "Do not use bare `except`";
    LambdaAssignment => "E731", "lambda-assignment", false, "Do not assign a `lambda` expression, use a `def`";
    AmbiguousVariableName => "E741", "ambiguous-variable-name", false, "Ambiguous variable name";
    TabIndentation => "W191", "tab-indentation", false, "Indentation contains tabs";
    TrailingWhitespace => "W291", "trailing-whitespace", true, "Trailing whitespace";
    MissingNewlineAtEof => "W292", "missing-newline-at-end-of-file", true, "No newline at end of file";
    BlankLineWithWhitespace => "W293", "blank-line-with-whitespace", true, "Blank line contains whitespace";
    InvalidEscapeSequence => "W605", "invalid-escape-sequence", false, "Invalid escape sequence";
    MutableArgumentDefault => "B006", "mutable-argument-default", false, "Do not use mutable data structures for argument defaults";
    AssertFalse => "B011", "assert-false", true, "Do not `assert False`, raise `AssertionError()`";
    UselessComparison => "B015", "useless-comparison", false, "Pointless comparison; did you mean to assign or assert?";
    UselessExpression => "B018", "useless-expression", false, "Found useless expression";
    Print => "T201", "print", false, "`print` found";
    PPrint => "T203", "p-print", false, "`pprint` found";
    UnreachableCode => "PLW0101", "unreachable-code", false, "Unreachable code after `return`/`raise`/`break`/`continue`";
    UnsortedImports => "I001", "unsorted-imports", true, "Import block is unsorted or unformatted";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: u32,
    pub end: u32,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub edits: Vec<Edit>,
}

impl Fix {
    pub fn replace(span: Span, content: impl Into<String>) -> Fix {
        Fix {
            edits: vec![Edit {
                start: span.start,
                end: span.end,
                content: content.into(),
            }],
        }
    }
    pub fn delete(span: Span) -> Fix {
        Fix::replace(span, "")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub rule: Rule,
    pub span: Span,
    pub message: String,
    pub fix: Option<Fix>,
}

impl Diagnostic {
    pub fn new(rule: Rule, span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            rule,
            span,
            message: message.into(),
            fix: None,
        }
    }
    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }
}

/// Which rules are enabled, from `select`/`ignore` code prefixes.
#[derive(Debug, Clone)]
pub struct RuleSet {
    enabled: Vec<bool>,
}

impl RuleSet {
    pub fn from_selectors(select: &[String], ignore: &[String]) -> RuleSet {
        let matches = |r: Rule, sel: &str| sel == "ALL" || r.code().starts_with(sel);
        let enabled = Rule::ALL
            .iter()
            .map(|&r| {
                let selected = if select.is_empty() {
                    r != Rule::Print && r != Rule::PPrint
                } else {
                    select.iter().any(|s| matches(r, s))
                };
                selected && !ignore.iter().any(|s| matches(r, s)) || r == Rule::SyntaxError
            })
            .collect();
        RuleSet { enabled }
    }
    pub fn all() -> RuleSet {
        RuleSet {
            enabled: vec![true; Rule::ALL.len()],
        }
    }
    pub fn only(rules: &[Rule]) -> RuleSet {
        let mut enabled = vec![false; Rule::ALL.len()];
        for r in rules {
            enabled[*r as usize] = true;
        }
        RuleSet { enabled }
    }
    pub fn enabled(&self, r: Rule) -> bool {
        self.enabled[r as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        assert!(Rule::ALL.len() >= 25);
        let mut codes: Vec<_> = Rule::ALL.iter().map(|r| r.code()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), Rule::ALL.len(), "duplicate rule codes");
        for r in Rule::ALL {
            assert_eq!(Rule::from_code(r.code()), Some(*r));
        }
    }

    #[test]
    fn selectors() {
        let rs = RuleSet::from_selectors(&["F".into()], &["F401".into()]);
        assert!(rs.enabled(Rule::UndefinedName));
        assert!(!rs.enabled(Rule::UnusedImport));
        assert!(!rs.enabled(Rule::LineTooLong));
        let d = RuleSet::from_selectors(&[], &[]);
        assert!(!d.enabled(Rule::Print));
        assert!(d.enabled(Rule::LineTooLong));
    }
}
