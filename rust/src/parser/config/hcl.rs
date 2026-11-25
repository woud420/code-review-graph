use crate::model::{ParsedFile, Symbol};
use anyhow::{Context, Result};
use std::collections::HashSet;
use tree_sitter::{Node, Parser};

fn child<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    node.named_children(&mut node.walk())
        .find(|n| n.kind() == kind)
}
fn text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}
fn attribute<'a>(body: Node<'_>, name: &str, src: &'a str) -> Option<&'a str> {
    for node in body
        .named_children(&mut body.walk())
        .filter(|n| n.kind() == "attribute")
    {
        if child(node, "identifier").is_some_and(|n| text(n, src) == name) {
            let expr = child(node, "expression")?;
            if name == "iterator" {
                return Some(text(expr, src).trim().trim_matches('"'));
            }
            let literal = child(expr, "literal_value")
                .and_then(|v| child(v, "string_lit"))
                .and_then(|s| child(s, "template_literal"));
            if let Some(literal) = literal {
                return Some(text(literal, src));
            }
        }
    }
    None
}
fn labels<'a>(block: Node<'_>, src: &'a str) -> Vec<&'a str> {
    block
        .named_children(&mut block.walk())
        .filter(|n| n.kind() == "string_lit")
        .filter_map(|n| child(n, "template_literal"))
        .map(|n| text(n, src))
        .collect()
}
fn reference(root: &str, attr: &[&str], provider: bool) -> Option<String> {
    if provider && !matches!(root, "each" | "count" | "self") {
        return Some(if let Some(first) = attr.first() {
            format!("provider.{root}.{first}")
        } else {
            format!("provider.{root}")
        });
    }
    match root {
        "var" | "local" | "module" => Some(format!("{root}.{}", attr.first()?)),
        "data" => Some(format!("data.{}.{}", attr.first()?, attr.get(1)?)),
        "each" | "count" | "self" | "path" | "terraform" => None,
        _ => Some(format!("resource.{root}.{}", attr.first()?)),
    }
}
fn recurses(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "expression"
            | "body"
            | "block"
            | "attribute"
            | "tuple"
            | "object"
            | "object_elem"
            | "collection_value"
            | "template_expr"
            | "for_expr"
            | "for_tuple_expr"
            | "for_object_expr"
            | "for_intro"
            | "for_cond"
            | "conditional"
            | "function_call"
            | "function_arguments"
            | "quoted_template"
            | "template_interpolation"
    )
}
fn iterators<'a>(node: Node<'_>, src: &'a str) -> Vec<&'a str> {
    if node.kind() == "block"
        && child(node, "identifier").is_some_and(|id| text(id, src) == "dynamic")
    {
        return child(node, "body")
            .and_then(|b| attribute(b, "iterator", src))
            .or_else(|| labels(node, src).first().copied())
            .into_iter()
            .collect();
    }
    if node.kind() == "for_expr" {
        let mut pending = vec![node];
        while let Some(n) = pending.pop() {
            if n.kind() == "for_intro" {
                return n
                    .named_children(&mut n.walk())
                    .filter(|c| c.kind() == "identifier")
                    .map(|c| text(c, src))
                    .collect();
            }
            pending.extend(n.named_children(&mut n.walk()));
        }
    }
    Vec::new()
}

struct Expressions<'s, 'f> {
    src: &'s str,
    owner: &'s str,
    provider_body: usize,
    provider_attr: &'s str,
    facts: &'f mut ParsedFile,
}
impl<'s> Expressions<'s, '_> {
    fn scan(&mut self, node: Node<'_>, locals: &HashSet<&'s str>, provider: bool) {
        let children: Vec<_> = node.named_children(&mut node.walk()).collect();
        if matches!(node.kind(), "expression" | "body") {
            for (i, v) in children
                .iter()
                .enumerate()
                .filter(|(_, n)| n.kind() == "variable_expr")
            {
                let Some(id) = child(*v, "identifier") else {
                    continue;
                };
                let root = text(id, self.src);
                if locals.contains(root) {
                    continue;
                }
                let attrs: Vec<_> = children[i + 1..]
                    .iter()
                    .take_while(|n| n.kind() == "get_attr")
                    .filter_map(|n| child(*n, "identifier"))
                    .map(|n| text(n, self.src))
                    .collect();
                if let Some(target) = reference(root, &attrs, provider) {
                    self.facts.hcl.references.push((
                        self.owner.into(),
                        target,
                        v.start_position().row + 1,
                    ));
                }
            }
        }
        for n in children.into_iter().filter(|n| recurses(*n)) {
            if provider
                && node.kind() == "object_elem"
                && node.child_by_field_name("key") == Some(n)
            {
                continue;
            }
            let next_provider = if n.kind() == "attribute" {
                node.id() == self.provider_body
                    && child(n, "identifier")
                        .is_some_and(|k| text(k, self.src) == self.provider_attr)
            } else {
                provider
            };
            let extra = iterators(n, self.src);
            if extra.is_empty() {
                self.scan(n, locals, next_provider);
            } else {
                let mut scoped = locals.clone();
                scoped.extend(extra);
                self.scan(n, &scoped, next_provider);
            }
        }
    }
}

fn symbol(out: &mut ParsedFile, node: Node<'_>, name: &str, kind: &str, block: &str) {
    out.symbols.push(Symbol {
        name: name.into(),
        kind: kind.into(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    });
    out.hcl.types.insert(name.into(), block.into());
}
fn extract(out: &mut ParsedFile, node: Node<'_>, src: &str) {
    if node.kind() != "block" {
        for n in node.named_children(&mut node.walk()) {
            extract(out, n, src);
        }
        return;
    }
    let Some(type_node) = child(node, "identifier") else {
        return;
    };
    let kind = text(type_node, src);
    let body = child(node, "body");
    if kind == "locals" {
        if let Some(body) = body {
            for a in body
                .named_children(&mut body.walk())
                .filter(|n| n.kind() == "attribute")
            {
                let Some(k) = child(a, "identifier") else {
                    continue;
                };
                let name = format!("local.{}", text(k, src));
                symbol(out, a, &name, "Function", "local");
                if let Some(e) = child(a, "expression") {
                    Expressions {
                        src,
                        owner: &name,
                        provider_body: a.id(),
                        provider_attr: "",
                        facts: out,
                    }
                    .scan(e, &HashSet::new(), false);
                }
            }
        }
        return;
    }
    let (node_kind, prefix, arity) = match kind {
        "resource" | "data" => ("Class", kind, 2),
        "module" => ("Class", kind, 1),
        "variable" => ("Function", "var", 1),
        "output" | "provider" => ("Function", kind, 1),
        _ => return,
    };
    let parts = labels(node, src);
    if parts.len() < arity {
        return;
    }
    let mut name = format!("{prefix}.{}", parts[..arity].join("."));
    if kind == "provider"
        && let Some(alias) = body
            .and_then(|b| attribute(b, "alias", src))
            .filter(|s| !s.is_empty())
    {
        name.push('.');
        name.push_str(alias);
    }
    symbol(out, node, &name, node_kind, kind);
    if let Some(b) = body {
        if kind == "module"
            && let Some(source) = attribute(b, "source", src).filter(|s| !s.is_empty())
        {
            out.hcl
                .modules
                .push((source.into(), node.start_position().row + 1));
        }
        if kind != "variable" {
            let provider_attr = match kind {
                "module" => "providers",
                "resource" | "data" => "provider",
                _ => "",
            };
            Expressions {
                src,
                owner: &name,
                provider_body: b.id(),
                provider_attr,
                facts: out,
            }
            .scan(b, &HashSet::new(), false);
        }
    }
}
pub(in crate::parser) fn parse(src: &str) -> Result<ParsedFile> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_hcl::LANGUAGE.into())?;
    let tree = parser.parse(src, None).context("parsing HCL")?;
    let mut file = ParsedFile::default();
    extract(&mut file, tree.root_node(), src);
    Ok(file)
}
