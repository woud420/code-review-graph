mod imports;
mod scopes;

use crate::model::go::{Definition, Selector};
use crate::model::{Call, ParsedFile, Symbol};
pub(crate) use imports::package_dir;
use serde_json::{Value, json};
use std::path::Path;
use tree_sitter::Node;

fn text<'s>(node: Node<'_>, s: &'s str) -> &'s str {
    &s[node.byte_range()]
}
fn field<'s>(node: Node<'_>, key: &str, s: &'s str) -> Option<&'s str> {
    node.child_by_field_name(key).map(|n| text(n, s))
}
fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.named_children(&mut node.walk()).collect()
}
fn first(node: Node<'_>) -> Option<Node<'_>> {
    node.named_children(&mut node.walk())
        .find(|n| n.kind() != "comment")
}

fn doc(node: Node<'_>, source: &str) -> Option<String> {
    let mut row = node.start_position().row;
    let mut comments = Vec::new();
    let mut previous = node.prev_named_sibling();
    while let Some(p) = previous {
        if p.kind() != "comment" || p.end_position().row + 1 < row {
            break;
        }
        let t = text(p, source).trim();
        if let Some(line) = t.strip_prefix("//") {
            let line = line.strip_prefix(' ').unwrap_or(line);
            if !line.starts_with("go:") && !line.starts_with("line ") {
                comments.push(line.to_owned());
            }
        } else if comments.is_empty() && (t.starts_with("/**") || t.starts_with("/*!")) {
            let body = t[3..].strip_suffix("*/")?.trim();
            return Some(
                body.lines()
                    .map(|l| l.trim().trim_start_matches('*').trim_start())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        } else {
            break;
        }
        row = p.start_position().row;
        previous = p.prev_named_sibling();
    }
    comments.reverse();
    (!comments.is_empty()).then(|| comments.join("\n"))
}

fn base<'a>(mut n: Node<'_>, source: &'a str) -> Option<&'a str> {
    while matches!(
        n.kind(),
        "generic_type" | "pointer_type" | "parenthesized_type"
    ) {
        n = first(n)?;
    }
    matches!(n.kind(), "type_identifier" | "qualified_type").then(|| text(n, source))
}

fn embeddings(spec: Node<'_>, s: &str) -> Vec<String> {
    let Some(ty) = spec.child_by_field_name("type") else {
        return Vec::new();
    };
    let fields = match ty.kind() {
        "struct_type" => first(ty).map(children).unwrap_or_default(),
        "interface_type" => children(ty),
        _ => return Vec::new(),
    };
    fields
        .into_iter()
        .filter_map(|n| {
            let elem = match n.kind() {
                "field_declaration" if n.child_by_field_name("name").is_none() => {
                    n.child_by_field_name("type")?
                }
                "type_elem" => {
                    let mut terms = n
                        .named_children(&mut n.walk())
                        .filter(|t| t.kind() != "comment")
                        .collect::<Vec<_>>();
                    if terms.len() != 1 {
                        return None;
                    }
                    let term = terms.pop()?;
                    if !matches!(
                        term.kind(),
                        "generic_type" | "type_identifier" | "qualified_type"
                    ) {
                        return None;
                    }
                    term
                }
                _ => return None,
            };
            base(elem, s).map(str::to_owned)
        })
        .collect()
}

fn add(f: &mut ParsedFile, n: Node<'_>, name: String, kind: &str, details: Definition) {
    f.go.declarations.insert(name.clone(), details);
    f.symbols.push(Symbol {
        name,
        kind: kind.into(),
        start_line: n.start_position().row + 1,
        end_line: n.end_position().row + 1,
    });
}

fn classes(f: &mut ParsedFile, decl: Node<'_>, s: &str) {
    let grouped = decl.children(&mut decl.walk()).any(|c| c.kind() == "(");
    for spec in children(decl)
        .into_iter()
        .filter(|n| n.kind() == "type_spec")
    {
        if let Some(name) = field(spec, "name", s) {
            let span = if grouped { spec } else { decl };
            add(
                f,
                span,
                name.into(),
                "Class",
                Definition {
                    doc: doc(span, s),
                    bases: embeddings(spec, s),
                    ..Default::default()
                },
            );
        }
    }
}

struct Method<'a> {
    variable: &'a str,
    parent: &'a str,
    shadow: Vec<std::ops::Range<usize>>,
}

fn calls(f: &mut ParsedFile, top: Node<'_>, s: &str, caller: &str, method: Option<&Method>) {
    let mut pending = vec![top];
    while let Some(n) = pending.pop() {
        if n.kind() == "if_statement" {
            let cond = n.child_by_field_name("condition");
            if let Some(literal) =
                cond.map(|v| text(v, s).trim_matches(|c| matches!(c, '(' | ')' | ' ')))
            {
                let skip = match literal {
                    "false" => n.child_by_field_name("consequence"),
                    _ => None,
                };
                if let Some(skip) = skip {
                    pending.extend(children(n).into_iter().rev().filter(|v| *v != skip));
                    continue;
                }
            }
        }
        if n.kind() == "call_expression"
            && let Some(fun) = n.child_by_field_name("function")
        {
            let line = n.start_position().row + 1;
            let mut callee = None;
            if matches!(fun.kind(), "identifier" | "type_identifier") {
                callee = Some(text(fun, s).to_owned());
            } else if fun.kind() == "selector_expression"
                && let (Some(mut receiver), Some(name)) =
                    (fun.child_by_field_name("operand"), field(fun, "field", s))
            {
                while receiver.kind() == "parenthesized_expression"
                    && let Some(child) = first(receiver)
                {
                    receiver = child;
                }
                if receiver.kind() == "unary_expression"
                    && receiver.child(0).is_some_and(|op| op.kind() == "*")
                    && let Some(v) = receiver.child_by_field_name("operand")
                {
                    receiver = v;
                }
                let value = text(receiver, s);
                let binding = method.filter(|m| {
                    m.variable == value && !m.shadow.iter().any(|r| r.contains(&n.start_byte()))
                });
                let raw = format!("{}.{name}", binding.map_or(value, |m| m.parent));
                f.go.selectors.entry(line).or_default().push(Selector {
                    caller: caller.into(),
                    raw: raw.clone(),
                    receiver: value.into(),
                    own: binding.is_some(),
                });
                callee = Some(raw);
            }
            if let Some(callee) = callee {
                f.calls.push(Call {
                    caller: caller.into(),
                    callee,
                    line,
                });
            }
        }
        if n.kind() == "type_declaration" {
            classes(f, n, s);
            continue;
        }
        pending.extend(children(n).into_iter().rev());
    }
}

pub fn extract(path: &Path, root: Node<'_>, s: &str) -> ParsedFile {
    let mut f = ParsedFile::default();
    let test_file = crate::model::test_file(&path.to_string_lossy(), None);
    for n in children(root) {
        match n.kind() {
            "type_declaration" => classes(&mut f, n, s),
            "import_declaration" => {
                let mut group = children(n);
                while let Some(spec) = group.pop() {
                    if spec.kind() == "import_spec" {
                        if let Some(path) = field(spec, "path", s) {
                            f.go.imports.push((
                                spec.start_position().row + 1,
                                path.trim_matches(['"', '`']).into(),
                            ));
                        }
                    } else {
                        group.extend(children(spec));
                    }
                }
            }
            "method_declaration" | "function_declaration" => {
                let Some(name) = field(n, "name", s) else {
                    continue;
                };
                let receiver = n.child_by_field_name("receiver");
                let param = receiver.and_then(first);
                let parent = param
                    .and_then(|p| p.child_by_field_name("type"))
                    .and_then(|v| base(v, s));
                let var = param.and_then(|p| field(p, "name", s));
                let ident = parent.map_or_else(|| name.to_owned(), |p| format!("{p}.{name}"));
                let details = Definition {
                    params: receiver
                        .or_else(|| n.child_by_field_name("parameters"))
                        .map(|v| text(v, s).to_owned()),
                    doc: doc(n, s),
                    extra: var.map_or(Value::Null, |v| json!({"go_receiver":v})),
                    ..Default::default()
                };
                let is_test = name.starts_with("Test")
                    || name.starts_with("test_")
                    || name.ends_with("_test")
                    || name.ends_with("_spec")
                    || (test_file
                        && matches!(
                            name,
                            "describe"
                                | "it"
                                | "test"
                                | "beforeEach"
                                | "afterEach"
                                | "beforeAll"
                                | "afterAll"
                                | "suite"
                        ));
                add(
                    &mut f,
                    n,
                    ident.clone(),
                    if is_test { "Test" } else { "Function" },
                    details,
                );
                if let Some(body) = n.child_by_field_name("body") {
                    let method = var.zip(parent).map(|(variable, parent)| Method {
                        variable,
                        parent,
                        shadow: scopes::shadows(body, variable, s),
                    });
                    calls(&mut f, body, s, &ident, method.as_ref());
                }
            }
            _ => calls(&mut f, n, s, "<module>", None),
        }
    }
    f
}
