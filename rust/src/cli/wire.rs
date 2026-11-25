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
    let mut text = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if in_string && !ch.is_ascii() {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                write!(text, "\\u{unit:04x}").expect("String writer");
            }
        } else {
            text.push(ch);
            if !pretty && !in_string && matches!(ch, ',' | ':') {
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

fn python_repr(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::from(quote);
            for c in s.chars() {
                match c {
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\\' => out.push_str("\\\\"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c if c.is_control() => {
                        write!(out, "\\x{:02x}", c as u32).expect("String writer");
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(python_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!(
                    "{}: {}",
                    python_repr(&Value::String(k.clone())),
                    python_repr(v)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => n.to_string(),
    }
}

pub fn dict_repr(map: &serde_json::Map<String, Value>) -> String {
    let raw = python_repr(&Value::Object(map.clone()));
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
