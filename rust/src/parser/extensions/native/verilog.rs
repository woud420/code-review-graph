use super::super::syntax::{call, child, children, edge, qualified, text};
use super::{declare, function_kind, name_child};
use crate::model::{ParsedFile, syntax::Definition};
use serde_json::json;
use tree_sitter::Node;

fn find<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    if n.kind() == kind {
        return Some(n);
    }
    children(n).into_iter().find_map(|c| find(c, kind))
}
fn signal(
    file: &mut ParsedFile,
    n: Node<'_>,
    name: Node<'_>,
    s: &str,
    scope: &str,
    kind: &str,
    meta: Definition,
) {
    let id = qualified(scope, text(name, s));
    if file.syntax.declarations.contains_key(&id) {
        return;
    }
    let mut meta = meta;
    meta.extra["verilog_kind"] = json!(kind);
    declare(file, n, &id, "Function", meta);
}
fn refs(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, origin: Node<'_>) {
    if matches!(
        n.kind(),
        "select1" | "select" | "bit_select" | "constant_range" | "constant_expression"
    ) {
        return;
    }
    if n.kind() == "simple_identifier" {
        let id = qualified(scope, text(n, s));
        if file.syntax.declarations.get(&id).is_some_and(|d| {
            matches!(
                d.extra["verilog_kind"].as_str(),
                Some("port" | "net" | "parameter" | "localparam")
            )
        }) {
            edge(file, origin, "REFERENCES", scope, &id);
        }
        return;
    }
    for c in children(n) {
        refs(c, s, file, scope, origin);
    }
}
pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, caller: &str) {
    match n.kind() {
        "module_declaration"
        | "interface_declaration"
        | "class_declaration"
        | "package_declaration"
        | "program_declaration" => {
            if let Some(name) = find(n, "simple_identifier") {
                let id = qualified(scope, text(name, s));
                declare(file, n, &id, "Class", Default::default());
                for c in children(n) {
                    walk(c, s, file, &id, &id);
                }
                return;
            }
        }
        "task_declaration" | "function_declaration" => {
            let tag = if n.kind() == "task_declaration" {
                "task_identifier"
            } else {
                "function_identifier"
            };
            if let Some(name) = find(n, tag).and_then(|c| find(c, "simple_identifier")) {
                let name = text(name, s);
                let id = qualified(scope, name);
                declare(file, n, &id, function_kind(name), Default::default());
                for c in children(n) {
                    walk(c, s, file, scope, &id);
                }
                return;
            }
        }
        "package_import_item" => {
            if let Some(name) = find(n, "package_identifier") {
                edge(file, n, "IMPORTS_FROM", "<module>", text(name, s));
                return;
            }
        }
        "list_of_port_declarations" => {
            let (mut direction, mut dtype) = (None, None);
            for port in children(n)
                .into_iter()
                .filter(|p| p.kind() == "ansi_port_declaration")
            {
                if let Some(header) = name_child(port, |k| {
                    k.ends_with("port_header") || k == "net_port_header1"
                }) {
                    if let Some(value) = child(header, "port_direction") {
                        direction = Some(text(value, s).to_owned());
                    }
                    if let Some(value) = name_child(header, |k| {
                        matches!(
                            k,
                            "data_type"
                                | "net_port_type1"
                                | "variable_port_type"
                                | "data_type_or_implicit1"
                        )
                    }) {
                        dtype = Some(text(value, s).to_owned());
                    }
                }
                if let Some(name) =
                    child(port, "port_identifier").and_then(|p| find(p, "simple_identifier"))
                {
                    signal(
                        file,
                        port,
                        name,
                        s,
                        scope,
                        "port",
                        Definition {
                            returns: dtype.clone(),
                            modifiers: direction.clone(),
                            ..Default::default()
                        },
                    );
                }
            }
            return;
        }
        "data_declaration"
        | "net_declaration"
        | "parameter_declaration"
        | "local_parameter_declaration" => {
            if caller != scope && caller != "<module>" {
                return;
            }
            let parameter = n.kind().contains("parameter");
            let kind = match n.kind() {
                "parameter_declaration" => "parameter",
                "local_parameter_declaration" => "localparam",
                _ => "net",
            };
            let dtype = name_child(n, |k| {
                matches!(k, "data_type_or_implicit1" | "data_type" | "net_type")
            })
            .map(|v| text(v, s));
            if let Some(list) = name_child(n, |k| {
                matches!(
                    k,
                    "list_of_param_assignments"
                        | "list_of_variable_decl_assignments"
                        | "list_of_net_decl_assignments"
                )
            }) {
                for assign in children(list) {
                    if let Some(name) = find(assign, "simple_identifier") {
                        let mut extra = json!({});
                        if let Some(value) = child(assign, "constant_param_expression") {
                            extra["default"] = json!(text(value, s));
                        }
                        signal(
                            file,
                            assign,
                            name,
                            s,
                            scope,
                            kind,
                            Definition {
                                returns: dtype.map(str::to_owned),
                                modifiers: if parameter {
                                    None
                                } else {
                                    dtype
                                        .and_then(|d| d.split_whitespace().next())
                                        .map(str::to_owned)
                                },
                                extra,
                                ..Default::default()
                            },
                        );
                    }
                }
                return;
            }
        }
        "modport_item" => {
            if let Some(name) =
                child(n, "modport_identifier").and_then(|p| find(p, "simple_identifier"))
            {
                signal(file, n, name, s, scope, "modport", Default::default());
                return;
            }
        }
        "module_instantiation" | "interface_instantiation" => {
            if let Some(name) = child(n, "simple_identifier") {
                call(file, n, caller, text(name, s));
            }
        }
        "named_port_connection" => {
            if let Some(expr) = child(n, "expression") {
                refs(expr, s, file, scope, n);
            }
            return;
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope, caller);
    }
}
