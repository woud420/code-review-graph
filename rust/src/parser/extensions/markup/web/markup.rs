use super::super::super::syntax::{child, children, text};
use super::super::declare;
use crate::model::{ParsedFile, syntax::Definition};
use serde_json::json;
use std::collections::BTreeMap;
use tree_sitter::Node;

pub(super) fn template(value: &str) -> bool {
    ["{{", "{%", "<%", "${"]
        .iter()
        .any(|mark| value.contains(mark))
}
pub(super) fn attrs(tag: Node<'_>, s: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for n in children(tag)
        .into_iter()
        .filter(|c| c.kind() == "attribute")
    {
        if let Some(name) = child(n, "attribute_name") {
            let val = child(n, "quoted_attribute_value")
                .and_then(|v| child(v, "attribute_value"))
                .or_else(|| child(n, "attribute_value"))
                .map_or("", |v| text(v, s));
            values
                .entry(text(name, s).to_lowercase())
                .or_insert_with(|| val.into());
        }
    }
    values
}
fn tag_name<'s>(n: Node<'_>, s: &'s str) -> Option<&'s str> {
    child(n, "start_tag")
        .or_else(|| child(n, "self_closing_tag"))
        .and_then(|v| child(v, "tag_name"))
        .map(|v| text(v, s))
}
fn heading(n: Node<'_>, s: &str) -> Option<String> {
    let mut stack: Vec<_> = children(n).into_iter().rev().collect();
    while let Some(v) = stack.pop() {
        if v.kind() != "element" {
            continue;
        }
        let tag = tag_name(v, s)?.to_lowercase();
        if matches!(tag.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            let mut pieces = Vec::new();
            let mut nodes = vec![v];
            while let Some(c) = nodes.pop() {
                match c.kind() {
                    "text" | "entity" => pieces.push(text(c, s)),
                    "start_tag" | "end_tag" => {}
                    _ => nodes.extend(children(c).into_iter().rev()),
                }
            }
            let heading = pieces
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if !heading.is_empty() {
                return Some(heading.chars().take(200).collect());
            }
        } else if !(matches!(tag.as_str(), "div" | "article" | "section")
            && child(v, "start_tag").is_some_and(|t| {
                attrs(t, s)
                    .get("id")
                    .is_some_and(|id| !id.trim().is_empty())
            }))
        {
            stack.extend(children(v).into_iter().rev());
        }
    }
    None
}
pub(super) fn section(
    file: &mut ParsedFile,
    handlers: &mut Vec<(String, String, usize)>,
    n: Node<'_>,
    s: &str,
) {
    let Some(tag) = child(n, "start_tag").or_else(|| child(n, "self_closing_tag")) else {
        return;
    };
    let name = tag_name(n, s).unwrap_or("").to_lowercase();
    let attributes = attrs(tag, s);
    for (k, v) in &attributes {
        if k.starts_with("on") && !v.is_empty() {
            handlers.push((k.clone(), v.clone(), tag.start_position().row + 1));
        }
    }
    let Some(id) = attributes
        .get("id")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty() && !template(v))
    else {
        return;
    };
    if !matches!(name.as_str(), "div" | "article" | "section") {
        return;
    }
    let key = format!("#{id}");
    if let Some(def) = file.syntax.declarations.get_mut(&key) {
        def.extra["html_id_occurrences"] =
            json!(def.extra["html_id_occurrences"].as_u64().unwrap_or(1) + 1);
        return;
    }
    if file.symbols.len() >= 500 {
        file.syntax.file["html_truncated"] = json!(true);
        return;
    }
    let mut extra = json!({"html_id":id,"html_tag":name,"html_id_occurrences":1});
    let doc = heading(n, s);
    if let Some(v) = &doc {
        extra["docstring"] = json!(v);
    }
    file.syntax.display_names.insert(key.clone(), key.clone());
    declare(
        file,
        n,
        &key,
        "DocSection",
        Definition {
            doc,
            extra,
            ..Default::default()
        },
    );
}
