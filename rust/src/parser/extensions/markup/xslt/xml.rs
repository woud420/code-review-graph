use super::super::super::syntax::{child, children, text};
use std::collections::BTreeMap;
use tree_sitter::Node;

pub(super) const NS: &str = "http://www.w3.org/1999/XSL/Transform";
pub(super) type Scope = BTreeMap<String, String>;

pub(super) struct Element<'t> {
    node: Node<'t>,
    pub name: String,
    pub attrs: BTreeMap<String, String>,
    pub scope: Scope,
}

impl<'t> Element<'t> {
    pub fn location(&self) -> Node<'t> {
        self.node
    }
    pub fn new(node: Node<'t>, src: &str, parent: &Scope) -> Self {
        let mut attrs = BTreeMap::new();
        let mut scope = parent.clone();
        let mut local = String::new();
        if let Some(tag) = children(node)
            .into_iter()
            .find(|c| matches!(c.kind(), "STag" | "EmptyElemTag"))
        {
            for attr in children(tag)
                .into_iter()
                .filter(|c| c.kind() == "Attribute")
            {
                if let (Some(name), Some(value)) = (child(attr, "Name"), child(attr, "AttValue")) {
                    let name = text(name, src);
                    let raw = text(value, src);
                    let value = unescape(raw.get(1..raw.len().saturating_sub(1)).unwrap_or(""));
                    if name == "xmlns" {
                        scope.insert(String::new(), value.clone());
                    } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                        scope.insert(prefix.into(), value.clone());
                    }
                    attrs.entry(name.into()).or_insert(value);
                }
            }
            if let Some(name) = child(tag, "Name") {
                let (prefix, name) = text(name, src)
                    .rsplit_once(':')
                    .unwrap_or(("", text(name, src)));
                if scope.get(prefix).is_some_and(|ns| ns == NS) {
                    local = name.into();
                }
            }
        }
        Self {
            node,
            name: local,
            attrs,
            scope,
        }
    }
    pub fn attr(&self, name: &str) -> String {
        self.attrs
            .get(name)
            .into_iter()
            .flat_map(|s| s.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub fn elements(&self, src: &str) -> Vec<Self> {
        let mut elements = Vec::new();
        for node in children(self.node) {
            if node.kind() == "element" {
                elements.push(Self::new(node, src, &self.scope));
            } else if node.kind() == "content" {
                elements.extend(
                    children(node)
                        .into_iter()
                        .filter(|c| c.kind() == "element")
                        .map(|c| Self::new(c, src, &self.scope)),
                );
            }
        }
        elements
    }
}

fn unescape(input: &str) -> String {
    let mut rest = input;
    let mut result = String::new();
    while let Some((prefix, tail)) = rest.split_once('&') {
        result.push_str(prefix);
        let Some((entity, after)) = tail.split_once(';') else {
            result.push('&');
            rest = tail;
            continue;
        };
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|n| u32::from_str_radix(n, 16).ok())
                .or_else(|| entity.strip_prefix('#')?.parse().ok())
                .and_then(char::from_u32),
        };
        if let Some(ch) = ch {
            result.push(ch);
            rest = after;
        } else {
            result.push('&');
            rest = tail;
        }
    }
    result.push_str(rest);
    result
}
