use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::ops::Range;

#[derive(Clone)]
pub(super) struct Item {
    pub key: String,
    pub value: Node,
    start: usize,
    comma: Option<usize>,
}

#[derive(Clone)]
pub(super) struct Node {
    pub value: Value,
    pub children: Vec<Item>,
    span: Range<usize>,
}
impl Node {
    pub fn member(&self, name: &str) -> Option<&Self> {
        self.children
            .iter()
            .find(|i| i.key == name)
            .map(|i| &i.value)
    }

    pub fn cut(&self, index: usize, cuts: &mut Vec<Range<usize>>) {
        let item = &self.children[index];
        if let Some(comma) = item.comma {
            cuts.push(item.start..comma + 1);
        } else {
            if let Some(comma) = index.checked_sub(1).and_then(|i| self.children[i].comma) {
                cuts.push(comma..comma + 1);
            }
            cuts.push(item.start..item.value.span.end);
        }
    }
}

struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl Cursor<'_> {
    fn peek(&mut self) -> Result<u8> {
        let bytes = self.text.as_bytes();
        while self.pos < bytes.len() {
            match bytes[self.pos] {
                b if b.is_ascii_whitespace() => self.pos += 1,
                b'/' if bytes.get(self.pos + 1) == Some(&b'/') => {
                    self.pos += self.text[self.pos..]
                        .find('\n')
                        .unwrap_or(bytes.len() - self.pos)
                }
                b'/' if bytes.get(self.pos + 1) == Some(&b'*') => {
                    self.pos += self.text[self.pos + 2..]
                        .find("*/")
                        .context("unterminated comment")?
                        + 4
                }
                b => return Ok(b),
            }
        }
        Ok(0)
    }
    fn string(&mut self) -> Result<String> {
        let start = self.pos;
        ensure!(self.peek()? == b'"', "expected quoted string");
        self.pos += 1;
        let bytes = self.text.as_bytes();
        while self.pos < bytes.len() {
            match bytes[self.pos] {
                b'\\' => self.pos += 2,
                b'"' => {
                    self.pos += 1;
                    return Ok(serde_json::from_str(&self.text[start..self.pos])?);
                }
                _ => self.pos += 1,
            }
        }
        bail!("unterminated string")
    }
    fn node(&mut self, depth: usize) -> Result<Node> {
        ensure!(depth < 128, "configuration nesting limit exceeded");
        let first = self.peek()?;
        let start = self.pos;
        let mut children = Vec::new();
        let value = match first {
            b'{' | b'[' => {
                self.pos += 1;
                let close = if first == b'{' { b'}' } else { b']' };
                while self.peek()? != close {
                    let start = self.pos;
                    let key = if first == b'{' {
                        let key = self.string()?;
                        ensure!(self.peek()? == b':', "expected colon");
                        self.pos += 1;
                        key
                    } else {
                        children.len().to_string()
                    };
                    let value = self.node(depth + 1)?;
                    let comma = if self.peek()? == b',' {
                        let pos = self.pos;
                        self.pos += 1;
                        Some(pos)
                    } else {
                        None
                    };
                    children.push(Item {
                        key,
                        value,
                        start,
                        comma,
                    });
                    if comma.is_none() {
                        break;
                    }
                }
                ensure!(self.peek()? == close, "expected container end");
                self.pos += 1;
                if first == b'{' {
                    Value::Object(
                        children
                            .iter()
                            .map(|c| (c.key.clone(), c.value.value.clone()))
                            .collect(),
                    )
                } else {
                    Value::Array(children.iter().map(|c| c.value.value.clone()).collect())
                }
            }
            b'"' => Value::String(self.string()?),
            _ => {
                let bytes = self.text.as_bytes();
                while self.pos < bytes.len()
                    && !bytes[self.pos].is_ascii_whitespace()
                    && !b",]}:/".contains(&bytes[self.pos])
                {
                    self.pos += 1
                }
                ensure!(self.pos > start, "expected a value");
                serde_json::from_str(&self.text[start..self.pos])?
            }
        };
        Ok(Node {
            value,
            children,
            span: start..self.pos,
        })
    }
}
pub(super) fn parse(text: &str) -> Result<Node> {
    let mut c = Cursor { text, pos: 0 };
    let value = c.node(0)?;
    ensure!(
        c.peek()? == 0 && c.pos == text.len(),
        "unexpected trailing input"
    );
    Ok(value)
}

pub(super) fn splice(text: &str, mut cuts: Vec<Range<usize>>, expected: &Value) -> Result<String> {
    let original = parse(text)?;
    fn trailing(n: &Node, cuts: &mut Vec<Range<usize>>) {
        let mut surviving = n
            .children
            .iter()
            .filter(|i| !cuts.iter().any(|r| r.contains(&i.start)));
        if let Some(last) = surviving.next_back()
            && n.children.last().is_some_and(|i| i.start != last.start)
            && let Some(comma) = last.comma
        {
            cuts.push(comma..comma + 1)
        }
        for i in &n.children {
            trailing(&i.value, cuts)
        }
    }
    trailing(&original, &mut cuts);
    cuts.sort_by_key(|r| r.start);
    let mut result = String::with_capacity(text.len());
    let mut stop = 0;
    for r in cuts {
        if r.start > stop {
            result.push_str(&text[stop..r.start])
        }
        stop = stop.max(r.end);
    }
    result.push_str(&text[stop..]);
    ensure!(
        parse(&result)?.value == *expected,
        "safe edit changed unrelated settings"
    );
    Ok(result)
}

fn column(raw: &str, at: usize) -> usize {
    raw[..at].rsplit('\n').next().unwrap_or("").chars().count()
}
fn trailing_comment(raw: &str, at: usize) -> usize {
    let rest = raw[at..].trim_start_matches([' ', '\t']);
    if rest.starts_with("//") {
        let stop = rest.find('\n').unwrap_or(rest.len());
        return raw.len() - rest.len() + rest[..stop].trim_end_matches('\r').len();
    }
    if rest.starts_with("/*")
        && let Some(end) = rest.find("*/")
        && !rest[..end].contains('\n')
    {
        return raw.len() - rest.len() + end + 2;
    }
    at
}
pub(super) fn comments(text: &str) -> bool {
    let mut string = false;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if string && c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            string = !string
        } else if !string
            && c == '/'
            && (text[i..].starts_with("//") || text[i..].starts_with("/*"))
        {
            return true;
        }
    }
    false
}
pub(super) fn set_member(raw: &str, parent: &Node, key: &str, value: &Value) -> Result<String> {
    ensure!(parent.value.is_object(), "parent must be an object");
    let render = |width| -> Result<String> {
        Ok(serde_json::to_string_pretty(value)?.replace('\n', &format!("\n{}", " ".repeat(width))))
    };
    if let Some(c) = parent.children.iter().find(|c| c.key == key) {
        ensure!(
            !comments(&raw[c.value.span.clone()]),
            "entry contains comments"
        );
        let mut out = raw.to_owned();
        out.replace_range(c.value.span.clone(), &render(column(raw, c.start))?);
        return Ok(out);
    }
    if let Some(last) = parent.children.last() {
        let width = column(raw, last.start);
        let end = last.value.span.end;
        let end = last.comma.map(|p| p + 1).unwrap_or(end);
        let comment = trailing_comment(raw, end);
        let comma = if last.comma.is_some() { "" } else { "," };
        return Ok(format!(
            "{}{comma}{}\n{}{}: {}{}",
            &raw[..end],
            &raw[end..comment],
            " ".repeat(width),
            serde_json::to_string(key)?,
            render(width)?,
            &raw[comment..]
        ));
    }
    let start = parent.span.start + 1;
    let close = parent.span.end - 1;
    let outer = raw[..parent.span.start]
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .count();
    let width = outer + 2;
    let inner = &raw[start..close];
    Ok(format!(
        "{}{}\n{}{}: {}\n{}{}",
        &raw[..start],
        if comments(inner) {
            inner.trim_end()
        } else {
            ""
        },
        " ".repeat(width),
        serde_json::to_string(key)?,
        render(width)?,
        " ".repeat(outer),
        &raw[close..]
    ))
}
pub(super) fn only_comments(text: &str) -> bool {
    let mut c = Cursor { text, pos: 0 };
    c.peek().is_ok_and(|b| b == 0 && c.pos == text.len())
}
