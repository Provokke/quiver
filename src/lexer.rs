//! Zero-copy tokenizer for Python source.
//!
//! Tokens carry byte spans plus a `&str` slice that borrows the original
//! source buffer (which may be a memory-mapped file). Comments are collected
//! on the side so the parser never sees them, but the formatter, `noqa`
//! handling and the import sorter can.

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tok {
    Name,
    Int,
    Float,
    Imag,
    String,
    FString,
    Newline,
    Indent,
    Dedent,
    EndMarker,
    Error,
    // keywords
    False,
    None,
    True,
    And,
    As,
    Assert,
    Async,
    Await,
    Break,
    Class,
    Continue,
    Def,
    Del,
    Elif,
    Else,
    Except,
    Finally,
    For,
    From,
    Global,
    If,
    Import,
    In,
    Is,
    Lambda,
    Nonlocal,
    Not,
    Or,
    Pass,
    Raise,
    Return,
    Try,
    While,
    With,
    Yield,
    // punctuation
    LPar,
    RPar,
    LSqb,
    RSqb,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Semi,
    Plus,
    Minus,
    Star,
    Slash,
    VBar,
    Amper,
    Less,
    Greater,
    Equal,
    Dot,
    Percent,
    EqEqual,
    NotEqual,
    LessEqual,
    GreaterEqual,
    Tilde,
    CircumFlex,
    LeftShift,
    RightShift,
    DoubleStar,
    PlusEqual,
    MinEqual,
    StarEqual,
    SlashEqual,
    PercentEqual,
    AmperEqual,
    VBarEqual,
    CircumflexEqual,
    LeftShiftEqual,
    RightShiftEqual,
    DoubleStarEqual,
    DoubleSlash,
    DoubleSlashEqual,
    At,
    AtEqual,
    RArrow,
    Ellipsis,
    ColonEqual,
    Exclamation,
}

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "False" => Tok::False,
        "None" => Tok::None,
        "True" => Tok::True,
        "and" => Tok::And,
        "as" => Tok::As,
        "assert" => Tok::Assert,
        "async" => Tok::Async,
        "await" => Tok::Await,
        "break" => Tok::Break,
        "class" => Tok::Class,
        "continue" => Tok::Continue,
        "def" => Tok::Def,
        "del" => Tok::Del,
        "elif" => Tok::Elif,
        "else" => Tok::Else,
        "except" => Tok::Except,
        "finally" => Tok::Finally,
        "for" => Tok::For,
        "from" => Tok::From,
        "global" => Tok::Global,
        "if" => Tok::If,
        "import" => Tok::Import,
        "in" => Tok::In,
        "is" => Tok::Is,
        "lambda" => Tok::Lambda,
        "nonlocal" => Tok::Nonlocal,
        "not" => Tok::Not,
        "or" => Tok::Or,
        "pass" => Tok::Pass,
        "raise" => Tok::Raise,
        "return" => Tok::Return,
        "try" => Tok::Try,
        "while" => Tok::While,
        "with" => Tok::With,
        "yield" => Tok::Yield,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: Tok,
    pub span: Span,
    pub text: &'a str,
}

#[derive(Debug, Clone)]
pub struct LexError {
    pub span: Span,
    pub msg: String,
}

/// Raw structure of an f-string body, found by [`scan_fstring`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FPartRaw {
    Literal(Span),
    Field {
        expr: Span,
        debug: bool,
        conversion: Option<u8>,
        spec: Option<Vec<FPartRaw>>,
        span: Span,
    },
}

pub struct Lexed<'a> {
    pub tokens: Vec<Token<'a>>,
    pub comments: Vec<Span>,
    pub errors: Vec<LexError>,
}

fn is_id_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic() || (c as u32 >= 0x80 && c.is_alphanumeric())
}
fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric() || (c as u32 >= 0x80 && (c.is_alphanumeric() || is_mark(c)))
}
fn is_mark(c: char) -> bool {
    // combining marks and a few common joiners used in identifiers
    matches!(c as u32, 0x300..=0x36F | 0x200C | 0x200D | 0xB7)
        || (!c.is_whitespace() && !c.is_ascii() && !c.is_control())
}

/// Returns `(is_string_prefix, is_f, is_raw, is_bytes)` for an identifier immediately followed by a quote.
pub fn string_prefix(p: &str) -> Option<(bool, bool, bool)> {
    if p.len() > 2 {
        return None;
    }
    let mut f = false;
    let mut r = false;
    let mut b = false;
    let mut u = false;
    for c in p.chars() {
        match c.to_ascii_lowercase() {
            'f' if !f && !b && !u => f = true,
            'r' if !r && !u => r = true,
            'b' if !b && !f && !u => b = true,
            'u' if p.len() == 1 => u = true,
            _ => return None,
        }
    }
    Some((f, r, b))
}

pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    end: usize,
    out: Vec<Token<'a>>,
    comments: Vec<Span>,
    errors: Vec<LexError>,
    indents: Vec<u32>,
    depth: u32,
    at_line_start: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self::new_range(src, 0, src.len(), true)
    }

    /// Lexes `src[start..end]` with spans relative to the full source.
    /// With `indent == false` the range is treated as being inside brackets.
    pub fn new_range(src: &'a str, start: usize, end: usize, indent: bool) -> Self {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: start,
            end,
            out: Vec::with_capacity((end - start) / 4 + 8),
            comments: Vec::new(),
            errors: Vec::new(),
            indents: vec![0],
            depth: if indent { 0 } else { 1 },
            at_line_start: indent,
        }
    }

    fn peek(&self, off: usize) -> u8 {
        let p = self.pos + off;
        if p < self.end { self.bytes[p] } else { 0 }
    }

    fn push(&mut self, kind: Tok, start: usize, end: usize) {
        self.out.push(Token {
            kind,
            span: Span::new(start as u32, end as u32),
            text: &self.src[start..end],
        });
    }

    fn err(&mut self, start: usize, end: usize, msg: &str) {
        self.errors.push(LexError {
            span: Span::new(start as u32, end as u32),
            msg: msg.to_string(),
        });
    }

    pub fn tokenize(mut self) -> Lexed<'a> {
        if self.pos == 0 && self.src.starts_with('\u{feff}') {
            self.pos = 3;
        }
        while self.pos < self.end {
            if self.at_line_start {
                self.at_line_start = false;
                if self.handle_indent() {
                    continue;
                }
            }
            let c = self.bytes[self.pos];
            match c {
                b' ' | b'\t' | b'\x0c' => self.pos += 1,
                b'\r' | b'\n' => {
                    let start = self.pos;
                    if c == b'\r' && self.peek(1) == b'\n' {
                        self.pos += 2
                    } else {
                        self.pos += 1
                    }
                    if self.depth == 0 {
                        let last = self.out.last().map(|t| t.kind);
                        if !matches!(last, None | Some(Tok::Newline) | Some(Tok::Indent) | Some(Tok::Dedent)) {
                            self.push(Tok::Newline, start, self.pos);
                        }
                        self.at_line_start = true;
                    }
                }
                b'#' => {
                    let start = self.pos;
                    while self.pos < self.end && !matches!(self.bytes[self.pos], b'\n' | b'\r') {
                        self.pos += 1;
                    }
                    self.comments.push(Span::new(start as u32, self.pos as u32));
                }
                b'\\' => {
                    // line continuation
                    let start = self.pos;
                    self.pos += 1;
                    match self.peek(0) {
                        b'\n' => self.pos += 1,
                        b'\r' => {
                            self.pos += 1;
                            if self.peek(0) == b'\n' {
                                self.pos += 1
                            }
                        }
                        _ => {
                            self.err(start, self.pos, "unexpected character after line continuation");
                            self.push(Tok::Error, start, self.pos);
                        }
                    }
                }
                b'0'..=b'9' => self.number(),
                b'.' if self.peek(1).is_ascii_digit() => self.number(),
                b'"' | b'\'' => {
                    let start = self.pos;
                    self.string(start, false, false);
                }
                _ if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => self.name(),
                _ => self.punct(),
            }
        }
        // finish
        let end = self.end;
        if self.depth == 0 || self.indents.len() > 1 {
            let last = self.out.last().map(|t| t.kind);
            if !matches!(last, None | Some(Tok::Newline) | Some(Tok::Dedent)) {
                self.push(Tok::Newline, end, end);
            }
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(Tok::Dedent, end, end);
        }
        self.push(Tok::EndMarker, end, end);
        Lexed {
            tokens: self.out,
            comments: self.comments,
            errors: self.errors,
        }
    }

    /// Handles indentation at the start of a logical line. Returns true if the
    /// line was blank / comment-only and has been consumed.
    fn handle_indent(&mut self) -> bool {
        let mut col: u32 = 0;
        let start = self.pos;
        while self.pos < self.end {
            match self.bytes[self.pos] {
                b' ' => col += 1,
                b'\t' => col = (col / 8 + 1) * 8,
                b'\x0c' => col = 0,
                _ => break,
            }
            self.pos += 1;
        }
        let c = self.peek(0);
        if c == b'\n'
            || c == b'\r'
            || c == b'#'
            || self.pos >= self.end
            || (c == b'\\' && matches!(self.peek(1), b'\n' | b'\r'))
        {
            // blank line: consume comment & newline without NEWLINE token
            if c == b'#' {
                let s = self.pos;
                while self.pos < self.end && !matches!(self.bytes[self.pos], b'\n' | b'\r') {
                    self.pos += 1;
                }
                self.comments.push(Span::new(s as u32, self.pos as u32));
            }
            if c == b'\\' {
                // a continuation on an otherwise blank line; treat as part of line
                return false;
            }
            if self.peek(0) == b'\r' {
                self.pos += 1;
            }
            if self.peek(0) == b'\n' {
                self.pos += 1;
            }
            self.at_line_start = true;
            return true;
        }
        let cur = *self.indents.last().unwrap();
        if col > cur {
            self.indents.push(col);
            self.push(Tok::Indent, start, self.pos);
        } else if col < cur {
            while col < *self.indents.last().unwrap() {
                self.indents.pop();
                self.push(Tok::Dedent, self.pos, self.pos);
            }
            if col != *self.indents.last().unwrap() {
                self.err(start, self.pos, "unindent does not match any outer indentation level");
                self.push(Tok::Error, start, self.pos);
            }
        }
        false
    }

    fn name(&mut self) {
        let start = self.pos;
        let rest = &self.src[self.pos..self.end];
        let mut len = 0;
        for (i, ch) in rest.char_indices() {
            let ok = if i == 0 { is_id_start(ch) } else { is_id_continue(ch) };
            if !ok || ch.is_whitespace() || (ch.is_ascii() && !(ch.is_ascii_alphanumeric() || ch == '_')) {
                break;
            }
            len = i + ch.len_utf8();
        }
        if len == 0 {
            let ch = rest.chars().next().unwrap();
            self.pos += ch.len_utf8();
            self.err(start, self.pos, "invalid character");
            self.push(Tok::Error, start, self.pos);
            return;
        }
        self.pos += len;
        let text = &self.src[start..self.pos];
        let q = self.peek(0);
        if (q == b'"' || q == b'\'')
            && let Some((f, r, _)) = string_prefix(text)
        {
            self.string(start, f, r);
            return;
        }
        let kind = keyword(text).unwrap_or(Tok::Name);
        self.push(kind, start, self.pos);
    }

    fn string(&mut self, start: usize, is_f: bool, raw: bool) {
        match scan_string(self.bytes, self.pos, self.end, is_f, raw, None) {
            Ok(end) => {
                self.pos = end;
                self.push(if is_f { Tok::FString } else { Tok::String }, start, end);
            }
            Err(end) => {
                self.pos = end;
                self.err(start, end, "unterminated string literal");
                self.push(Tok::Error, start, end);
            }
        }
    }

    fn digits(&mut self, pred: fn(u8) -> bool) {
        while self.pos < self.end && (pred(self.bytes[self.pos]) || self.bytes[self.pos] == b'_') {
            self.pos += 1;
        }
    }

    fn number(&mut self) {
        let start = self.pos;
        let mut kind = Tok::Int;
        if self.peek(0) == b'0' && matches!(self.peek(1), b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
            self.pos += 2;
            self.digits(|c| c.is_ascii_hexdigit());
        } else {
            self.digits(|c| c.is_ascii_digit());
            if self.peek(0) == b'.' {
                self.pos += 1;
                kind = Tok::Float;
                self.digits(|c| c.is_ascii_digit());
            }
            if matches!(self.peek(0), b'e' | b'E')
                && (self.peek(1).is_ascii_digit()
                    || (matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit()))
            {
                kind = Tok::Float;
                self.pos += 2;
                self.digits(|c| c.is_ascii_digit());
            }
            if matches!(self.peek(0), b'j' | b'J') {
                kind = Tok::Imag;
                self.pos += 1;
            }
        }
        self.push(kind, start, self.pos);
    }

    fn punct(&mut self) {
        let start = self.pos;
        let (c0, c1, c2) = (self.peek(0), self.peek(1), self.peek(2));
        let (kind, len) = match (c0, c1, c2) {
            (b'*', b'*', b'=') => (Tok::DoubleStarEqual, 3),
            (b'/', b'/', b'=') => (Tok::DoubleSlashEqual, 3),
            (b'<', b'<', b'=') => (Tok::LeftShiftEqual, 3),
            (b'>', b'>', b'=') => (Tok::RightShiftEqual, 3),
            (b'.', b'.', b'.') => (Tok::Ellipsis, 3),
            (b'*', b'*', _) => (Tok::DoubleStar, 2),
            (b'/', b'/', _) => (Tok::DoubleSlash, 2),
            (b'<', b'<', _) => (Tok::LeftShift, 2),
            (b'>', b'>', _) => (Tok::RightShift, 2),
            (b'=', b'=', _) => (Tok::EqEqual, 2),
            (b'!', b'=', _) => (Tok::NotEqual, 2),
            (b'<', b'=', _) => (Tok::LessEqual, 2),
            (b'>', b'=', _) => (Tok::GreaterEqual, 2),
            (b'<', b'>', _) => (Tok::NotEqual, 2),
            (b'-', b'>', _) => (Tok::RArrow, 2),
            (b':', b'=', _) => (Tok::ColonEqual, 2),
            (b'+', b'=', _) => (Tok::PlusEqual, 2),
            (b'-', b'=', _) => (Tok::MinEqual, 2),
            (b'*', b'=', _) => (Tok::StarEqual, 2),
            (b'/', b'=', _) => (Tok::SlashEqual, 2),
            (b'%', b'=', _) => (Tok::PercentEqual, 2),
            (b'&', b'=', _) => (Tok::AmperEqual, 2),
            (b'|', b'=', _) => (Tok::VBarEqual, 2),
            (b'^', b'=', _) => (Tok::CircumflexEqual, 2),
            (b'@', b'=', _) => (Tok::AtEqual, 2),
            (b'(', ..) => (Tok::LPar, 1),
            (b')', ..) => (Tok::RPar, 1),
            (b'[', ..) => (Tok::LSqb, 1),
            (b']', ..) => (Tok::RSqb, 1),
            (b'{', ..) => (Tok::LBrace, 1),
            (b'}', ..) => (Tok::RBrace, 1),
            (b':', ..) => (Tok::Colon, 1),
            (b',', ..) => (Tok::Comma, 1),
            (b';', ..) => (Tok::Semi, 1),
            (b'+', ..) => (Tok::Plus, 1),
            (b'-', ..) => (Tok::Minus, 1),
            (b'*', ..) => (Tok::Star, 1),
            (b'/', ..) => (Tok::Slash, 1),
            (b'|', ..) => (Tok::VBar, 1),
            (b'&', ..) => (Tok::Amper, 1),
            (b'<', ..) => (Tok::Less, 1),
            (b'>', ..) => (Tok::Greater, 1),
            (b'=', ..) => (Tok::Equal, 1),
            (b'.', ..) => (Tok::Dot, 1),
            (b'%', ..) => (Tok::Percent, 1),
            (b'~', ..) => (Tok::Tilde, 1),
            (b'^', ..) => (Tok::CircumFlex, 1),
            (b'@', ..) => (Tok::At, 1),
            (b'!', ..) => (Tok::Exclamation, 1),
            _ => (Tok::Error, 1),
        };
        match kind {
            Tok::LPar | Tok::LSqb | Tok::LBrace => self.depth += 1,
            Tok::RPar | Tok::RSqb | Tok::RBrace => self.depth = self.depth.saturating_sub(1),
            Tok::Error => self.err(start, start + 1, "invalid character"),
            _ => {}
        }
        self.pos += len;
        self.push(kind, start, self.pos);
    }
}

/// Scans a string literal starting at the opening quote `pos`. Returns the
/// position after the closing quote, or `Err(pos)` where scanning stopped.
/// When `parts` is given (f-strings only), the structure of the body is recorded.
pub fn scan_string(
    b: &[u8],
    pos: usize,
    end: usize,
    is_f: bool,
    raw: bool,
    parts: Option<&mut Vec<FPartRaw>>,
) -> Result<usize, usize> {
    let q = b[pos];
    let triple = pos + 2 < end && b[pos + 1] == q && b[pos + 2] == q;
    let body = pos + if triple { 3 } else { 1 };
    if is_f {
        let mut tmp = Vec::new();
        let p = parts.unwrap_or(&mut tmp);
        return scan_fbody(b, body, end, q, triple, raw, false, p);
    }
    let mut i = body;
    while i < end {
        let c = b[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if c == q {
            if !triple {
                return Ok(i + 1);
            }
            if i + 2 < end && b[i + 1] == q && b[i + 2] == q {
                return Ok(i + 3);
            }
        } else if (c == b'\n' || c == b'\r') && !triple {
            return Err(i);
        }
        i += 1;
    }
    Err(end.min(i))
}

fn closes(b: &[u8], i: usize, end: usize, q: u8, triple: bool) -> Option<usize> {
    if b[i] != q {
        return None;
    }
    if !triple {
        return Some(i + 1);
    }
    if i + 2 < end && b[i + 1] == q && b[i + 2] == q {
        return Some(i + 3);
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn scan_fbody(
    b: &[u8],
    mut i: usize,
    end: usize,
    q: u8,
    triple: bool,
    _raw: bool,
    in_spec: bool,
    parts: &mut Vec<FPartRaw>,
) -> Result<usize, usize> {
    let mut lit_start = i;
    let flush = |parts: &mut Vec<FPartRaw>, s: usize, e: usize| {
        if e > s {
            parts.push(FPartRaw::Literal(Span::new(s as u32, e as u32)));
        }
    };
    while i < end {
        let c = b[i];
        if c == b'\\' {
            if i + 1 < end && (b[i + 1] == b'{' || b[i + 1] == b'}') {
                i += 1;
            } else {
                i += 2;
            }
            continue;
        }
        if in_spec && c == b'}' {
            flush(parts, lit_start, i);
            return Ok(i);
        }
        if let Some(e) = closes(b, i, end, q, triple) {
            if in_spec {
                return Err(i);
            }
            flush(parts, lit_start, i);
            return Ok(e);
        }
        if (c == b'\n' || c == b'\r') && !triple {
            return Err(i);
        }
        if c == b'{' {
            if !in_spec && i + 1 < end && b[i + 1] == b'{' {
                i += 2;
                continue;
            }
            flush(parts, lit_start, i);
            let field_start = i;
            i = scan_field(b, i + 1, end, q, triple, field_start, parts)?;
            lit_start = i;
            continue;
        }
        if c == b'}' && i + 1 < end && b[i + 1] == b'}' {
            i += 2;
            continue;
        }
        i += 1;
    }
    Err(end)
}

fn scan_field(
    b: &[u8],
    mut i: usize,
    end: usize,
    q: u8,
    triple: bool,
    field_start: usize,
    parts: &mut Vec<FPartRaw>,
) -> Result<usize, usize> {
    let expr_start = i;
    let mut depth = 0i32;
    let mut expr_end = None;
    let mut debug = false;
    let mut conversion = None;
    while i < end {
        let c = b[i];
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => depth -= 1,
            b'}' if depth > 0 => depth -= 1,
            b'}' => {
                let ee = expr_end.unwrap_or(i);
                parts.push(FPartRaw::Field {
                    expr: Span::new(expr_start as u32, ee as u32),
                    debug,
                    conversion,
                    spec: None,
                    span: Span::new(field_start as u32, i as u32 + 1),
                });
                return Ok(i + 1);
            }
            b'\'' | b'"' => {
                if closes(b, i, end, q, triple).is_some() && !(triple && c == q) {
                    // the enclosing quote can't appear raw before 3.12; 3.12 allows it
                }
                // nested string: find prefix
                let mut ps = i;
                while ps > expr_start && (b[ps - 1].is_ascii_alphabetic()) {
                    ps -= 1;
                }
                let prefix = std::str::from_utf8(&b[ps..i]).unwrap_or("");
                let (f, r) = match string_prefix(prefix) {
                    Some((f, r, _)) => (f, r),
                    None => (false, false),
                };
                i = scan_string(b, i, end, f, r, None)?;
                continue;
            }
            b'=' if depth == 0 && expr_end.is_none() => {
                let prev = if i > expr_start { b[i - 1] } else { 0 };
                let next = if i + 1 < end { b[i + 1] } else { 0 };
                if next != b'=' && !matches!(prev, b'=' | b'!' | b'<' | b'>') {
                    debug = true;
                    expr_end = Some(i);
                } else if next == b'=' {
                    i += 1;
                }
            }
            b'!' if depth == 0 && i + 1 < end && b[i + 1] != b'=' => {
                if expr_end.is_none() {
                    expr_end = Some(i);
                }
                if i + 1 < end {
                    conversion = Some(b[i + 1]);
                }
                i += 2;
                continue;
            }
            b':' if depth == 0 => {
                let ee = expr_end.unwrap_or(i);
                let mut spec = Vec::new();
                let close = scan_fbody(b, i + 1, end, q, triple, false, true, &mut spec)?;
                parts.push(FPartRaw::Field {
                    expr: Span::new(expr_start as u32, ee as u32),
                    debug,
                    conversion,
                    spec: Some(spec),
                    span: Span::new(field_start as u32, close as u32 + 1),
                });
                return Ok(close + 1);
            }
            b'\n' | b'\r' if !triple && depth == 0 => return Err(i),
            _ => {}
        }
        i += 1;
    }
    Err(end)
}

pub fn tokenize(src: &str) -> Lexed<'_> {
    Lexer::new(src).tokenize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        tokenize(src).tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn simple_assignment() {
        assert_eq!(
            kinds("x = 1\n"),
            vec![Tok::Name, Tok::Equal, Tok::Int, Tok::Newline, Tok::EndMarker]
        );
    }

    #[test]
    fn indent_dedent() {
        let k = kinds("if x:\n    y\nz\n");
        assert_eq!(
            k,
            vec![
                Tok::If,
                Tok::Name,
                Tok::Colon,
                Tok::Newline,
                Tok::Indent,
                Tok::Name,
                Tok::Newline,
                Tok::Dedent,
                Tok::Name,
                Tok::Newline,
                Tok::EndMarker
            ]
        );
    }

    #[test]
    fn dedent_at_eof_without_newline() {
        let k = kinds("def f():\n    pass");
        assert_eq!(k[k.len() - 3..], [Tok::Newline, Tok::Dedent, Tok::EndMarker]);
    }

    #[test]
    fn brackets_suppress_newlines() {
        let k = kinds("x = (1,\n  2)\n");
        assert!(!k[..k.len() - 2].contains(&Tok::Newline));
    }

    #[test]
    fn comments_and_blank_lines() {
        let l = tokenize("# hi\n\n  # indented comment\nx = 1  # trailing\n");
        assert_eq!(l.comments.len(), 3);
        assert_eq!(l.tokens[0].kind, Tok::Name);
    }

    #[test]
    fn numbers() {
        for (s, k) in [
            ("0x_FF", Tok::Int),
            ("1_000", Tok::Int),
            ("1.5e-3", Tok::Float),
            (".5", Tok::Float),
            ("1.", Tok::Float),
            ("3j", Tok::Imag),
            ("0b101", Tok::Int),
            ("1e5", Tok::Float),
            ("2.5J", Tok::Imag),
        ] {
            let l = tokenize(s);
            assert_eq!(l.tokens[0].kind, k, "{s}");
            assert_eq!(l.tokens[0].text, s);
        }
    }

    #[test]
    fn strings_all_forms() {
        for s in [
            r#""a""#,
            "'b'",
            r#"'''x'y'''"#,
            r#""""multi
line""""#,
            r#"r"\d""#,
            r#"b'\x00'"#,
            r#"Rb"raw""#,
            r#""esc\"aped""#,
            r#"u"x""#,
        ] {
            let l = tokenize(s);
            assert_eq!(l.tokens[0].kind, Tok::String, "{s}");
            assert_eq!(l.tokens[0].text, s);
        }
    }

    #[test]
    fn fstrings_nested() {
        for s in [
            r#"f"{x}""#,
            r#"f"{x!r:>{width}}""#,
            r#"f'{a["k"]}'"#,
            r#"f"{x["k"]}""#,
            r#"f"{{literal}}""#,
            r#"f"{x=}""#,
            r#"rf"\d{x}""#,
            r#"f"{f'{y}'}""#,
            r#"f"{a!=b}""#,
            r#"f"{'\n'.join(x)}""#,
        ] {
            let l = tokenize(s);
            assert!(l.errors.is_empty(), "{s}: {:?}", l.errors);
            assert_eq!(l.tokens[0].kind, Tok::FString, "{s}");
            assert_eq!(l.tokens[0].text, s);
        }
    }

    #[test]
    fn fstring_parts() {
        let s = r#"f"a{x!r:>{w}}b{y=}""#;
        let mut parts = Vec::new();
        let end = scan_string(s.as_bytes(), 1, s.len(), true, false, Some(&mut parts)).unwrap();
        assert_eq!(end, s.len());
        assert_eq!(parts.len(), 4);
        match &parts[1] {
            FPartRaw::Field {
                expr, conversion, spec, ..
            } => {
                assert_eq!(&s[expr.range()], "x");
                assert_eq!(*conversion, Some(b'r'));
                assert_eq!(spec.as_ref().unwrap().len(), 2);
            }
            _ => panic!(),
        }
        assert!(matches!(&parts[3], FPartRaw::Field { debug: true, .. }));
    }

    #[test]
    fn unterminated_string_is_error_not_panic() {
        let l = tokenize("x = 'abc\n");
        assert!(!l.errors.is_empty());
    }

    #[test]
    fn keywords_and_ops() {
        assert_eq!(
            kinds("a **= b // c -> d := e"),
            vec![
                Tok::Name,
                Tok::DoubleStarEqual,
                Tok::Name,
                Tok::DoubleSlash,
                Tok::Name,
                Tok::RArrow,
                Tok::Name,
                Tok::ColonEqual,
                Tok::Name,
                Tok::Newline,
                Tok::EndMarker
            ]
        );
        assert_eq!(
            kinds("not in is lambda")[..4],
            [Tok::Not, Tok::In, Tok::Is, Tok::Lambda]
        );
    }

    #[test]
    fn unicode_identifiers() {
        let l = tokenize("héllo = 1\n");
        assert_eq!(l.tokens[0].text, "héllo");
    }

    #[test]
    fn continuation_lines() {
        assert_eq!(
            kinds("x = 1 + \\\n  2\n"),
            vec![
                Tok::Name,
                Tok::Equal,
                Tok::Int,
                Tok::Plus,
                Tok::Int,
                Tok::Newline,
                Tok::EndMarker
            ]
        );
    }

    #[test]
    fn bad_dedent_reports_error() {
        let l = tokenize("if x:\n        a\n    b\n");
        assert!(!l.errors.is_empty());
    }
}
