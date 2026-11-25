use super::super::syntax::{child, children, edge, text};
use super::declare;
use crate::model::ParsedFile;
use tree_sitter::Node;

fn imported(n: Node<'_>, s: &str) -> Option<String> {
    let mut head = n;
    let mut path = None;
    while head.kind() == "apply_expression" {
        let arg = head.child_by_field_name("argument")?;
        if arg.kind() == "path_expression" {
            path = Some(text(arg, s).to_owned());
        }
        head = head.child_by_field_name("function")?;
    }
    let name = text(head, s).rsplit('.').next()?;
    matches!(name, "import" | "callPackage")
        .then_some(path)
        .flatten()
}

pub(super) fn walk(
    n: Node<'_>,
    s: &str,
    file: &mut ParsedFile,
    flake: bool,
    inside_inputs: bool,
    in_binding: bool,
) {
    if n.kind() == "binding"
        && let (Some(attrs), Some(rhs)) = (
            n.child_by_field_name("attrpath"),
            n.child_by_field_name("expression"),
        )
    {
        let parts: Vec<_> = children(attrs)
            .into_iter()
            .filter(|v| v.kind() == "identifier")
            .map(|v| text(v, s))
            .collect();
        if !parts.is_empty() {
            let flat_url = flake && parts.len() > 1 && parts.first() == Some(&"inputs");
            if (flat_url || inside_inputs)
                && parts.last() == Some(&"url")
                && let Some(url) = child(rhs, "string_fragment")
            {
                edge(file, n, "IMPORTS_FROM", "<module>", text(url, s));
                if flat_url {
                    return;
                }
            }
            declare(file, n, &parts.join("."), "Function", Default::default());
            walk(
                rhs,
                s,
                file,
                flake,
                inside_inputs || (flake && parts == ["inputs"]),
                true,
            );
            return;
        }
    }
    if in_binding
        && n.kind() == "apply_expression"
        && let Some(target) = imported(n, s)
    {
        edge(file, n, "IMPORTS_FROM", "<module>", &target);
    }
    for c in children(n) {
        walk(c, s, file, flake, inside_inputs, in_binding);
    }
}
