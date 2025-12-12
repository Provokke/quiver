//! Parser unit tests: structure checks via the span-free dump, spans, and
//! error cases.

use bumpalo::Bump;
use quiver::ast::*;
use quiver::dump::{dump_expr, dump_module};
use quiver::parser::{parse_expression, parse_module};

fn expr(src: &str) -> String {
    let b = Bump::new();
    dump_expr(parse_expression(src, &b).unwrap_or_else(|e| panic!("{src}: {e:?}")))
}

fn module(src: &str) -> String {
    let b = Bump::new();
    dump_module(&parse_module(src, &b).unwrap_or_else(|e| panic!("{src}: {e:?}")).module)
}

fn fails(src: &str) {
    let b = Bump::new();
    assert!(parse_module(src, &b).is_err(), "expected parse error for {src:?}");
}

#[test]
fn precedence() {
    assert_eq!(expr("1 + 2 * 3"), "Bin(Add,Int(\"1\"),Bin(Mult,Int(\"2\"),Int(\"3\")))");
    assert_eq!(expr("-2 ** 2"), "Un(USub,Bin(Pow,Int(\"2\"),Int(\"2\")))");
    assert_eq!(expr("2 ** -1"), "Bin(Pow,Int(\"2\"),Un(USub,Int(\"1\")))");
    assert_eq!(expr("a or b and not c"), "Or[a,And[b,Un(Not,c),],]");
    assert_eq!(expr("a < b <= c"), "Cmp(a Lt b LtE c)");
    assert_eq!(expr("a not in b is not c"), "Cmp(a NotIn b IsNot c)");
    assert_eq!(
        expr("a | b ^ c & d << e"),
        "Bin(BitOr,a,Bin(BitXor,b,Bin(BitAnd,c,Bin(LShift,d,e))))"
    );
    assert_eq!(expr("x if y else z"), "IfExp(yxz)");
    assert_eq!(expr("a - b - c"), "Bin(Sub,Bin(Sub,a,b),c)");
    assert_eq!(
        expr("2 ** 3 ** 4"),
        "Bin(Pow,Int(\"2\"),Bin(Pow,Int(\"3\"),Int(\"4\")))"
    );
}

#[test]
fn atoms_and_trailers() {
    assert_eq!(expr("f(a, *b, c=1, **d)"), "Call(f(a,*b,c=Int(\"1\"),**d,))");
    assert_eq!(expr("a.b[c:d:e, ::2]"), "a.b[Tuple[Slice(cde),Slice(__Int(\"2\")),]]");
    assert_eq!(expr("(1,)"), "Tuple[Int(\"1\"),]");
    assert_eq!(expr("()"), "Tuple[]");
    assert_eq!(expr("((a))"), "a");
    assert_eq!(expr("[*a, b]"), "List[*a,b,]");
    assert_eq!(expr("{**a, 'b': 1}"), "Dict{_:a,Str(false\"b\" ):Int(\"1\"),}");
    assert_eq!(expr("{1, 2}"), "Set[Int(\"1\"),Int(\"2\"),]");
    assert_eq!(expr("await x"), "Await(x)");
    assert_eq!(
        expr("lambda x, *a, k=1, **kw: x"),
        "Lambda(po()a(x:_=_,)va(a:_=_,)kw(k:_=Int(\"1\"),)k(kw:_=_,)x)"
    );
    assert_eq!(expr("(y := 5)"), "Walrus(yInt(\"5\"))");
    assert_eq!(expr("'a' 'b'"), "Str(false\"a\" false\"b\" )");
    assert_eq!(expr("..."), "Ellipsis");
}

#[test]
fn comprehensions() {
    assert_eq!(expr("[x for x in y if x if z]"), "ListComp(xfor(xy[x,z,]))");
    assert_eq!(expr("{k: v for k, v in d}"), "DictComp(kvfor(Tuple[k,v,]d[]))");
    assert_eq!(expr("(x async for x in y)"), "Gen(xafor(xy[]))");
    assert_eq!(expr("f(x for x in y)"), "Call(f(Gen(xfor(xy[])),))");
    assert_eq!(expr("[x for x in a for y in b]"), "ListComp(xfor(xa[])for(yb[]))");
}

#[test]
fn fstrings() {
    let d = expr("f'a{x!r:>{w}}b{y=}'");
    assert!(d.contains("{xfalseSome(114):L\">\"{wfalseNone}}"), "{d}");
    assert!(d.contains("{ytrueNone}"), "{d}");
    let d = expr("f\"{d['k']}\" f'{a[\"x\"]}'");
    assert!(d.starts_with("Str(f["));
    let d = expr("f\"{x:{'>' if a else '<'}10}\"");
    assert!(d.contains("IfExp"), "{d}");
}

#[test]
fn statements() {
    assert_eq!(module("x = y = 1\n"), "[Assign([x,y,]Int(\"1\"));]");
    assert_eq!(module("x: int = 1\n"), "[Ann(xintInt(\"1\"));]");
    assert_eq!(module("x += 1\n"), "[Aug(Add,xInt(\"1\"));]");
    assert_eq!(module("a, *b = c\n"), "[Assign([Tuple[a,*b,],]c);]");
    assert_eq!(module("del a, b[0]\n"), "[Del[a,b[Int(\"0\")],];]");
    assert_eq!(
        module("import a.b as c, d\n"),
        "[Import(a.b as Some(\"c\"),d as None,);]"
    );
    assert_eq!(
        module("from ..x import (a, b as c,)\n"),
        "[From(2,Some(\"x\"),a as None,b as Some(\"c\"),);]"
    );
    assert_eq!(module("from . import *\n"), "[From(1,None,* as None,);]");
    assert_eq!(module("global a, b\n"), "[Global([\"a\", \"b\"]);]");
    assert_eq!(
        module("if a:\n    pass\nelif b:\n    pass\nelse:\n    pass\n"),
        "[If(a[Pass;][If(b[Pass;][Pass;]);]);]"
    );
    assert_eq!(
        module("for x, y in z:\n    break\nelse:\n    pass\n"),
        "[For(false,Tuple[x,y,]z[Break;][Pass;]);]"
    );
    assert_eq!(module("while 1: continue\n"), "[While(Int(\"1\")[Continue;][]);]");
    assert_eq!(module("x = yield\n"), "[Assign([x,]Yield(_));]");
    assert_eq!(module("a; b;\n"), "[Expr(a);Expr(b);]");
}

#[test]
fn functions_and_classes() {
    let d = module("@dec\nasync def f[T: int](a, /, b=1, *, c, **k) -> T:\n    return await a\n");
    assert!(
        d.starts_with("[Def(true,f,[dec,]tp(TypeVar,T,int_)po(a:_=_,)a(b:_=Int(\"1\"),)va()kw(c:_=_,)k(k:_=_,)T"),
        "{d}"
    );
    let d = module("class C(B, metaclass=M):\n    x = 1\n    def m(self): ...\n");
    assert_eq!(
        d,
        "[Class(C,[](B,metaclass=M,)[Assign([x,]Int(\"1\"));Def(false,m,[]po()a(self:_=_,)va()kw()k()_[Expr(Ellipsis);]);]);]"
    );
    assert_eq!(module("type X[T] = list[T]\n"), "[Type(Xtp(TypeVar,T,__)list[T]);]");
}

#[test]
fn try_with_match() {
    let d = module(
        "try:\n    pass\nexcept (A, B) as e:\n    pass\nexcept* C:\n    pass\nelse:\n    pass\nfinally:\n    pass\n",
    );
    assert!(d.starts_with("[Try(true,"), "{d}");
    let d = module("with (open(a) as f, open(b) as g):\n    pass\n");
    assert_eq!(d, "[With(false,Call(open(a,)) as f,Call(open(b,)) as g,[Pass;]);]");
    let d = module("with (a, b):\n    pass\n");
    assert_eq!(d, "[With(false,a as _,b as _,[Pass;]);]");
    let d = module(
        "match p:\n    case [1, *rest] if rest:\n        pass\n    case {'k': v, **kw}:\n        pass\n    case Point(x=0) | None as q:\n        pass\n    case _:\n        pass\n",
    );
    assert!(d.contains("Seq[VInt(\"1\"),*Some(\"rest\"),]"), "{d}");
    assert!(d.contains("**Some(\"kw\")"), "{d}");
    assert!(
        d.contains("As(Or[Cls(Pointx=As(VInt(\"0\")") || d.contains("As(Or["),
        "{d}"
    );
    // `match` as an identifier still works
    assert_eq!(
        module("match = 1\nmatch.x\n"),
        "[Assign([match,]Int(\"1\"));Expr(match.x);]"
    );
}

#[test]
fn spans_are_byte_accurate() {
    let src = "x = foo(bar, baz)\n";
    let b = Bump::new();
    let m = parse_module(src, &b).unwrap().module;
    let StmtKind::Assign { value, .. } = &m.body[0].kind else {
        panic!()
    };
    assert_eq!(&src[value.span.range()], "foo(bar, baz)");
    assert_eq!(&src[m.body[0].span.range()], "x = foo(bar, baz)");
}

#[test]
fn syntax_errors() {
    for s in [
        "def f(:\n  pass\n",
        "x = (1,\n",
        "if x\n    pass\n",
        "x = 'abc\n",
        "a b\n",
        "@d\nx = 1\n",
        "try:\n    pass\n",
        "f(**)\n",
        "class\n",
        "for x in:\n  pass\n",
        "  x = 1\n",
        "x = $\n",
    ] {
        fails(s);
    }
}

#[test]
fn deep_nesting_does_not_overflow() {
    let src = format!("x = {}1{}\n", "(".repeat(5000), ")".repeat(5000));
    let b = Bump::new();
    assert!(parse_module(&src, &b).is_err());
    let src = format!("x = {}1\n", "-".repeat(5000));
    assert!(parse_module(&src, &b).is_err());
    let src = format!("x = {}\n", "[".repeat(90) + &"]".repeat(90));
    assert!(parse_module(&src, &b).is_ok());
}
