use super::*;
use regex::Regex;
use std::{collections::HashMap, sync::LazyLock};

pub(crate) fn supports(path: &Path) -> bool {
    static NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^application(?:-[A-Za-z0-9_.-]+)?\.(?:properties|ya?ml)$").unwrap()
    });
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| NAME.is_match(n))
}

fn normalize(s: &str) -> String {
    s.trim()
        .split('.')
        .map(|part| {
            let (base, idx) = part
                .rfind('[')
                .filter(|&start| {
                    part.ends_with(']')
                        && part[start + 1..part.len() - 1]
                            .bytes()
                            .all(|b| b.is_ascii_digit())
                })
                .map_or((part, ""), |start| part.split_at(start));
            let mut tokens = base.split(['-', '_']).filter(|s| !s.is_empty());
            let Some(first) = tokens.next() else {
                return idx.to_owned();
            };
            let words: Vec<_> = tokens.collect();
            let mut out = if !words.is_empty() || base.to_uppercase() == base {
                first.to_lowercase()
            } else {
                first.to_owned()
            };
            for word in words {
                let mut chars = word.chars();
                out.extend(chars.next().unwrap().to_uppercase());
                out.push_str(&chars.as_str().to_lowercase());
            }
            out.push_str(idx);
            out
        })
        .collect::<Vec<_>>()
        .join(".")
}

struct Properties<'a> {
    name: &'a str,
    ext: &'a str,
    doc: Option<usize>,
}

impl Properties<'_> {
    fn emit(&self, out: &mut Collector<'_>, raw: &str, ty: &str, first: usize, last: usize) {
        let id = normalize(raw);
        if id.is_empty() || out.file.yaml.declarations.contains_key(&id) {
            return;
        }
        let mut extra = json!({"raw_key":raw,"source_file":self.name,"value_type":ty});
        if let Some(doc) = self.doc {
            extra["document_index"] = json!(doc);
        }
        out.file.yaml.declarations.insert(
            id.clone(),
            Declaration {
                name: id.clone(),
                parent: None,
                extra,
            },
        );
        out.file.symbols.push(Symbol {
            name: id,
            kind: "ConfigProperty".into(),
            start_line: first,
            end_line: last,
        });
    }

    fn walk<'n>(
        &self,
        out: &mut Collector<'_>,
        node: Node<'n>,
        prefix: &str,
        anchors: &mut HashMap<String, Node<'n>>,
        parents: &mut HashSet<usize>,
    ) {
        let text = out.text;
        if let Some(a) = super::generic::decoration(node, "anchor") {
            anchors.insert(text[a.byte_range()].trim_start_matches('&').into(), node);
        }
        let n = peel(node);
        if !parents.insert(n.id()) {
            return;
        }
        match n.kind() {
            "flow_mapping" | "block_mapping" => {
                for (k, v) in entries(n) {
                    let Some((segment, _)) =
                        super::generic::key(k, text).filter(|(s, _)| !s.is_empty() && s != "<<")
                    else {
                        continue;
                    };
                    let raw = if prefix.is_empty() {
                        segment
                    } else {
                        format!("{prefix}.{segment}")
                    };
                    if let Some(v) = v {
                        self.walk(out, v, &raw, anchors, parents);
                    } else {
                        self.emit(out, &raw, "null", line(k), line(k));
                    }
                }
            }
            "flow_sequence" | "block_sequence" => {
                for (i, v) in items(n).into_iter().enumerate() {
                    self.walk(out, v, &format!("{prefix}[{i}]"), anchors, parents);
                }
            }
            "alias" => {
                if let Some(&target) = anchors.get(text[n.byte_range()].trim_start_matches('*')) {
                    self.walk(out, target, prefix, anchors, parents);
                }
            }
            _ if !prefix.is_empty() => {
                let ty = super::generic::scalar_type(node, text);
                self.emit(
                    out,
                    prefix,
                    if ty == "binary" { "scalar" } else { ty },
                    line(n),
                    last_line(n, text),
                );
            }
            _ => {}
        }
        parents.remove(&n.id());
    }

    fn format(&self, out: &mut Collector<'_>, lang: &str) {
        out.file.yaml.language = lang.into();
        out.file.yaml.file = json!({"config_format":self.ext});
    }
}

pub(super) fn yaml(path: &Path, docs: &[Node<'_>], out: &mut Collector<'_>) -> bool {
    if !supports(path) {
        return false;
    }
    for &doc in docs {
        if !matches!(peel(doc).kind(), "flow_mapping" | "block_mapping") {
            return false;
        }
        let names: HashSet<_> = entries(doc)
            .into_iter()
            .filter_map(|(k, _)| scalar(k, out.text))
            .collect();
        if [
            ("apiVersion", "kind"),
            ("jobs", "on"),
            ("openapi", "paths"),
            ("paths", "swagger"),
            ("services", "version"),
            ("AWSTemplateFormatVersion", "Resources"),
        ]
        .iter()
        .any(|(a, b)| names.contains(a) && names.contains(b))
        {
            return false;
        }
    }
    let mut props = Properties {
        name: path.file_name().and_then(|v| v.to_str()).unwrap_or(""),
        ext: path.extension().and_then(|v| v.to_str()).unwrap_or(""),
        doc: None,
    };
    props.format(out, "yaml");
    for (i, &doc) in docs.iter().enumerate() {
        props.doc = Some(i);
        props.walk(out, doc, "", &mut HashMap::new(), &mut HashSet::new());
    }
    true
}

pub(super) fn properties(path: &Path, out: &mut Collector<'_>) {
    let props = Properties {
        name: path.file_name().and_then(|v| v.to_str()).unwrap_or(""),
        ext: "properties",
        doc: None,
    };
    props.format(out, "properties");
    let emit = |out: &mut Collector<'_>, buf: &str, start: usize| {
        let s = buf.trim_start();
        if s.is_empty() || s.starts_with(['#', '!']) {
            return;
        }
        let mut escaped = false;
        let end = s
            .char_indices()
            .find_map(|(i, c)| {
                if c == '\\' {
                    escaped = !escaped;
                    return None;
                }
                let boundary = !escaped && (c.is_whitespace() || matches!(c, ':' | '='));
                escaped = false;
                boundary.then_some(i)
            })
            .unwrap_or(s.len());
        let mut key = s[..end].to_owned();
        for c in [' ', ':', '=', '!', '#'] {
            key = key.replace(&format!("\\{c}"), &c.to_string());
        }
        props.emit(out, &key, "scalar", start, start);
    };
    let mut buffer = String::new();
    let mut start = 1;
    for (index, line) in out.text.lines().enumerate() {
        if buffer.is_empty() {
            start = index + 1;
            buffer.push_str(line);
        } else {
            buffer.push_str(line.trim_start());
        }
        if buffer.bytes().rev().take_while(|b| *b == b'\\').count() % 2 == 1 {
            buffer.pop();
            continue;
        }
        emit(out, &buffer, start);
        buffer.clear();
    }
    if !buffer.is_empty() {
        emit(out, &buffer, start);
    }
}
