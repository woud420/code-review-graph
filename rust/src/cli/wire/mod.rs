pub(super) mod diff;
use serde_json::Value;
use std::fmt::Write;

/// Match json.dumps, including ASCII escapes and spaces in single-line CLI output.
pub fn json_text(value: &Value, pretty: bool) -> String {
    let raw = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .expect("JSON value");
    ascii(&raw, !pretty)
}

pub(in crate::cli) fn ascii(raw: &str, space_after_separators: bool) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if in_string && (!ch.is_ascii() || ch == '\u{7f}') {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                write!(text, "\\u{unit:04x}").expect("String writer");
            }
        } else {
            text.push(ch);
            if space_after_separators && !in_string && matches!(ch, ',' | ':') {
                text.push(' ');
            }
        }
        if ch == '"' && !escaped {
            in_string = !in_string;
        }
        escaped = in_string && ch == '\\' && !escaped;
    }
    text
}

pub(super) use code_review_graph_rs::model::value_repr;

pub fn dict_repr(map: &serde_json::Map<String, Value>) -> String {
    let raw = value_repr(&Value::Object(map.clone()));
    let chars: Vec<_> = raw.chars().collect();
    if chars.len() > 50 {
        format!(
            "{}...{}",
            chars[..25].iter().collect::<String>(),
            chars[chars.len() - 24..].iter().collect::<String>()
        )
    } else {
        raw
    }
}
