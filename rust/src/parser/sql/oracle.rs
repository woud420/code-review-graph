use super::{Extractor, LazyLock, Path, Regex, is_keyword, pattern};
use crate::model::Call;
use std::collections::{BTreeMap, HashSet};

fn signal(text: &str) -> bool {
    pattern!(r"(?ix)%ROWTYPE\b|%TYPE\b|\b(?:VARCHAR2|PLS_INTEGER|BINARY_INTEGER|SYS_REFCURSOR|DBMS_OUTPUT)\b|:(?:NEW|OLD)\.|\bCREATE\s+(?:OR\s+REPLACE\s+)?PACKAGE\b").is_match(text)
        || pattern!(r"(?ix)\bCREATE\s+(?:OR\s+REPLACE\s+)?(?:FUNCTION\s+[\w$\#.]+\s*(?:\((?:[^();]|\([^()]*\))*\)\s*)?RETURN\b|PROCEDURE\s+[\w$\#.]+\s*(?:\((?:[^();]|\([^()]*\))*\)\s*)?IS\b)|\bCREATE\s+OR\s+REPLACE\s+PROCEDURE\s+[\w$\#.]+\s*(?:\((?:[^();]|\([^()]*\))*\)\s*)?AS\s+[^\s$']").is_match(text)
}

pub(super) fn clean_if_oracle(path: &Path, text: &str) -> Option<String> {
    let oracle_ext = path.extension().is_some_and(|e| e != "sql");
    if !oracle_ext && !signal(text) {
        return None;
    }
    let clean = mask(text);
    (oracle_ext || signal(&clean)).then_some(clean)
}

// Mask byte-for-byte to keep tree-sitter offsets and original line numbers valid.
fn mask(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut clean = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"--") {
            i += bytes[i..]
                .iter()
                .position(|b| *b == b'\n')
                .unwrap_or(bytes.len() - i);
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            i += bytes[i..]
                .windows(2)
                .position(|w| w == b"*/")
                .map_or(bytes.len() - i, |n| n + 2);
        } else if matches!(bytes[i], b'q' | b'Q')
            && bytes.get(i + 1) == Some(&b'\'')
            && i + 2 < bytes.len()
        {
            let closing = match bytes[i + 2] {
                b'[' => b']',
                b'{' => b'}',
                b'(' => b')',
                b'<' => b'>',
                c => c,
            };
            i += 3;
            i += bytes[i..]
                .windows(2)
                .position(|w| w == [closing, b'\''])
                .map_or(bytes.len() - i, |n| n + 2);
        } else if matches!(bytes[i], b'\'' | b'"') {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if c == quote {
                    if bytes.get(i) != Some(&quote) {
                        break;
                    }
                    i += 1;
                }
            }
        } else {
            i += 1;
            continue;
        }
        for b in &mut clean[start..i] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    String::from_utf8(clean).expect("only complete SQL tokens are blanked")
}

struct Blocks<'a> {
    text: &'a str,
    named: BTreeMap<String, Vec<usize>>,
}
impl<'a> Blocks<'a> {
    fn new(text: &'a str) -> Self {
        let mut named = BTreeMap::<String, Vec<usize>>::new();
        for c in pattern!(r"(?i)\bEND\b\s*([^\W\d][\w$#]*)\s*;").captures_iter(text) {
            named
                .entry(c[1].to_lowercase())
                .or_default()
                .push(c.get(0).unwrap().end());
        }
        Self { text, named }
    }

    fn end(&self, start: usize, limit: usize, name: &str, package: bool) -> usize {
        if let Some(ends) = self.named.get(&name.to_lowercase()) {
            let first = ends.partition_point(|i| *i < start);
            if let Some(&end) = ends.get(first).filter(|e| **e <= limit) {
                return end;
            }
        }
        let mut stack = Vec::new();
        let mut last = limit;
        let span = &self.text[start..limit];
        for hit in pattern!(r"(?i)\b(?:BEGIN|CASE|END\s+(?:IF|LOOP|CASE)\b|END)\b").find_iter(span)
        {
            if span[..hit.start()].ends_with(['$', '#'])
                || span[hit.end()..].starts_with(['$', '#'])
            {
                continue;
            }
            let token = hit.as_str().to_ascii_uppercase();
            if token == "BEGIN" || token == "CASE" {
                stack.push(token);
                continue;
            }
            if token.ends_with("IF") || token.ends_with("LOOP") {
                continue;
            }
            let raw_end = start + hit.end();
            let semi = self.text[raw_end..limit].find(';').map(|i| i + raw_end);
            last = semi.map_or(raw_end, |p| p + 1);
            let Some(open) = stack.pop() else { return last };
            if !package
                && stack.is_empty()
                && open == "BEGIN"
                && semi.is_some_and(|p| self.text[raw_end..p].trim().is_empty())
            {
                return last;
            }
        }
        last
    }
}

pub(super) fn routines(out: &mut Extractor<'_>, text: &str, oracle: bool) {
    let defs = pattern!(
        r"(?i)\bCREATE\s+(?:OR\s+REPLACE\s+)?(PACKAGE\s+BODY|PROCEDURE|FUNCTION|TRIGGER|PACKAGE|TABLE|VIEW)\s+(\w[\w$#]*(?:\.\w[\w$#]*)*)"
    );
    let boundaries: Vec<_> = defs.captures_iter(text).collect();
    let blocks = Blocks::new(if oracle { text } else { "" });
    for (index, decl) in boundaries.iter().enumerate() {
        let token = decl.get(0).unwrap();
        let name = decl[2].rsplit('.').next().unwrap();
        let kind = decl[1].to_ascii_lowercase();
        if kind == "table" || kind == "view" {
            continue;
        }
        if !oracle && kind != "procedure" {
            continue;
        }
        if kind != "procedure" && name.starts_with(char::is_numeric) {
            continue;
        }
        if kind == "function"
            && out
                .file
                .sql
                .declarations
                .get(name)
                .is_some_and(|d| d.extra["sql_kind"] == "function")
        {
            continue;
        }
        let package_body = kind.starts_with("package") && kind != "package";
        let limit = boundaries
            .get(index + 1)
            .map_or(text.len(), |n| n.get(0).unwrap().start());
        let end = if oracle {
            blocks.end(token.end(), limit, name, package_body)
        } else {
            token.start()
        };
        if package_body {
            let identity = format!("{name}$body");
            out.declare(name, &identity, None, "package_body", token.start()..end);
            blocks.members(out, &identity, token.end(), end);
        } else {
            out.declare(name, name, None, &kind, token.start()..end);
            if oracle && kind != "package" {
                calls(out, text, token.end(), end, name);
            }
        }
    }
}

impl Blocks<'_> {
    fn members(&self, out: &mut Extractor<'_>, parent: &str, start: usize, end: usize) {
        let span = &self.text[start..end];
        let mut declarations: Vec<_> = pattern!(r"(?i)\b(PROCEDURE|FUNCTION)\s+([^\W\d][\w$#]*)")
            .captures_iter(span)
            .map(|h| (h, end))
            .collect();
        let mut next = BTreeMap::new();
        for (decl, bound) in declarations.iter_mut().rev() {
            let name = decl[2].to_lowercase();
            *bound = next
                .insert(name, start + decl.get(0).unwrap().start())
                .unwrap_or(end);
        }
        for (decl, bound) in declarations {
            let token = decl.get(0).unwrap();
            let name = &decl[2];
            let offset = start + token.end();
            let tail = &self.text[offset..bound];
            let params = tail.trim_start();
            let (signature, header) = if let Some(body) = params.strip_prefix('(') {
                let mut depth = 1;
                let last = body.char_indices().find_map(|(i, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    (depth == 0).then_some(i)
                });
                let Some(last) = last else { continue };
                (
                    body[..last]
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    &body[last + 1..],
                )
            } else {
                (String::new(), tail)
            };
            if pattern!(r"(?i)\b(?:IS|AS)\b|;")
                .find(header)
                .is_none_or(|m| m.as_str() == ";")
            {
                continue;
            }
            let identity = format!("{parent}.{name}({signature})");
            let last = self.end(offset, bound, name, false);
            out.declare(
                name,
                &identity,
                Some(parent),
                &decl[1].to_ascii_lowercase(),
                (start + token.start())..last,
            );
            calls(out, self.text, offset, last, &identity);
        }
    }
}

fn calls(out: &mut Extractor<'_>, text: &str, start: usize, end: usize, owner: &str) {
    let span = &text[start..end];
    let mut seen = HashSet::new();
    for hit in pattern!(r"([^\W\d][\w$#]*(?:\.[^\W\d][\w$#]*){0,2})\s*\(").captures_iter(span) {
        let m = hit.get(1).unwrap();
        let name = m.as_str();
        if span[..m.start()]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '#'))
        {
            continue;
        }
        let lower = name.to_lowercase();
        let (receiver, method) = lower.rsplit_once('.').unwrap_or(("", &lower));
        if ignore_call(receiver, method) || lower.starts_with("dbms_") || lower.starts_with("utl_")
        {
            continue;
        }
        let prefix = span[..m.start()].trim_end();
        if prefix
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .is_some_and(|t| t.eq_ignore_ascii_case("into"))
        {
            continue;
        }
        if seen.insert(name) {
            out.file.calls.push(Call {
                caller: owner.into(),
                callee: name.into(),
                line: out.line(start + m.start()),
            });
        }
    }
}

fn ignore_call(receiver: &str, method: &str) -> bool {
    if is_keyword(method)
        || matches!(
            method,
            "if" | "elsif"
                | "else"
                | "then"
                | "end"
                | "loop"
                | "while"
                | "for"
                | "case"
                | "when"
                | "begin"
                | "declare"
                | "exception"
                | "return"
                | "raise"
                | "is"
                | "as"
                | "procedure"
                | "function"
                | "trigger"
                | "package"
                | "cursor"
                | "type"
                | "record"
                | "table"
                | "pragma"
                | "and"
                | "or"
                | "not"
                | "in"
                | "exists"
                | "between"
                | "into"
                | "over"
                | "varchar"
                | "varchar2"
                | "nvarchar2"
                | "number"
                | "char"
                | "nchar"
                | "date"
                | "timestamp"
                | "raw"
                | "boolean"
                | "clob"
                | "blob"
                | "long"
                | "float"
                | "integer"
                | "int"
                | "smallint"
                | "decimal"
                | "numeric"
                | "double"
                | "real"
                | "pls_integer"
                | "binary_integer"
                | "urowid"
                | "rowid"
                | "bfile"
                | "xmltype"
                | "sys_refcursor"
                | "interval"
        )
    {
        return true;
    }
    if !receiver.is_empty() && matches!(method, "count" | "trim") {
        return true;
    }
    matches!(receiver, "" | "sys" | "standard")
        && matches!(
            method,
            "count"
                | "sum"
                | "avg"
                | "max"
                | "min"
                | "greatest"
                | "least"
                | "to_char"
                | "to_date"
                | "to_number"
                | "to_timestamp"
                | "cast"
                | "nvl"
                | "nvl2"
                | "coalesce"
                | "nullif"
                | "decode"
                | "substr"
                | "substrb"
                | "instr"
                | "trim"
                | "ltrim"
                | "rtrim"
                | "upper"
                | "lower"
                | "initcap"
                | "length"
                | "lengthb"
                | "replace"
                | "lpad"
                | "rpad"
                | "concat"
                | "round"
                | "trunc"
                | "mod"
                | "abs"
                | "ceil"
                | "floor"
                | "power"
                | "sqrt"
                | "extract"
                | "sysdate"
                | "systimestamp"
                | "raise_application_error"
        )
}
