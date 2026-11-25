use regex::Regex;
use std::sync::LazyLock;

pub(super) const IDENT: &str = r"(?:[A-Za-z_][A-Za-z0-9_]*|\[[^\]\r\n]+\])";
pub(super) fn dotted() -> String {
    format!(r"{IDENT}(?:\.{IDENT})*")
}
const MODIFIERS: &str = "Public|Private|Protected|Friend|Partial|Shared|Static|MustInherit|NotInheritable|Overridable|Overrides|MustOverride|Overloads|Default|ReadOnly|WriteOnly|Shadows|Async|Iterator|Declare|Narrowing|Widening";
pub(super) static DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)^((?:(?:{MODIFIERS})\s+)*)(Class|Interface|Structure|Module|Enum|Function|Sub|Property)\s+({IDENT})\b(.*)$")).unwrap()
});
pub(super) static OPERATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^((?:(?:{MODIFIERS})\s+)*)(Operator)\s+(\S+)\s*(.*)$"
    ))
    .unwrap()
});
pub(super) static NAMESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)^Namespace\s+({})$", dotted())).unwrap());
pub(super) static REL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(Inherits|Implements)\s+(.+)$").unwrap());
pub(super) static NEW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\bNew\s+({})\s*(?:\(Of\b[^)]*\))?\s*\(",
        dotted()
    ))
    .unwrap()
});
pub(super) static CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)({})\s*\(", dotted())).unwrap());
static RETURN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bAs\s+(.+?)(?:\s+(?:Implements|Handles)\b|$)").unwrap());
static REL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)^({})(?:\s*\(Of\b.*\))?$", dotted())).unwrap());
pub(super) fn name(raw: &str) -> String {
    raw.trim()
        .split('.')
        .map(|p| {
            p.strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(p)
        })
        .collect::<Vec<_>>()
        .join(".")
}
fn group(s: &str) -> Option<(&str, &str)> {
    if !s.starts_with('(') {
        return None;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}
fn split(s: &str) -> Vec<&str> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub(super) fn signature(rest: &str) -> (Vec<String>, Option<String>, Option<String>) {
    let mut tail = rest.trim_start();
    let mut generics = Vec::new();
    if tail
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("(of "))
        && let Some((params, after)) = group(tail)
    {
        generics = split(&params[3..])
            .into_iter()
            .filter_map(|p| p.split_whitespace().next().map(str::to_owned))
            .collect();
        tail = after.trim_start();
    }
    let params = group(tail).map(|(params, after)| {
        tail = after;
        collapse(params)
    });
    let returns = RETURN.captures(tail).map(|c| collapse(&c[1]));
    (generics, params, returns)
}
pub(super) fn targets(rest: &str) -> Vec<String> {
    split(rest)
        .into_iter()
        .filter_map(|p| {
            let p = p.split_once('=').map_or(p, |(_, p)| p).trim();
            let p = if p
                .get(..7)
                .is_some_and(|x| x.eq_ignore_ascii_case("global."))
            {
                &p[7..]
            } else {
                p
            };
            REL_NAME.captures(p).map(|c| name(&c[1]))
        })
        .collect()
}
pub(super) fn statements(source: &str) -> Vec<(usize, usize, String)> {
    let mut result = Vec::new();
    let mut parts = Vec::new();
    let mut first = 1;
    let mut depth = 0isize;
    let mut last = 1;
    for (i, line) in source.lines().enumerate() {
        last = i + 1;
        let mut output = String::new();
        let mut quoted = false;
        let mut chars = line.chars().peekable();
        if !line
            .split_whitespace()
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case("rem"))
        {
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        output.push('"');
                        if quoted && chars.next_if_eq(&'"').is_some() {
                            output.push(' ');
                        } else {
                            quoted = !quoted;
                        }
                    }
                    '\'' if !quoted => break,
                    _ => output.push(if quoted { ' ' } else { c }),
                }
            }
        }
        let line = output.trim();
        if line.is_empty() && parts.is_empty() {
            continue;
        }
        if parts.is_empty() {
            first = last;
        }
        let part = line.strip_suffix('_').unwrap_or(line).trim_end();
        parts.push(part.to_owned());
        depth += part.matches('(').count() as isize - part.matches(')').count() as isize;
        if !line.ends_with('_') && depth <= 0 {
            result.push((first, last, parts.join(" ")));
            parts.clear();
            depth = 0;
        }
    }
    if !parts.is_empty() {
        result.push((first, last, parts.join(" ")));
    }
    result
}
pub(super) fn call_keyword(s: &str) -> bool {
    const KEYWORDS: &str = "addhandler and andalso as call case catch class cobj continue ctype directcast do each else elseif end enum erase error event exit finally for function get gettype getxmlnamespace global gosub goto if implements imports inherits interface loop module mustinherit new next not nothing operator option or orelse property raiseevent redim rem removehandler resume return select set step stop structure sub synclock then throw to try typeof until using when while with withevents xor";
    KEYWORDS
        .split_ascii_whitespace()
        .any(|k| s.eq_ignore_ascii_case(k))
}
