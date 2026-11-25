use regex::Regex;
use std::{collections::HashMap, sync::LazyLock};

pub(super) fn annotation(decorator: &str) -> &str {
    decorator
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('@')
        .rsplit('.')
        .next()
        .unwrap_or("")
        .trim()
}
pub(super) fn argument(decorator: &str) -> Option<&str> {
    static NAMED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?:pointcut|value)\s*=\s*"((?:[^"\\]|\\.)*)""#).unwrap());
    static QUOTED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap());
    let capture = NAMED
        .captures(decorator)
        .or_else(|| QUOTED.captures(decorator))?;
    let whole = capture.get(0)?;
    if decorator[..whole.start()].trim_end().ends_with('+')
        || decorator[whole.end()..].trim_start().starts_with('+')
    {
        return None;
    }
    capture.get(1).map(|v| v.as_str()).filter(|s| !s.is_empty())
}

pub(super) enum Selector<'a> {
    Annotation(&'a str),
    Execution {
        parts: Vec<Option<&'a str>>,
        scope: bool,
    },
}
pub(super) struct Pointcut<'a> {
    pub expr: &'a str,
    filter: Selector<'a>,
}
impl<'a> Pointcut<'a> {
    pub fn parse(mut expr: &'a str, named: &HashMap<&str, &'a str>) -> Option<Self> {
        static REFERENCE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\(\s*\)\s*$").unwrap());
        if let Some(cap) = REFERENCE.captures(expr) {
            expr = *named.get(cap.get(1)?.as_str())?;
        }
        if expr.contains("&&") || expr.contains("||") {
            return None;
        }
        let (designator, body) = expr.trim().split_once('(')?;
        let body = body.strip_suffix(')')?.trim();
        let filter = match designator.trim() {
            "@annotation" => {
                let name = body.rsplit('.').next()?.trim();
                if name.is_empty() {
                    return None;
                }
                Selector::Annotation(name)
            }
            "execution" => {
                let (sig, _) = body.strip_suffix(')')?.rsplit_once('(')?;
                let mut tokens = sig.split_whitespace();
                if tokens.clone().any(|t| t.starts_with(['@', '!'])) {
                    return None;
                }
                let pattern = tokens.next_back()?;
                if pattern.ends_with('.') {
                    return None;
                }
                let mut parts = Vec::new();
                if pattern.starts_with("*.") && pattern.bytes().filter(|&b| b == b'.').count() == 1
                {
                    parts.push(None);
                }
                for (i, run) in pattern.split("..").enumerate() {
                    if i > 0 {
                        parts.push(None);
                    }
                    if !run.is_empty() {
                        parts.extend(run.split('.').map(Some));
                    }
                }
                Selector::Execution {
                    parts,
                    scope: pattern.contains('.'),
                }
            }
            _ => return None,
        };
        Some(Self { expr, filter })
    }
    pub fn kind(&self) -> &'static str {
        match self.filter {
            Selector::Annotation(_) => "@annotation",
            Selector::Execution { .. } => "execution",
        }
    }
    pub fn applies(&self, method: &str, qualified: &str, decorators: &[String]) -> bool {
        match &self.filter {
            Selector::Annotation(name) => decorators.iter().any(|d| annotation(d) == *name),
            Selector::Execution { parts, scope } => {
                dotted(parts, if *scope { qualified } else { method })
            }
        }
    }
}

fn dotted(parts: &[Option<&str>], subject: &str) -> bool {
    let segments: Vec<_> = subject.split('.').collect();
    if segments.contains(&"") {
        return false;
    }
    let mut positions = vec![0];
    for part in parts {
        if let Some(pattern) = part {
            positions = positions
                .into_iter()
                .filter(|&i| i < segments.len() && wildcard(pattern, segments[i]))
                .map(|i| i + 1)
                .collect();
        } else {
            positions = (positions[0]..=segments.len()).collect();
        }
        if positions.is_empty() {
            return false;
        }
    }
    positions.last() == Some(&segments.len())
}
// Literal runs separated by stars can each consume the earliest remaining match.
fn wildcard(pattern: &str, text: &str) -> bool {
    let Some((first, tail)) = pattern.split_once('*') else {
        return pattern == text;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let mut parts = tail.split('*').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return rest.ends_with(part);
        }
        let Some((_, next)) = rest.split_once(part) else {
            return false;
        };
        rest = next;
    }
    true
}
