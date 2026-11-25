use super::super::syntax::{call, child, children, edge, field, text};
use super::{declare, function_kind, identity::overload};
use crate::model::ParsedFile;
use tree_sitter::Node;
pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, caller: &str, test_file: bool) {
    match n.kind() {
        "package_statement" => {
            if let Some(name) = child(n, "package_name") {
                declare(file, n, text(name, s), "Class", Default::default());
            }
        }
        "use_no_statement" => {
            edge(file, n, "IMPORTS_FROM", "<module>", text(n, s));
            return;
        }
        "function_definition" => {
            if let Some(name) = field(n, "name", s) {
                let kind = function_kind(name);
                let id = overload(file, "", name, kind, "perl", None, test_file);
                declare(file, n, &id, kind, Default::default());
                if let Some(body) = n.child_by_field_name("body") {
                    if let Some(symbol) = file.symbols.last_mut() {
                        symbol.end_line =
                            n.start_position().row + text(n, s).trim_end().lines().count();
                    }
                    walk(body, s, file, &id, test_file);
                }
                return;
            }
        }
        "call_expression_with_bareword" | "method_invocation" => {
            if let Some(name) = field(n, "function_name", s) {
                let raw = field(n, "package_name", s)
                    .map_or_else(|| name.to_owned(), |pkg| format!("{pkg}::{name}"));
                call(file, n, caller, &raw);
            }
        }
        "bless" => call(file, n, caller, "bless"),
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, caller, test_file);
    }
}
