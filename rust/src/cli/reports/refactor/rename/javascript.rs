use super::*;
use std::ops::Range;
use syntax::{is_field, parents, walk, within};
use tree_sitter::Node;
fn function(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "program"
            | "function_declaration"
            | "function_expression"
            | "function"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function"
    )
}
fn block(n: Node<'_>) -> bool {
    function(n)
        || matches!(
            n.kind(),
            "statement_block"
                | "for_statement"
                | "for_in_statement"
                | "catch_clause"
                | "class_body"
                | "switch_body"
        )
}
fn declaration(n: Node<'_>) -> bool {
    let Some(p) = n.parent() else {
        return false;
    };
    match p.kind() {
        "formal_parameters" | "array_pattern" | "rest_pattern" | "import_clause"
        | "namespace_import" => true,
        "required_parameter" | "optional_parameter" => is_field(p, "pattern", n),
        "catch_clause" | "arrow_function" => is_field(p, "parameter", n),
        "assignment_pattern" | "for_in_statement" => is_field(p, "left", n),
        "pair_pattern" => is_field(p, "value", n),
        "variable_declarator"
        | "function_declaration"
        | "function_signature"
        | "generator_function_declaration"
        | "function_expression"
        | "function"
        | "generator_function"
        | "class_declaration"
        | "abstract_class_declaration"
        | "class"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration"
        | "type_parameter" => is_field(p, "name", n),
        _ => n.kind() == "shorthand_property_identifier_pattern",
    }
}
fn scope(n: Node<'_>, root: Node<'_>) -> Range<usize> {
    let mut child = n;
    for p in parents(n) {
        let s = match p.kind() {
            "formal_parameters" => p.parent(),
            "arrow_function" if is_field(p, "parameter", child) => Some(p),
            "catch_clause" | "for_in_statement" => Some(p),
            "variable_declarator"
                if p.parent()
                    .is_some_and(|a| a.kind() == "variable_declaration") =>
            {
                parents(p).find(|a| function(*a))
            }
            "variable_declarator" => parents(p).find(|a| block(*a)),
            "function_expression" | "function" | "generator_function" | "class"
                if is_field(p, "name", child) =>
            {
                Some(p)
            }
            _ if is_field(p, "name", child) && declaration(child) => parents(p).find(|a| block(*a)),
            "import_statement" | "program" => Some(root),
            _ => None,
        };
        if let Some(s) = s {
            return s.byte_range();
        }
        child = p;
    }
    root.byte_range()
}
fn specifier<'a>(src: &'a Source, node: Node<'_>) -> &'a str {
    src.value(node).trim_matches(['\'', '"', '`'])
}
fn exports_spec(
    src: &Source,
    spec: Node<'_>,
    statement: Node<'_>,
    exports: &BTreeSet<String>,
) -> bool {
    if src
        .imports
        .get(&(statement.start_position().row + 1))
        .is_some_and(|v| v.iter().any(|p| exports.contains(p)))
    {
        return true;
    }
    let value = specifier(src, spec);
    if !value.starts_with('.') {
        return false;
    }
    let Some(parent) = Path::new(&src.path).parent() else {
        return false;
    };
    let base = parent.join(value);
    if base
        .canonicalize()
        .ok()
        .is_some_and(|p| exports.contains(p.to_string_lossy().as_ref()))
    {
        return true;
    }
    ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"]
        .iter()
        .flat_map(|ext| {
            [
                base.with_extension(ext),
                base.join("index").with_extension(ext),
            ]
        })
        .any(|p| {
            p.canonicalize()
                .ok()
                .is_some_and(|v| exports.contains(v.to_string_lossy().as_ref()))
        })
}
#[derive(Default)]
struct Bindings {
    scopes: Vec<(Range<usize>, bool)>,
    namespaces: Vec<(String, Range<usize>)>,
}
impl Bindings {
    fn bound(&self, n: Node<'_>) -> Option<bool> {
        self.scopes
            .iter()
            .filter(|(s, _)| within(s, &n.byte_range()))
            .min_by_key(|(s, b)| (s.len(), *b))
            .map(|(_, b)| *b)
    }
}
fn import(
    src: &Source,
    statement: Node<'_>,
    root: Node<'_>,
    t: &Target,
    exports: &BTreeSet<String>,
    bindings: &mut Bindings,
    scan: &mut Scan,
) {
    let spec = statement.child_by_field_name("source").or_else(|| {
        statement
            .named_children(&mut statement.walk())
            .find(|p| p.kind() == "import_require_clause")
            .and_then(|p| p.child_by_field_name("source"))
    });
    let direct = spec.is_some_and(|s| exports_spec(src, s, statement, exports));
    walk(statement, |part| {
        match part.kind() {
            "import_specifier" | "export_specifier" => {
                let Some(original) = part.child_by_field_name("name") else {
                    return false;
                };
                let alias = part.child_by_field_name("alias");
                let target = src.value(original) == t.name && direct;
                let label = if part.kind() == "import_specifier" {
                    "import"
                } else {
                    "export"
                };
                if target {
                    scan.add(src, original, label, None);
                }
                if alias.map(|a| src.value(a)).unwrap_or(src.value(original)) == t.name {
                    if label == "import" {
                        bindings.scopes.push((root.byte_range(), target));
                    } else if target {
                        scan.exports = true;
                    }
                    if target && let Some(a) = alias {
                        scan.add(src, a, label, None);
                    }
                }
                return false;
            }
            "identifier"
                if part.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        "namespace_import" | "import_require_clause" | "import_clause"
                    )
                }) =>
            {
                if direct && part.parent().is_some_and(|p| p.kind() != "import_clause") {
                    bindings
                        .namespaces
                        .push((src.value(part).into(), root.byte_range()));
                }
                if src.value(part) == t.name {
                    bindings.scopes.push((root.byte_range(), false));
                }
            }
            "*" if direct
                && statement.kind() == "export_statement"
                && part.parent() == Some(statement) =>
            {
                scan.exports = true;
            }
            _ => (),
        }
        true
    });
}
fn dynamic<'a>(src: &Source, call: Node<'a>) -> Option<Node<'a>> {
    if call.kind() != "call_expression"
        || !matches!(src.field(call, "function"), "require" | "import")
    {
        return None;
    }
    call.child_by_field_name("arguments")?
        .named_child(0)
        .filter(|s| s.kind() == "string")
}
pub(super) fn scan(
    root: Node<'_>,
    src: &Source,
    t: &Target,
    new: &str,
    exports: &BTreeSet<String>,
) -> Scan {
    let home = src.path == t.file;
    let mut scan = Scan::default();
    let mut bindings = Bindings::default();
    let mut plain = Vec::new();
    let mut members = Vec::new();
    let mut commonjs = false;
    let mut found = false;
    walk(root, |n| {
        match n.kind() {
            "import_statement" => {
                import(src, n, root, t, exports, &mut bindings, &mut scan);
                return false;
            }
            "export_statement" if n.child_by_field_name("source").is_some() => {
                import(src, n, root, t, exports, &mut bindings, &mut scan);
                return false;
            }
            "member_expression" | "nested_identifier" => {
                let obj = n.child_by_field_name("object").or(n.named_child(0));
                let prop = n.child_by_field_name("property").or(n.named_child(1));
                if let (Some(obj), Some(prop)) = (obj, prop) {
                    if src.value(obj) == "exports"
                        || (src.value(obj) == "module" && src.value(prop) == "exports")
                    {
                        commonjs = true;
                    }
                    if obj.kind() == "identifier" && src.value(prop) == t.name {
                        members.push((obj, prop));
                    }
                }
            }
            "variable_declarator" => {
                if let (Some(var), Some(mut val)) = (
                    n.child_by_field_name("name"),
                    n.child_by_field_name("value"),
                ) {
                    if val.kind() == "await_expression"
                        && let Some(child) = val.named_child(0)
                    {
                        val = child;
                    }
                    if var.kind() == "identifier"
                        && let Some(spec) = dynamic(src, val)
                        && exports_spec(src, spec, n, exports)
                    {
                        bindings
                            .namespaces
                            .push((src.value(var).into(), scope(var, root)));
                    }
                }
            }
            "call_expression" => {
                if let Some(func) = n.child_by_field_name("function").filter(|f| {
                    f.kind() == "member_expression" && src.field(*f, "property") == "then"
                }) && let Some(spec) = func
                    .child_by_field_name("object")
                    .and_then(|x| dynamic(src, x))
                    && exports_spec(src, spec, n, exports)
                    && let Some(cb) = n
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                {
                    let param = cb
                        .child_by_field_name("parameter")
                        .or_else(|| cb.child_by_field_name("parameters")?.named_child(0));
                    if let Some(param) = param
                        .map(|p| p.child_by_field_name("pattern").unwrap_or(p))
                        .filter(|p| p.kind() == "identifier")
                    {
                        bindings
                            .namespaces
                            .push((src.value(param).into(), cb.byte_range()));
                    }
                }
            }
            "identifier"
            | "type_identifier"
            | "shorthand_property_identifier_pattern"
            | "shorthand_property_identifier"
                if src.value(n) == t.name =>
            {
                if declaration(n) {
                    let target = home && !found && t.window(n.start_position().row + 1);
                    if target {
                        found = true;
                        scan.add(src, n, "definition", None);
                    }
                    bindings.scopes.push((scope(n, root), target));
                } else if !n.parent().is_some_and(|p| {
                    matches!(p.kind(), "member_expression" | "nested_identifier")
                        && (is_field(p, "property", n) || p.named_child(1) == Some(n))
                }) {
                    let exported = n.parent().filter(|p| p.kind() == "export_specifier");
                    if !exported
                        .is_some_and(|p| !is_field(p, "name", n) && src.field(p, "name") != t.name)
                    {
                        plain.push((n, exported));
                    }
                }
            }
            _ => (),
        }
        true
    });
    for (obj, prop) in members {
        if bindings
            .namespaces
            .iter()
            .any(|(prefix, scope)| prefix == src.value(obj) && scope.contains(&obj.start_byte()))
        {
            scan.add(src, prop, "attribute", None);
        }
    }
    for (n, exported) in plain {
        match bindings.bound(n) {
            Some(true) => {
                if exported.is_some_and(|p| {
                    p.child_by_field_name("alias").is_none() || src.field(p, "alias") == t.name
                }) {
                    scan.exports = true;
                }
                let replacement = (n.kind() == "shorthand_property_identifier")
                    .then(|| format!("{}: {new}", t.name));
                scan.add(
                    src,
                    n,
                    if exported.is_some() {
                        "export"
                    } else {
                        "reference"
                    },
                    replacement,
                );
            }
            Some(false) => {
                scan.kept.insert(n.start_byte());
            }
            None => (),
        }
    }
    scan.authoritative = bindings.scopes.iter().any(|(_, b)| *b);
    scan.exports |= home && found && !commonjs;
    scan
}
