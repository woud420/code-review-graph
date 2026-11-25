mod annotations;
mod calls;
mod events;

use crate::model::{
    ParsedFile, Symbol,
    syntax::{Definition, Edge},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tree_sitter::Node;

fn text<'s>(n: Node<'_>, s: &'s str) -> &'s str {
    &s[n.byte_range()]
}
fn children(n: Node<'_>) -> Vec<Node<'_>> {
    n.named_children(&mut n.walk()).collect()
}
fn field<'s>(n: Node<'_>, key: &str, s: &'s str) -> Option<&'s str> {
    n.child_by_field_name(key).map(|child| text(child, s))
}
fn base_type(t: &str) -> &str {
    t.split(['<', '[']).next().unwrap_or(t).trim()
}
fn child<'n>(n: Node<'n>, kind: &str) -> Option<Node<'n>> {
    children(n).into_iter().find(|c| c.kind() == kind)
}
fn named_type(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "class_declaration" | "interface_declaration" | "enum_declaration"
    )
}
fn methods(n: Node<'_>) -> bool {
    matches!(n.kind(), "constructor_declaration" | "method_declaration")
}
fn qualified(class: &str, name: &str) -> String {
    if class.is_empty() {
        name.into()
    } else {
        format!("{class}.{name}")
    }
}

struct Input<'s> {
    test_file: bool,
    source: &'s str,
    file: ParsedFile,
    package: String,
    imports: HashMap<String, String>,
    fields: HashMap<(String, String), String>,
}

pub fn extract(n: Node<'_>, s: &str, test_file: bool) -> ParsedFile {
    let mut input = Input {
        test_file,
        source: s,
        file: Default::default(),
        package: String::new(),
        imports: Default::default(),
        fields: Default::default(),
    };
    for item in children(n) {
        let path = children(item)
            .into_iter()
            .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"));
        if let Some(name) = path.map(|c| text(c, s)) {
            if item.kind() == "package_declaration" {
                input.package = name.into();
            }
            if item.kind() == "import_declaration" {
                let wildcard = child(item, "asterisk").is_some();
                let target = if wildcard {
                    format!("{name}.*")
                } else {
                    name.into()
                };
                input.imports.insert(
                    target.rsplit('.').next().unwrap_or(&target).into(),
                    target.clone(),
                );
                input.edge("IMPORTS_FROM", "<module>", &target, item, json!({}));
            }
        }
    }
    input.declarations(n, "");
    input.collect_calls(n, "", "<module>", &mut HashMap::new());
    input.file
}

impl Input<'_> {
    fn edge(&mut self, kind: &str, owner: &str, target: &str, n: Node<'_>, extra: Value) {
        self.file.syntax.edges.push(Edge {
            kind: kind.into(),
            owner: owner.into(),
            target: target.into(),
            line: n.start_position().row + 1,
            extra,
        });
    }
    fn type_identity(&self, t: &str, spring: bool) -> String {
        let head = t.split('.').next().unwrap_or(t);
        if let Some(import) = self.imports.get(head) {
            return format!("{import}{}", &t[head.len()..]);
        }
        if self.package.is_empty()
            || (spring && self.imports.contains_key("*"))
            || (t.contains('.') && head.starts_with(char::is_lowercase))
        {
            t.into()
        } else {
            format!("{}.{t}", self.package)
        }
    }
    fn declarations(&mut self, n: Node<'_>, class: &str) {
        let s = self.source;
        let mut nested = class.to_owned();
        if let Some(name) = field(n, "name", s) {
            let mut extra = json!({});
            let mut kind = None;
            let mut identity = qualified(class, name);
            if named_type(n) {
                nested = identity.clone();
                kind = Some("Class");
                self.class_metadata(n, &nested, &mut extra);
            } else if methods(n) {
                let dec = annotations::decorators(n, s);
                kind = Some(
                    if dec.iter().any(|d| {
                        matches!(
                            d.as_str(),
                            "Test"
                                | "ParameterizedTest"
                                | "RepeatedTest"
                                | "TestFactory"
                                | "TestTemplate"
                        )
                    }) || name.starts_with("test_")
                        || name.starts_with("Test")
                    {
                        "Test"
                    } else {
                        "Function"
                    },
                );
                self.method_metadata(n, class, &identity, &mut extra);
                self.events(n, &identity, &mut extra);
            }
            if let Some(kind) = kind {
                let decorators = annotations::decorators(n, s);
                let params = if methods(n) {
                    field(n, "parameters", s).map(str::to_owned)
                } else {
                    None
                };
                if self.file.syntax.declarations.contains_key(&identity) {
                    use sha2::{Digest, Sha256};
                    let modifiers = (!decorators.is_empty()).then(|| decorators.join(","));
                    let parent = (!class.is_empty()).then_some(class);
                    let payload = serde_json::to_string(&(
                        kind,
                        name,
                        parent,
                        "java",
                        &params,
                        None::<String>,
                        modifiers,
                        kind == "Test",
                    ))
                    .unwrap();
                    let suffix = hex::encode(&Sha256::digest(payload)[..6]);
                    let base = format!("{identity}:S{suffix}");
                    identity = base.clone();
                    let mut ordinal = 2;
                    while self.file.syntax.declarations.contains_key(&identity) {
                        identity = format!("{base}:{ordinal}");
                        ordinal += 1;
                    }
                }
                let doc = n
                    .prev_named_sibling()
                    .filter(|c| {
                        c.kind() == "block_comment"
                            && c.end_position().row + 1 >= n.start_position().row
                    })
                    .and_then(|c| text(c, s).strip_prefix("/**"))
                    .and_then(|d| d.strip_suffix("*/"))
                    .map(|d| {
                        d.trim()
                            .lines()
                            .map(|l| l.trim().trim_start_matches('*').trim())
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                self.file.syntax.declarations.insert(
                    identity.clone(),
                    Definition {
                        params,
                        returns: None,
                        modifiers: None,
                        decorators,
                        extra,
                        doc,
                    },
                );
                self.file.symbols.push(Symbol {
                    name: identity,
                    kind: kind.into(),
                    start_line: n.start_position().row + 1,
                    end_line: n.end_position().row + 1,
                });
            }
        }
        if n.kind() == "field_declaration"
            && let Some(t) = field(n, "type", s)
        {
            for d in children(n)
                .into_iter()
                .filter(|c| c.kind() == "variable_declarator")
            {
                if let Some(name) = field(d, "name", s) {
                    self.fields
                        .insert((class.into(), name.into()), base_type(t).into());
                }
            }
        }
        for c in children(n) {
            self.declarations(c, &nested);
        }
    }
}
