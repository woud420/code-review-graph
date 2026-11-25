use tree_sitter::{Node, Parser, Tree};

pub(in crate::parser) fn tree(input: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .ok()?;
    parser
        .parse(input, None)
        .filter(|t| !t.root_node().has_error())
}

fn peel(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "document" | "block_node" | "flow_node" | "block_sequence_item"
    ) {
        let mut walk = node.walk();
        let Some(next) = node.named_children(&mut walk).find(|n| {
            !matches!(
                n.kind(),
                "comment" | "anchor" | "tag" | "yaml_directive" | "tag_directive"
            )
        }) else {
            break;
        };
        node = next;
    }
    node
}

fn entries(map: Node<'_>) -> Vec<(Node<'_>, Option<Node<'_>>)> {
    let map = peel(map);
    map.named_children(&mut map.walk())
        .filter_map(|n| {
            Some((
                n.child_by_field_name("key")?,
                n.child_by_field_name("value"),
            ))
        })
        .collect()
}

fn scalar<'a>(node: Node<'_>, input: &'a str) -> Option<&'a str> {
    let n = peel(node);
    matches!(
        n.kind(),
        "plain_scalar" | "double_quote_scalar" | "single_quote_scalar"
    )
    .then(|| input[n.byte_range()].trim_matches(['\'', '"']))
}

pub(in crate::parser) fn model_paths(input: &str) -> Vec<String> {
    let fallback = || vec!["models".to_owned()];
    let Some(tree) = tree(input) else {
        return fallback();
    };
    let root = tree.root_node();
    let Some(doc) = root
        .named_children(&mut root.walk())
        .find(|n| n.kind() == "document")
    else {
        return fallback();
    };
    let Some(paths) = entries(doc).into_iter().find_map(|(k, v)| {
        (scalar(k, input) == Some("model-paths"))
            .then_some(v)
            .flatten()
    }) else {
        return fallback();
    };
    let paths = peel(paths);
    if let Some(path) = scalar(paths, input) {
        return vec![path]
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if !matches!(paths.kind(), "flow_sequence" | "block_sequence") {
        return fallback();
    }
    paths
        .named_children(&mut paths.walk())
        .filter_map(|n| scalar(n, input))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

mod ansible;
mod generic;
pub(crate) mod spring;

use crate::model::{
    Call, ParsedFile, Symbol,
    yaml::{Declaration, Edge},
};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};

fn items(node: Node<'_>) -> Vec<Node<'_>> {
    let seq = peel(node);
    if !matches!(seq.kind(), "block_sequence" | "flow_sequence") {
        return Vec::new();
    }
    seq.named_children(&mut seq.walk())
        .filter(|n| n.kind() != "comment")
        .collect()
}

fn value<'a>(node: Node<'a>, input: &str, key: &str) -> Option<Node<'a>> {
    entries(node)
        .into_iter()
        .find_map(|(k, v)| (scalar(k, input) == Some(key)).then_some(v).flatten())
}

fn line(node: Node<'_>) -> usize {
    peel(node).start_position().row + 1
}

fn last_line(node: Node<'_>, source: &str) -> usize {
    let node = peel(node);
    let slice = &source[node.byte_range()];
    node.start_position().row
        + slice
            .trim_end()
            .as_bytes()
            .iter()
            .filter(|b| **b == b'\n')
            .count()
        + 1
}

struct Collector<'a> {
    text: &'a str,
    file: ParsedFile,
    contains: HashSet<(String, String)>,
}

impl Collector<'_> {
    fn declare(&mut self, symbol: Symbol, d: Declaration, owner: &str) {
        self.edge(
            "CONTAINS",
            owner,
            &symbol.name,
            true,
            symbol.start_line,
            json!({}),
        );
        self.file.yaml.declarations.insert(symbol.name.clone(), d);
        self.file.symbols.push(symbol);
    }

    fn edge(&mut self, kind: &str, from: &str, to: &str, local: bool, line: usize, extra: Value) {
        if kind == "CONTAINS" && !self.contains.insert((from.into(), to.into())) {
            return;
        }
        self.file.yaml.edges.push(Edge {
            kind: kind.into(),
            source: from.into(),
            target: to.into(),
            local,
            line,
            extra,
        });
    }
}

pub(in crate::parser) fn parse(path: &Path, text: &str) -> ParsedFile {
    let mut out = Collector {
        text,
        file: ParsedFile::default(),
        contains: HashSet::new(),
    };
    if path.extension().is_some_and(|e| e == "properties") {
        spring::properties(path, &mut out);
        return out.file;
    }
    if text.len() > 1024 * 1024 {
        return out.file;
    }
    if generic::locale(path) {
        out.file.yaml.language = "yaml".into();
        out.file.yaml.file = json!({"config_format":path.extension().and_then(|s|s.to_str()),"yaml_locale_file":true,"values_indexed":false});
        return out.file;
    }
    let Some(tree) = tree(text) else {
        return out.file;
    };
    let root = tree.root_node();
    let docs: Vec<_> = root
        .named_children(&mut root.walk())
        .filter(|n| n.kind() == "document")
        .collect();
    if text.contains(['&', '*']) && docs.iter().any(|doc| !valid_anchors(*doc, text)) {
        return out.file;
    }
    if let Some(doc) = docs.first()
        && ansible::parse(path, *doc, &mut out)
    {
        return out.file;
    }
    if !spring::yaml(path, &docs, &mut out) {
        generic::parse(path, &docs, &mut out);
    }
    out.file
}

fn valid_anchors(doc: Node<'_>, text: &str) -> bool {
    let mut defined = HashSet::new();
    let mut pending = vec![doc];
    while let Some(n) = pending.pop() {
        match n.kind() {
            "anchor" if !defined.insert(text[n.byte_range()].trim_start_matches('&')) => {
                return false;
            }
            "alias" if !defined.contains(text[n.byte_range()].trim_start_matches('*')) => {
                return false;
            }
            _ => {}
        }
        let children: Vec<_> = n.named_children(&mut n.walk()).collect();
        pending.extend(children.into_iter().rev());
    }
    true
}
