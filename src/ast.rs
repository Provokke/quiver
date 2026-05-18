//! Arena-allocated AST. All nodes live in a `bumpalo::Bump`; identifiers and
//! literal text borrow from the source buffer.

use crate::span::Span;

pub type Ex<'a> = &'a Expr<'a>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Add,
    Sub,
    Mult,
    MatMult,
    Div,
    Mod,
    Pow,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
    FloorDiv,
}

impl Operator {
    pub fn as_str(self) -> &'static str {
        match self {
            Operator::Add => "+",
            Operator::Sub => "-",
            Operator::Mult => "*",
            Operator::MatMult => "@",
            Operator::Div => "/",
            Operator::Mod => "%",
            Operator::Pow => "**",
            Operator::LShift => "<<",
            Operator::RShift => ">>",
            Operator::BitOr => "|",
            Operator::BitXor => "^",
            Operator::BitAnd => "&",
            Operator::FloorDiv => "//",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Invert,
    Not,
    UAdd,
    USub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

impl CmpOp {
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::NotEq => "!=",
            CmpOp::Lt => "<",
            CmpOp::LtE => "<=",
            CmpOp::Gt => ">",
            CmpOp::GtE => ">=",
            CmpOp::Is => "is",
            CmpOp::IsNot => "is not",
            CmpOp::In => "in",
            CmpOp::NotIn => "not in",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrKind {
    Str,
    Bytes,
    FString,
}

#[derive(Debug, Clone, Copy)]
pub struct StrLit<'a> {
    pub span: Span,
    /// full token text including prefix and quotes
    pub text: &'a str,
    pub kind: StrKind,
    /// Parsed f-string structure (empty for plain strings)
    pub fparts: &'a [FPart<'a>],
}

#[derive(Debug, Clone, Copy)]
pub enum FPart<'a> {
    Literal(Span),
    Field {
        expr: Ex<'a>,
        debug: bool,
        conversion: Option<u8>,
        spec: Option<&'a [FPart<'a>]>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum Constant<'a> {
    None,
    True,
    False,
    Ellipsis,
    Int(&'a str),
    Float(&'a str),
    Imag(&'a str),
}

#[derive(Debug, Clone, Copy)]
pub enum Arg<'a> {
    Positional(Ex<'a>),
    Starred(Ex<'a>),
    Keyword(Ident<'a>, Ex<'a>),
    DoubleStarred(Ex<'a>),
}

impl<'a> Arg<'a> {
    pub fn value(&self) -> Ex<'a> {
        match *self {
            Arg::Positional(e) | Arg::Starred(e) | Arg::Keyword(_, e) | Arg::DoubleStarred(e) => e,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ident<'a> {
    pub name: &'a str,
    pub span: Span,
}

#[derive(Debug, Clone, Copy)]
pub struct Comprehension<'a> {
    pub target: Ex<'a>,
    pub iter: Ex<'a>,
    pub ifs: &'a [Ex<'a>],
    pub is_async: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Param<'a> {
    pub name: Ident<'a>,
    pub annotation: Option<Ex<'a>>,
    pub default: Option<Ex<'a>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Parameters<'a> {
    pub posonly: &'a [Param<'a>],
    pub args: &'a [Param<'a>],
    pub vararg: Option<Param<'a>>,
    /// true if a bare `*` separator was present
    pub bare_star: bool,
    pub kwonly: &'a [Param<'a>],
    pub kwarg: Option<Param<'a>>,
}

impl<'a> Parameters<'a> {
    pub fn all(&self) -> impl Iterator<Item = &Param<'a>> {
        self.posonly
            .iter()
            .chain(self.args.iter())
            .chain(self.vararg.iter())
            .chain(self.kwonly.iter())
            .chain(self.kwarg.iter())
    }
    pub fn is_empty(&self) -> bool {
        self.posonly.is_empty()
            && self.args.is_empty()
            && self.vararg.is_none()
            && !self.bare_star
            && self.kwonly.is_empty()
            && self.kwarg.is_none()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ExprKind<'a> {
    BoolOp(BoolOp, &'a [Ex<'a>]),
    Named(Ex<'a>, Ex<'a>),
    BinOp(Ex<'a>, Operator, Ex<'a>),
    Unary(UnaryOp, Ex<'a>),
    Lambda(&'a Parameters<'a>, Ex<'a>),
    IfExp {
        test: Ex<'a>,
        body: Ex<'a>,
        orelse: Ex<'a>,
    },
    /// key `None` means `**value`
    Dict(&'a [(Option<Ex<'a>>, Ex<'a>)]),
    Set(&'a [Ex<'a>]),
    ListComp(Ex<'a>, &'a [Comprehension<'a>]),
    SetComp(Ex<'a>, &'a [Comprehension<'a>]),
    DictComp(Ex<'a>, Ex<'a>, &'a [Comprehension<'a>]),
    Generator(Ex<'a>, &'a [Comprehension<'a>]),
    Await(Ex<'a>),
    Yield(Option<Ex<'a>>),
    YieldFrom(Ex<'a>),
    Compare(Ex<'a>, &'a [(CmpOp, Ex<'a>)]),
    Call(Ex<'a>, &'a [Arg<'a>]),
    Attribute(Ex<'a>, Ident<'a>),
    Subscript(Ex<'a>, Ex<'a>),
    Starred(Ex<'a>),
    Name(&'a str),
    List(&'a [Ex<'a>]),
    Tuple {
        elts: &'a [Ex<'a>],
        parenthesized: bool,
    },
    Slice(Option<Ex<'a>>, Option<Ex<'a>>, Option<Ex<'a>>),
    Constant(Constant<'a>),
    Str(&'a [StrLit<'a>]),
}

#[derive(Debug, Clone, Copy)]
pub struct Expr<'a> {
    pub kind: ExprKind<'a>,
    pub span: Span,
    /// number of redundant parentheses directly around this expression in the source
    pub parens: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Alias<'a> {
    pub name: Ident<'a>,
    pub asname: Option<Ident<'a>>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy)]
pub struct WithItem<'a> {
    pub context: Ex<'a>,
    pub vars: Option<Ex<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ExceptHandler<'a> {
    pub typ: Option<Ex<'a>>,
    pub name: Option<Ident<'a>>,
    pub body: Body<'a>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy)]
pub enum TypeParamKind {
    TypeVar,
    TypeVarTuple,
    ParamSpec,
}

#[derive(Debug, Clone, Copy)]
pub struct TypeParam<'a> {
    pub kind: TypeParamKind,
    pub name: Ident<'a>,
    pub bound: Option<Ex<'a>>,
    pub default: Option<Ex<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub enum Pattern<'a> {
    Value(Ex<'a>),
    Singleton(Ex<'a>),
    Sequence(&'a [Pattern<'a>], Span),
    Mapping {
        keys: &'a [Ex<'a>],
        patterns: &'a [Pattern<'a>],
        rest: Option<Ident<'a>>,
        span: Span,
    },
    Class {
        cls: Ex<'a>,
        patterns: &'a [Pattern<'a>],
        kwd: &'a [(Ident<'a>, Pattern<'a>)],
        span: Span,
    },
    Star(Option<Ident<'a>>, Span),
    As(Option<&'a Pattern<'a>>, Option<Ident<'a>>, Span),
    Or(&'a [Pattern<'a>], Span),
}

#[derive(Debug, Clone, Copy)]
pub struct MatchCase<'a> {
    pub pattern: Pattern<'a>,
    pub guard: Option<Ex<'a>>,
    pub body: Body<'a>,
    pub span: Span,
}

pub type Body<'a> = &'a [Stmt<'a>];

#[derive(Debug, Clone, Copy)]
pub enum StmtKind<'a> {
    FunctionDef {
        name: Ident<'a>,
        decorators: &'a [Ex<'a>],
        type_params: &'a [TypeParam<'a>],
        params: &'a Parameters<'a>,
        returns: Option<Ex<'a>>,
        body: Body<'a>,
        is_async: bool,
    },
    ClassDef {
        name: Ident<'a>,
        decorators: &'a [Ex<'a>],
        type_params: &'a [TypeParam<'a>],
        bases: &'a [Arg<'a>],
        /// whether `()` was written even if empty
        has_parens: bool,
        body: Body<'a>,
    },
    Return(Option<Ex<'a>>),
    Delete(&'a [Ex<'a>]),
    Assign {
        targets: &'a [Ex<'a>],
        value: Ex<'a>,
    },
    TypeAlias {
        name: Ex<'a>,
        type_params: &'a [TypeParam<'a>],
        value: Ex<'a>,
    },
    AugAssign {
        target: Ex<'a>,
        op: Operator,
        value: Ex<'a>,
    },
    AnnAssign {
        target: Ex<'a>,
        annotation: Ex<'a>,
        value: Option<Ex<'a>>,
    },
    For {
        is_async: bool,
        target: Ex<'a>,
        iter: Ex<'a>,
        body: Body<'a>,
        orelse: Body<'a>,
    },
    While {
        test: Ex<'a>,
        body: Body<'a>,
        orelse: Body<'a>,
    },
    If {
        test: Ex<'a>,
        body: Body<'a>,
        orelse: Body<'a>,
        elif: bool,
    },
    With {
        is_async: bool,
        items: &'a [WithItem<'a>],
        body: Body<'a>,
    },
    Match {
        subject: Ex<'a>,
        cases: &'a [MatchCase<'a>],
    },
    Raise {
        exc: Option<Ex<'a>>,
        cause: Option<Ex<'a>>,
    },
    Try {
        body: Body<'a>,
        handlers: &'a [ExceptHandler<'a>],
        orelse: Body<'a>,
        finalbody: Body<'a>,
        star: bool,
    },
    Assert {
        test: Ex<'a>,
        msg: Option<Ex<'a>>,
    },
    Import(&'a [Alias<'a>]),
    ImportFrom {
        module: Option<Ident<'a>>,
        level: u32,
        names: &'a [Alias<'a>],
    },
    Global(&'a [Ident<'a>]),
    Nonlocal(&'a [Ident<'a>]),
    Expr(Ex<'a>),
    Pass,
    Break,
    Continue,
}

#[derive(Debug, Clone, Copy)]
pub struct Stmt<'a> {
    pub kind: StmtKind<'a>,
    pub span: Span,
}

#[derive(Debug)]
pub struct Module<'a> {
    pub body: Body<'a>,
}

impl<'a> Expr<'a> {
    pub fn is_name(&self, n: &str) -> bool {
        matches!(self.kind, ExprKind::Name(x) if x == n)
    }
}

