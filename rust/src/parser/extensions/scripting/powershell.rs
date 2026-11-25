use super::super::syntax::{call, child, children, qualified, text};
use super::{declare, identity};
use crate::model::ParsedFile;
use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::Node;

static RANGE: LazyLock<Regex> = LazyLock::new(|| {
    let number = r"[+-]?(?:(?:0x[0-9a-f]+|0b[01]+)(?:u[ysl]?|[ysln])?|\d+(?:u[ysl]?|[yslnd])?|(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?d?)(?:[kmgtp]b)?";
    Regex::new(&format!(r"(?i)^{number}\.\.(?:{number}|\$.*)?$")).unwrap()
});

pub(super) fn walk(
    n: Node<'_>,
    s: &str,
    file: &mut ParsedFile,
    scope: &str,
    caller: &str,
    test: bool,
) {
    let class = n.kind() == "class_statement";
    if (class || matches!(n.kind(), "class_method_definition" | "function_statement"))
        && let Some(id) = child(n, "function_name").or_else(|| child(n, "simple_name"))
    {
        let name = text(id, s);
        let name = name
            .split_once(':')
            .filter(|(prefix, _)| {
                ["global", "local", "private", "script"]
                    .iter()
                    .any(|p| prefix.eq_ignore_ascii_case(p))
            })
            .map_or(name, |(_, name)| name);
        let kind = if class {
            "Class"
        } else if test {
            "Test"
        } else {
            "Function"
        };
        let id = if class {
            qualified(scope, name)
        } else {
            identity::overload(file, scope, name, kind, "powershell", None, test)
        };
        declare(file, n, &id, kind, Default::default());
        for c in children(n) {
            walk(
                c,
                s,
                file,
                if class { &id } else { scope },
                if class { caller } else { &id },
                test,
            );
        }
        return;
    }
    if n.kind() == "command"
        && let Some(command) = child(n, "command_name")
    {
        let name = text(command, s).trim();
        if !name.is_empty() && !RANGE.is_match(name) {
            call(file, n, caller, name);
        }
    }
    for c in children(n) {
        walk(c, s, file, scope, caller, test);
    }
}
