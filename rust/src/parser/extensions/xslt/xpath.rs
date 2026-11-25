use regex::Regex;
use std::{collections::BTreeSet, sync::LazyLock};

fn split(expression: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    let mut quote = None;
    for (i, ch) in expression.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
        } else {
            match ch {
                '\'' | '"' => quote = Some(ch),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                _ if ch == sep && depth == 0 => {
                    parts.push(&expression[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    parts.push(&expression[start..]);
    parts
}
pub(super) fn steps(expression: &str) -> BTreeSet<String> {
    static STEP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^@?[^\W\d][\w.\-]*(?::[^\W\d][\w.\-]*)?$").unwrap());
    let mut result = BTreeSet::new();
    for alt in split(expression, '|') {
        let path = split(alt, '/');
        let last = path.last().unwrap().split('[').next().unwrap().trim();
        let name = last.strip_prefix("child::").unwrap_or(last);
        if !STEP.is_match(name) {
            return Default::default();
        }
        result.insert(name.into());
    }
    result
}
