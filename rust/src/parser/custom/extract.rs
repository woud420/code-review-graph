use super::Language;
use crate::{
    model::{ParsedFile, syntax::Definition},
    parser::{
        extensions,
        syntax::{call, children, edge, qualified, text},
    },
};
use tree_sitter::Node;

fn descendant<'a>(
    root: Node<'a>,
    depth: u8,
    predicate: &impl Fn(Node<'a>) -> bool,
) -> Option<Node<'a>> {
    if predicate(root) {
        return Some(root);
    }
    if depth == 0 {
        return None;
    }
    children(root)
        .into_iter()
        .find_map(|n| descendant(n, depth - 1, predicate))
}
fn name_leaf<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let leaf = descendant(node, 4, &|n| {
        n.child_count() == 0
            && matches!(
                n.kind(),
                "word"
                    | "identifier"
                    | "name"
                    | "simple_identifier"
                    | "command_name"
                    | "key_brace"
                    | "type_identifier"
                    | "property_identifier"
                    | "constant"
            )
    })
    .unwrap_or(node);
    let value = text(leaf, source)
        .trim()
        .trim_matches(['{', '}', '"', '\''])
        .trim();
    (!value.is_empty() && !value.contains('\n') && value.chars().count() <= 256).then_some(value)
}
fn called<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    for field in ["function", "callee", "expr", "name"] {
        let Some(mut callee) = node.child_by_field_name(field) else {
            continue;
        };
        for _ in 0..32 {
            let Some(inner) = callee.child_by_field_name(field) else {
                break;
            };
            callee = inner;
        }
        let value = text(callee, source).trim();
        return (!value.is_empty() && !value.contains('\n') && value.chars().count() <= 256)
            .then_some(value);
    }
    None
}
impl Language {
    fn definition_name<'s>(&self, n: Node<'_>, s: &'s str) -> Option<&'s str> {
        self.names
            .iter()
            .find_map(|field| n.child_by_field_name(field).and_then(|v| name_leaf(v, s)))
            .or_else(|| {
                self.names.iter().find_map(|ty| {
                    children(n)
                        .into_iter()
                        .find_map(|v| descendant(v, 3, &|x| x.kind() == ty))
                        .and_then(|v| name_leaf(v, s))
                })
            })
            .or_else(|| n.child_by_field_name("name").map(|v| text(v, s)))
            .or_else(|| {
                children(n)
                    .into_iter()
                    .find(|v| {
                        matches!(
                            v.kind(),
                            "identifier"
                                | "name"
                                | "type_identifier"
                                | "property_identifier"
                                | "simple_identifier"
                                | "constant"
                                | "field_identifier"
                        )
                    })
                    .map(|v| text(v, s))
            })
    }
    pub(super) fn extract(&self, root: Node<'_>, s: &str, file: &mut ParsedFile) {
        let mut stack = vec![(root, String::new(), "<module>".to_owned())];
        while let Some((n, mut scope, mut owner)) = stack.pop() {
            let class = self.classes.iter().any(|ty| ty == n.kind());
            if (class || self.functions.iter().any(|ty| ty == n.kind()))
                && let Some(name) = self.definition_name(n, s)
            {
                let id = qualified(&scope, name);
                let kind = if class {
                    "Class"
                } else if name.starts_with("test_")
                    || name.starts_with("Test")
                    || name.ends_with("_test")
                    || name.ends_with("_spec")
                {
                    "Test"
                } else {
                    "Function"
                };
                let property = |kinds: &[&str]| {
                    children(n)
                        .into_iter()
                        .find(|v| kinds.contains(&v.kind()))
                        .map(|v| text(v, s).to_owned())
                };
                let declaration = if class {
                    Definition::default()
                } else {
                    Definition {
                        params: property(&[
                            "parameters",
                            "formal_parameters",
                            "parameter_list",
                            "formal_parameter_list",
                        ]),
                        returns: property(&[
                            "type",
                            "return_type",
                            "type_annotation",
                            "return_type_definition",
                        ]),
                        ..Default::default()
                    }
                };
                extensions::declare(file, n, &id, kind, declaration);
                if class {
                    scope = id.clone();
                }
                owner = id;
            } else if self.calls.iter().any(|ty| ty == n.kind())
                && let Some(raw) = called(n, s)
            {
                call(file, n, &owner, raw.rsplit('.').next().unwrap_or(raw));
            } else if self.imports.iter().any(|ty| ty == n.kind()) {
                let target = ["module", "name", "path", "source"]
                    .into_iter()
                    .find_map(|key| {
                        n.child_by_field_name(key)
                            .map(|v| text(v, s).trim().trim_matches(['\'', '"']))
                            .filter(|v| !v.is_empty())
                    })
                    .unwrap_or_else(|| text(n, s));
                edge(file, n, "IMPORTS_FROM", "<module>", target);
            }
            stack.extend(
                children(n)
                    .into_iter()
                    .rev()
                    .map(|c| (c, scope.clone(), owner.clone())),
            );
        }
    }
}
