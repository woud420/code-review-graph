use super::{children, text};
use std::collections::HashSet;
use std::ops::Range;
use tree_sitter::Node;

fn binds(node: Node<'_>, key: &str, name: &str, s: &str) -> bool {
    node.children_by_field_name(key, &mut node.walk()).any(|v| {
        let valid =
            |n: Node| matches!(n.kind(), "identifier" | "type_identifier") && text(n, s) == name;
        if v.kind() == "expression_list" {
            children(v).into_iter().any(valid)
        } else {
            valid(v)
        }
    })
}

pub(super) fn shadows(body: Node<'_>, name: &str, s: &str) -> Vec<Range<usize>> {
    let mut intervals = Vec::new();
    let mut declared = HashSet::from([body.id()]);
    let mut stack = vec![(body, body)];
    while let Some((n, mut scope)) = stack.pop() {
        if matches!(
            n.kind(),
            "block" | "communication_case" | "default_case" | "expression_case" | "type_case"
        ) {
            scope = n;
        }
        match n.kind() {
            "func_literal" => {
                let bound = ["parameters", "result"]
                    .iter()
                    .filter_map(|f| n.child_by_field_name(f))
                    .flat_map(children)
                    .any(|p| binds(p, "name", name, s));
                if bound && let Some(b) = n.child_by_field_name("body") {
                    intervals.push(b.byte_range());
                    declared.insert(b.id());
                }
            }
            "type_switch_statement" if binds(n, "alias", name, s) => {
                for case in children(n)
                    .into_iter()
                    .filter(|v| matches!(v.kind(), "default_case" | "type_case"))
                {
                    if let Some(list) = children(case)
                        .into_iter()
                        .find(|v| v.kind() == "statement_list")
                    {
                        intervals.push(list.start_byte()..case.end_byte());
                    }
                    declared.insert(case.id());
                }
            }
            "const_spec" | "type_alias" | "type_spec" | "var_spec" if binds(n, "name", name, s) => {
                intervals.push(n.end_byte()..scope.end_byte());
                declared.insert(scope.id());
            }
            "short_var_declaration" if binds(n, "left", name, s) => {
                let scope = match n.parent() {
                    Some(p) if p.kind() == "for_clause" => p.parent().unwrap_or(scope),
                    Some(p)
                        if matches!(
                            p.kind(),
                            "if_statement"
                                | "type_switch_statement"
                                | "expression_switch_statement"
                        ) =>
                    {
                        p
                    }
                    _ => scope,
                };
                if declared.insert(scope.id()) {
                    intervals.push(n.end_byte()..scope.end_byte());
                }
            }
            "range_clause" | "receive_statement"
                if n.children(&mut n.walk()).any(|c| c.kind() == ":=")
                    && binds(n, "left", name, s) =>
            {
                if let Some(p) = n.parent() {
                    if n.kind() == "range_clause"
                        && let Some(b) = p.child_by_field_name("body")
                    {
                        intervals.push(b.byte_range());
                        declared.insert(b.id());
                    } else if n.kind() == "receive_statement" {
                        intervals.push(n.end_byte()..p.end_byte());
                        declared.insert(p.id());
                    }
                }
            }
            _ => {}
        }
        stack.extend(children(n).into_iter().rev().map(|c| (c, scope)));
    }
    intervals
}
