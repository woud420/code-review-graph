use super::syntax::{direct_target, field, record, text};
use crate::model::{Call, ParsedFile, syntax::Site};
use std::path::Path;
use tree_sitter::Node;

pub(super) fn extract(root: Node<'_>, source: &str, path: &Path) -> ParsedFile {
    let mut file = ParsedFile::default();
    let mut cursor = root.walk();
    file.syntax.rust_module_items = root
        .named_children(&mut cursor)
        .filter(|node| {
            matches!(
                node.kind(),
                "struct_item"
                    | "enum_item"
                    | "trait_item"
                    | "function_item"
                    | "function_signature_item"
            )
        })
        .filter_map(|node| field(node, "name", source).map(str::to_owned))
        .collect();
    let test_file = crate::model::test_file(&path.to_string_lossy(), None);
    walk(root, source, &mut file, "", "<module>", test_file);
    let names: std::collections::HashSet<_> =
        file.symbols.iter().map(|s| s.name.as_str()).collect();
    for site in file.syntax.sites.values_mut().flatten() {
        if names.contains(site.raw.as_str()) {
            site.target = Some(site.raw.clone());
        }
    }
    file
}

pub(crate) fn attributes<'a>(node: Node<'_>, source: &'a str) -> impl Iterator<Item = &'a str> {
    std::iter::successors(node.prev_sibling(), |n| n.prev_sibling())
        .take_while(|n| n.kind() == "attribute_item")
        .filter_map(move |attr| attr.named_child(0).map(|inner| text(inner, source)))
}

fn test(name: &str, node: Node<'_>, source: &str, test_file: bool) -> bool {
    name.starts_with("test_")
        || name.starts_with("Test")
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
            ))
        || attributes(node, source).any(|a| {
            matches!(
                a,
                "Test"
                    | "ParameterizedTest"
                    | "RepeatedTest"
                    | "TestFactory"
                    | "org.junit.Test"
                    | "org.junit.jupiter.api.Test"
                    | "test"
                    | "tokio::test"
                    | "async_std::test"
                    | "rstest"
                    | "rstest::rstest"
                    | "proptest"
            )
        })
}

pub(crate) fn last_segment<'s>(mut node: Node<'_>, source: &'s str) -> Option<&'s str> {
    loop {
        let field = match node.kind() {
            "identifier" | "type_identifier" | "crate" | "self" | "super" => {
                return Some(text(node, source));
            }
            "generic_type" => "type",
            "generic_function" => "function",
            "scoped_type_identifier" | "scoped_identifier" => "name",
            _ => return None,
        };
        node = node.child_by_field_name(field)?;
    }
}

pub(crate) fn doc(node: Node<'_>, source: &str) -> Option<String> {
    let mut line = node.start_position().row;
    let mut lines = Vec::new();
    for sibling in std::iter::successors(node.prev_sibling(), |n| n.prev_sibling()) {
        if sibling.end_position().row + 1 < line {
            break;
        }
        line = sibling.start_position().row;
        let raw = text(sibling, source).trim();
        match sibling.kind() {
            "attribute_item" => continue,
            "line_comment" if raw.starts_with("///") && !raw.starts_with("////") => {
                lines.push(raw[3..].trim().to_owned())
            }
            "block_comment" if lines.is_empty() && raw.starts_with("/**") => {
                lines.extend(
                    raw[3..]
                        .strip_suffix("*/")?
                        .lines()
                        .rev()
                        .map(|s| s.trim().trim_start_matches('*').trim().to_owned()),
                );
                break;
            }
            _ => break,
        }
    }
    lines.reverse();
    static PARAGRAPH_TAG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)</?(?:p|para)(?:\s[^>]*)?>").unwrap());
    static INLINE_TAG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\{@(?:code|literal|link)\s+([^}]+)\}").unwrap()
    });
    static XML_TAG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"<[^>]+>").unwrap());
    let raw = lines.join("\n");
    let paragraphs = PARAGRAPH_TAG.replace_all(&raw, "\n\n");
    let inline = INLINE_TAG.replace_all(&paragraphs, "$1");
    let text = XML_TAG.replace_all(&inline, " ");
    let s = text
        .lines()
        .map(str::trim)
        .skip_while(|s| s.is_empty())
        .take_while(|s| !s.is_empty() && !s.starts_with("@param") && !s.starts_with("@return"))
        .flat_map(|s| s.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ");
    (!s.is_empty()).then(|| s.chars().take(400).collect())
}

fn in_dead_rust_branch(node: Node<'_>, src: &str) -> bool {
    let mut next = node.parent();
    while let Some(parent) = next {
        if parent.kind() == "impl_item" {
            return false;
        }
        if parent.kind() == "if_expression"
            && let (Some(mut condition), Some(body)) = (
                parent.child_by_field_name("condition"),
                parent.child_by_field_name("consequence"),
            )
        {
            while condition.kind() == "parenthesized_expression"
                && let Some(inner) = condition.named_child(0)
            {
                condition = inner;
            }
            if condition.kind() == "boolean_literal"
                && &src[condition.byte_range()] == "false"
                && node.start_byte() >= body.start_byte()
                && node.end_byte() <= body.end_byte()
            {
                return true;
            }
        }
        next = parent.parent();
    }
    false
}

fn walk(
    node: Node<'_>,
    source: &str,
    file: &mut ParsedFile,
    class: &str,
    caller: &str,
    test_file: bool,
) {
    let mut scope = class;
    let mut owner = caller;
    let qualified;
    match node.kind() {
        "struct_item" | "enum_item" | "trait_item" => {
            if let Some(name) = field(node, "name", source) {
                scope = name;
                record(file, node, name, "Class");
            }
        }
        "impl_item" => {
            let Some(name) = node
                .child_by_field_name("type")
                .and_then(|ty| last_segment(ty, source))
            else {
                return;
            };
            scope = name;
        }
        "function_item" | "function_signature_item" => {
            if let Some(name) = field(node, "name", source) {
                qualified = if class.is_empty() {
                    name.to_owned()
                } else {
                    format!("{class}.{name}")
                };
                owner = &qualified;
                let is_test = test(name, node, source, test_file);
                record(file, node, owner, if is_test { "Test" } else { "Function" });
            }
        }
        "macro_invocation" | "call_expression" if in_dead_rust_branch(node, source) => return,
        "macro_invocation" => {
            if let Some(name) = field(node, "macro", source) {
                file.calls.push(Call {
                    caller: caller.to_owned(),
                    callee: name.to_owned(),
                    line: node.start_position().row + 1,
                });
            }
        }
        "call_expression" => {
            if let Some(fun) = node.child_by_field_name("function")
                && let Some(target) = direct_target(fun, source, class)
            {
                let mut callee = fun;
                while callee.kind() == "generic_function"
                    && let Some(inner) = callee.child_by_field_name("function")
                {
                    callee = inner;
                }
                let receiver = callee.kind() == "field_expression"
                    && field(callee, "value", source) == Some("self");
                let associated = field(callee, "path", source) == Some("Self");
                if receiver || associated {
                    file.syntax
                        .sites
                        .entry(node.start_position().row + 1)
                        .or_default()
                        .push(Site {
                            caller: caller.into(),
                            raw: target.clone(),
                            target: None,
                            extra: if receiver {
                                serde_json::json!({"receiver":"self"})
                            } else {
                                serde_json::json!({})
                            },
                        });
                }
                file.calls.push(Call {
                    caller: caller.to_owned(),
                    callee: target,
                    line: node.start_position().row + 1,
                });
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, source, file, scope, owner, test_file);
    }
}
