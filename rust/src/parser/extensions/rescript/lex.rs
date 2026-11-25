use regex::Regex;
use std::sync::LazyLock;

pub(super) struct Source {
    pub cleaned: String,
    depths: Vec<usize>,
    pub lines: Vec<usize>,
}
impl Source {
    pub fn new(source: &str) -> Self {
        let input = source.as_bytes();
        let mut bytes = input.to_vec();
        let mut i = 0;
        let mut comments = 0;
        let mut quote = None;
        let mut line_comment = false;
        while i < bytes.len() {
            let ch = input[i];
            let next = input.get(i + 1).copied();
            if line_comment {
                if ch == b'\n' {
                    line_comment = false;
                } else {
                    bytes[i] = b' ';
                }
                i += 1;
                continue;
            }
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                } else if q == b'"' && ch == b'\\' && next.is_some() {
                    bytes[i..i + 2].fill(b' ');
                    i += 2;
                    continue;
                } else if ch != b'\n' {
                    bytes[i] = b' ';
                }
                i += 1;
                continue;
            }
            if ch == b'/' && next == Some(b'*') {
                comments += 1;
                bytes[i..i + 2].fill(b' ');
                i += 2;
                continue;
            }
            if comments > 0 {
                if ch == b'*' && next == Some(b'/') {
                    comments -= 1;
                    bytes[i..i + 2].fill(b' ');
                    i += 2;
                    continue;
                }
                if ch != b'\n' {
                    bytes[i] = b' ';
                }
                i += 1;
                continue;
            }
            if ch == b'/' && next == Some(b'/') {
                line_comment = true;
                continue;
            }
            if matches!(ch, b'"' | b'`') {
                quote = Some(ch);
            }
            i += 1;
        }
        let mut depth = 0usize;
        let mut depths = Vec::with_capacity(bytes.len() + 1);
        let mut lines = vec![0];
        for (i, &ch) in bytes.iter().enumerate() {
            depths.push(depth);
            match ch {
                b'{' => depth += 1,
                b'}' => depth = depth.saturating_sub(1),
                b'\n' => lines.push(i + 1),
                _ => {}
            }
        }
        depths.push(depth);
        Self {
            cleaned: String::from_utf8(bytes).unwrap(),
            depths,
            lines,
        }
    }
    pub fn depth(&self, offset: usize) -> usize {
        self.depths[offset]
    }
    pub fn line(&self, off: usize) -> usize {
        self.lines.partition_point(|&v| v <= off)
    }
}
pub(super) static MODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*module\s+(?:type\s+)?([A-Z][A-Za-z0-9_']*)\s*[:=]").unwrap()
});
pub(super) static ALIAS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*module\s+([A-Z][A-Za-z0-9_']*)\s*=\s*([A-Z][A-Za-z0-9_']*(?:\.[A-Za-z_][A-Za-z0-9_']*)*)\s*$").unwrap()
});
pub(super) static NEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)(?:let|type|module|external|and)\s").unwrap());
pub(super) static OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(open|include)\s+([A-Z][A-Za-z0-9_'.]*)").unwrap());
pub(super) static ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@module\(\s*"([^"]+)"\s*\)"#).unwrap());
pub(super) static JSX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Z][A-Za-z0-9_']*(?:\.[A-Z][A-Za-z0-9_']*)*)\b").unwrap());
pub(super) static CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z_][A-Za-z0-9_']*(?:\.[A-Za-z_][A-Za-z0-9_']*)*)\s*\(").unwrap()
});
fn declaration(keyword: &str) -> Regex {
    Regex::new(&format!(r"(?m)^\s*(?:@[A-Za-z_][A-Za-z0-9_']*(?:\([^)]*\))?\s+)*{keyword}([A-Za-z_][A-Za-z0-9_']*)\b")).unwrap()
}
pub(super) static LET: LazyLock<Regex> =
    LazyLock::new(|| declaration(r"(?:let\s+(?:rec\s+)?|and\s+)"));
pub(super) static EXTERNAL: LazyLock<Regex> = LazyLock::new(|| declaration(r"external\s+"));
pub(super) static TYPE: LazyLock<Regex> = LazyLock::new(|| declaration(r"type\s+(?:rec\s+)?"));
pub(super) fn keyword(v: &str) -> bool {
    matches!(
        v,
        "let"
            | "rec"
            | "and"
            | "type"
            | "module"
            | "open"
            | "include"
            | "external"
            | "if"
            | "else"
            | "switch"
            | "when"
            | "match"
            | "fun"
            | "true"
            | "false"
            | "for"
            | "while"
            | "mutable"
            | "try"
            | "catch"
            | "throw"
            | "assert"
            | "lazy"
            | "do"
            | "in"
            | "of"
            | "as"
            | "exception"
            | "private"
            | "constraint"
            | "with"
            | "downto"
            | "to"
            | "unpack"
            | "async"
            | "await"
    )
}
