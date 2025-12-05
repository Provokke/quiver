/// A half-open byte range `[start, end)` into the source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }
    pub fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
    pub fn to(self, other: Span) -> Span {
        Span::new(self.start, other.end)
    }
    pub fn len(self) -> u32 {
        self.end - self.start
    }
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Maps byte offsets to 1-based (line, column) pairs.
pub struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(src: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }
        LineIndex { starts }
    }
    /// 0-based line number containing `off`.
    pub fn line_of(&self, off: u32) -> usize {
        match self.starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }
    pub fn line_start(&self, line: usize) -> u32 {
        self.starts[line]
    }
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }
    /// 1-based line and 1-based column (in characters).
    pub fn location(&self, src: &str, off: u32) -> (usize, usize) {
        let line = self.line_of(off);
        let start = self.starts[line] as usize;
        let off = (off as usize).min(src.len());
        let col = src.get(start..off).map(|s| s.chars().count()).unwrap_or(off - start);
        (line + 1, col + 1)
    }
}
