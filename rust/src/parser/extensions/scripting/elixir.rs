use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Definition};
use tree_sitter::Node;
pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, caller: &str) {
    if n.kind() == "call"
        && let Some(f) = n.child_by_field_name("target")
    {
        let args = child(n, "arguments");
        let arg = args.and_then(|a| a.named_child(0));
        let target = field(f, "right", s).unwrap_or(text(f, s));
        if target == "defmodule"
            && let Some(name) = arg
        {
            let id = text(name, s);
            declare(file, n, id, "Class", Default::default());
            if let Some(body) = child(n, "do_block") {
                walk(body, s, file, id, "<module>");
            }
            return;
        }
        if matches!(target, "def" | "defp" | "defmacro" | "defmacrop")
            && let Some(head) = arg
        {
            let head = if head.kind() == "binary_operator" {
                head.child_by_field_name("left").unwrap_or(head)
            } else {
                head
            };
            let name = field(head, "target", s).unwrap_or(text(head, s));
            let id = qualified(scope, name);
            let params = child(head, "arguments").map(|v| text(v, s).to_owned());
            if let Some(previous) = file.symbols.iter_mut().find(|sym| sym.name == id) {
                previous.end_line = n.end_position().row + 1;
            } else {
                declare(
                    file,
                    n,
                    &id,
                    function_kind(name),
                    Definition {
                        params,
                        ..Default::default()
                    },
                );
            }
            if let Some(body) = child(n, "do_block") {
                walk(body, s, file, scope, &id);
            }
            if let Some(kw) = args.and_then(|a| child(a, "keywords")) {
                walk(kw, s, file, scope, &id);
            }
            return;
        }
        if matches!(target, "alias" | "import" | "require" | "use")
            && let Some(name) = arg
        {
            edge(file, n, "IMPORTS_FROM", "<module>", text(name, s));
            return;
        }
        call(file, n, caller, target);
        if let Some(a) = args {
            walk(a, s, file, scope, caller);
        }
        if let Some(body) = child(n, "do_block") {
            walk(body, s, file, scope, caller);
        }
        return;
    }
    for c in children(n) {
        walk(c, s, file, scope, caller);
    }
}
