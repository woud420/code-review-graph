use std::path::Path;

use anyhow::{Result, anyhow, bail};
use tree_sitter::{Node, Parser};

use crate::model::{Call, Export, Import, ParsedFile, Symbol};

pub fn parse_typescript(path: &Path, source: &str) -> Result<ParsedFile> {
    let grammar = match path.extension().and_then(|ext| ext.to_str()) {
        Some("tsx") => tree_sitter_typescript::LANGUAGE_TSX,
        Some("ts" | "cts" | "mts") => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Some("js" | "jsx" | "cjs" | "mjs") => tree_sitter_javascript::LANGUAGE,
        _ => bail!(
            "unsupported JavaScript or TypeScript path {}",
            path.display()
        ),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("parser cancelled"))?;
    let mut extractor = Extractor {
        source,
        is_test: is_test_file(path),
        file: ParsedFile::default(),
    };
    extractor.walk(tree.root_node());
    Ok(extractor.file)
}

fn is_test_file(path: &Path) -> bool {
    let file = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    file.contains(".spec.")
        || file.contains(".test.")
        || path.components().any(|p| {
            matches!(
                p.as_os_str().to_str(),
                Some("tests" | "test" | "__tests__" | "spec")
            )
        })
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.named_children(&mut node.walk()).collect()
}

fn has_token(node: Node<'_>, kind: &str) -> bool {
    node.children(&mut node.walk()).any(|n| n.kind() == kind)
}

fn function_value(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_expression" | "arrow_function" | "generator_function" | "function"
    )
}

struct Extractor<'a> {
    source: &'a str,
    is_test: bool,
    file: ParsedFile,
}

impl<'s> Extractor<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        &self.source[node.byte_range()]
    }

    fn name(&self, node: Node<'_>) -> &'s str {
        self.text(node).trim_matches(['\'', '"', '`'])
    }

    fn binding(&self, node: Node<'_>) -> Option<&'s str> {
        node.child_by_field_name("name").map(|n| self.name(n))
    }

    fn symbol(&mut self, node: Node<'_>, kind: &str, name: &str) {
        self.file.symbols.push(Symbol {
            kind: kind.into(),
            name: name.into(),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
        });
    }

    fn call(&mut self, caller: &str, callee: &str, node: Node<'_>) {
        self.file.calls.push(Call {
            caller: caller.into(),
            callee: callee.into(),
            line: node.start_position().row + 1,
        });
    }

    fn import(&mut self, local: &str, imported: &str, module: &str) {
        self.file.imports.push(Import {
            local: local.into(),
            imported: imported.into(),
            module: module.into(),
        });
    }

    fn export(&mut self, name: &str, local: &str, module: Option<&str>) {
        self.file.exports.push(Export {
            name: name.into(),
            local: local.into(),
            module: module.map(str::to_owned),
        });
    }

    fn es_import(&mut self, node: Node<'_>) {
        let Some(source) = node.child_by_field_name("source") else {
            return;
        };
        let module = self.name(source);
        let count = self.file.imports.len();
        let mut stack = children(node);
        while let Some(child) = stack.pop() {
            match child.kind() {
                "import_clause" | "named_imports" => stack.extend(children(child)),
                "identifier" => self.import(self.text(child), "default", module),
                "namespace_import" => {
                    if let Some(local) = child.named_child(0) {
                        self.import(self.text(local), "*", module);
                    }
                }
                "import_specifier" => {
                    if let Some(imported) = child.child_by_field_name("name") {
                        let local = child.child_by_field_name("alias").unwrap_or(imported);
                        self.import(self.name(local), self.name(imported), module);
                    }
                }
                _ => {}
            }
        }
        if self.file.imports.len() == count {
            self.import("", "", module);
        }
    }

    fn es_export(&mut self, node: Node<'_>) {
        let module = node.child_by_field_name("source").map(|src| self.name(src));
        let default = has_token(node, "default");
        if let Some(decl) = node.child_by_field_name("declaration") {
            if let Some(name) = self.binding(decl) {
                self.export(if default { "default" } else { name }, name, None);
            } else {
                for item in children(decl) {
                    if item.kind() == "variable_declarator"
                        && let Some(name) = self.binding(item)
                    {
                        self.export(name, name, None);
                    }
                }
            }
        } else if let Some(value) = node.child_by_field_name("value") {
            if default {
                let target = if value.kind() == "identifier" {
                    self.text(value)
                } else {
                    self.binding(value).unwrap_or("default")
                };
                self.export("default", target, None);
            }
        } else if has_token(node, "*") {
            self.export("*", "*", module);
        } else {
            for entry in children(node) {
                if entry.kind() == "namespace_export" {
                    if let Some(name) = entry.named_child(0) {
                        self.export(self.name(name), "*", module);
                    }
                } else if entry.kind() == "export_clause" {
                    for spec in children(entry) {
                        if let Some(local) = spec.child_by_field_name("name") {
                            let name = spec.child_by_field_name("alias").unwrap_or(local);
                            self.export(self.name(name), self.name(local), module);
                        }
                    }
                }
            }
        }
    }

    fn module_call(&mut self, node: Node<'_>, name: &str) -> bool {
        if !matches!(name, "require" | "import") {
            return false;
        }
        let Some(args) = node.child_by_field_name("arguments") else {
            return false;
        };
        let Some(arg) = args.named_child(0) else {
            return false;
        };
        if args.named_child_count() != 1 || arg.kind() != "string" {
            return false;
        }
        let module = self.name(arg);
        if module.is_empty() {
            return false;
        }
        // File-wide aliases must be visible throughout the module. A require
        // inside a block still records a module dependency.
        let binding = node
            .parent()
            .filter(|decl| {
                decl.kind() == "variable_declarator"
                    && decl
                        .parent()
                        .and_then(|statement| statement.parent())
                        .is_some_and(|parent| {
                            parent.kind() == "program" || parent.kind() == "export_statement"
                        })
            })
            .and_then(|decl| decl.child_by_field_name("name"));
        match binding {
            Some(binding) if name == "require" && binding.kind() == "identifier" => {
                self.import(self.text(binding), "*", module)
            }
            Some(binding) if name == "require" && binding.kind() == "object_pattern" => {
                for field in children(binding) {
                    match field.kind() {
                        "shorthand_property_identifier_pattern" => {
                            self.import(self.text(field), self.text(field), module)
                        }
                        "pair_pattern" => {
                            if let (Some(key), Some(value)) = (
                                field.child_by_field_name("key"),
                                field.child_by_field_name("value"),
                            ) && value.kind() == "identifier"
                            {
                                self.import(self.text(value), self.name(key), module);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => self.import("", "", module),
        }
        true
    }

    fn reference(&self, node: Node<'_>) -> Option<String> {
        let mut parts = Vec::new();
        let mut base = node;
        while base.kind() == "member_expression" {
            let property = base.child_by_field_name("property")?;
            parts.push(self.text(property));
            base = base.child_by_field_name("object")?;
        }
        if !matches!(base.kind(), "identifier" | "this" | "super" | "import") {
            return None;
        }
        parts.push(self.text(base));
        parts.reverse();
        Some(parts.join("."))
    }

    fn record_call(&mut self, node: Node<'_>, scope: &str, class: &str) -> Option<String> {
        let target = node.child_by_field_name("function")?;
        let reference = if let Some(reference) = self.reference(target) {
            reference
        } else {
            let property = target.child_by_field_name("property")?;
            self.call(scope, self.text(property), node);
            return None;
        };
        if self.module_call(node, &reference) {
            return None;
        }
        if self.is_test {
            let base = reference.split('.').next().unwrap_or("");
            if matches!(
                base,
                "it" | "test"
                    | "describe"
                    | "beforeEach"
                    | "afterEach"
                    | "beforeAll"
                    | "afterAll"
                    | "suite"
            ) {
                let description = node
                    .child_by_field_name("arguments")
                    .and_then(|args| {
                        children(args)
                            .into_iter()
                            .find(|n| matches!(n.kind(), "string" | "template_string"))
                    })
                    .map(|arg| {
                        self.name(arg)
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|text| !text.is_empty());
                let line = node.start_position().row + 1;
                let name = match description {
                    Some(text) => format!(
                        "{base}:{}@L{line}",
                        text.chars().take(200).collect::<String>()
                    ),
                    None => format!("{base}@L{line}"),
                };
                self.symbol(node, "Test", &name);
                if scope != "<module>" {
                    self.file
                        .typescript
                        .containers
                        .insert(name.clone(), scope.to_owned());
                }
                return Some(name);
            }
        }
        let raw = reference
            .strip_prefix("this.")
            .filter(|_| !class.is_empty())
            .map(|method| format!("{class}.{method}"))
            .unwrap_or_else(|| reference.clone());
        if let Some((prefix, _)) = reference.rsplit_once('.') {
            let mut evidence = serde_json::json!({"member_call":reference});
            if !prefix.contains('.') {
                evidence["receiver"] = serde_json::json!(prefix);
            } else if let Some(field) = prefix
                .strip_prefix("this.")
                .filter(|name| !name.contains('.'))
            {
                evidence["receiver"] = serde_json::json!(field);
            }
            if let Some(receiver) = evidence["receiver"].as_str()
                && let Some(ty) = parameter_type(node, receiver, self.source)
            {
                evidence["receiver_type"] = serde_json::json!(ty);
                evidence["receiver_resolution"] = serde_json::json!("typed_receiver_unresolved");
            }
            self.file.typescript.members.push((
                scope.to_owned(),
                raw.clone(),
                node.start_position().row + 1,
                evidence,
            ));
        }
        self.call(scope, &raw, node);
        None
    }

    fn walk(&mut self, root: Node<'_>) {
        let mut stack = vec![(root, "<module>".to_owned(), String::new())];
        while let Some((node, mut caller, mut class)) = stack.pop() {
            let mut nested = children(node);
            let parent = node.parent();
            match node.kind() {
                "import_statement" => {
                    self.es_import(node);
                    continue;
                }
                "export_statement" => self.es_export(node),
                "class_declaration"
                | "abstract_class_declaration"
                | "class"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration" => {
                    if let Some(name) = self.binding(node) {
                        class = name.to_owned();
                        caller.clone_from(&class);
                        self.symbol(node, "Class", &class);
                        if matches!(
                            node.kind(),
                            "interface_declaration" | "type_alias_declaration"
                        ) {
                            self.file.typescript.kinds.insert(
                                class.clone(),
                                serde_json::json!({"typescript_kind":node.kind()}),
                            );
                        }
                        for base in nested
                            .iter()
                            .filter(|n| n.kind() == "class_heritage")
                            .flat_map(|n| children(*n))
                            .filter(|n| n.kind() == "extends_clause")
                            .filter_map(|n| n.child_by_field_name("value"))
                        {
                            let base = self.text(base).to_owned();
                            self.file
                                .typescript
                                .bases
                                .entry(class.clone())
                                .or_default()
                                .push(base);
                        }
                    }
                }
                "function_declaration"
                | "generator_function_declaration"
                | "method_definition"
                | "abstract_method_signature"
                | "arrow_function" => {
                    // Upstream names unassigned arrows after their first direct identifier.
                    // A parenthesized parameter list is a subtree and supplies no such name.
                    let name = self.binding(node).or_else(|| {
                        nested
                            .iter()
                            .find(|child| child.kind() == "identifier")
                            .map(|child| self.text(*child))
                    });
                    if let Some(name) = name {
                        caller = if class.is_empty() {
                            name.into()
                        } else {
                            format!("{class}.{name}")
                        };
                        self.symbol(node, function_kind(name), &caller);
                    }
                }
                "variable_declarator" | "public_field_definition" | "field_definition" => {
                    if let (Some(name), Some(value)) = (
                        node.child_by_field_name("name")
                            .or_else(|| node.child_by_field_name("property")),
                        node.child_by_field_name("value"),
                    ) && function_value(value)
                    {
                        let name = self.name(name);
                        caller = if class.is_empty() {
                            name.into()
                        } else {
                            format!("{class}.{name}")
                        };
                        let declaration = parent
                            .filter(|p| {
                                matches!(p.kind(), "lexical_declaration" | "variable_declaration")
                            })
                            .unwrap_or(node);
                        self.symbol(declaration, function_kind(name), &caller);
                        nested = children(value);
                    }
                }
                "call_expression" | "new_expression" => {
                    if let Some(test) = self.record_call(node, &caller, &class) {
                        caller = test;
                    }
                }
                "jsx_opening_element" | "jsx_self_closing_element" => {
                    if let Some(target) = node
                        .child_by_field_name("name")
                        .and_then(|n| self.reference(n))
                        && (target.starts_with(char::is_uppercase) || target.contains('.'))
                    {
                        self.call(&caller, &target, node);
                    }
                }
                "identifier" | "shorthand_property_identifier" | "type_identifier"
                    if is_value_or_type_reference(node) =>
                {
                    self.file.typescript.references.push((
                        caller.clone(),
                        self.text(node).to_owned(),
                        node.start_position().row + 1,
                    ));
                }
                "if_statement" => {
                    if let Some(mut cond) = node.child_by_field_name("condition") {
                        while cond.kind() == "parenthesized_expression" {
                            let Some(inner) = cond.named_child(0) else {
                                break;
                            };
                            cond = inner;
                        }
                        if matches!(self.text(cond), "false" | "0") {
                            let dead = node.child_by_field_name("consequence");
                            nested.retain(|n| Some(*n) != dead);
                        }
                    }
                }
                _ => {}
            }
            stack.extend(
                nested
                    .into_iter()
                    .rev()
                    .map(|child| (child, caller.clone(), class.clone())),
            );
        }
    }
}

fn parameter_type<'s>(node: Node<'_>, receiver: &str, source: &'s str) -> Option<&'s str> {
    let mut cursor = node.parent();
    while let Some(scope) = cursor {
        if let Some(params) = scope.child_by_field_name("parameters") {
            for param in children(params) {
                if param
                    .child_by_field_name("pattern")
                    .is_some_and(|id| &source[id.byte_range()] == receiver)
                {
                    let ty = param.child_by_field_name("type")?.named_child(0)?;
                    return (ty.kind() == "type_identifier").then(|| &source[ty.byte_range()]);
                }
            }
        }
        cursor = scope.parent();
    }
    None
}

fn is_value_or_type_reference(node: Node<'_>) -> bool {
    let Some(p) = node.parent() else {
        return false;
    };
    if node.kind() == "type_identifier" {
        return std::iter::successors(Some(p), |n| n.parent())
            .take_while(|n| {
                matches!(
                    n.kind(),
                    "type_annotation"
                        | "union_type"
                        | "array_type"
                        | "type_arguments"
                        | "generic_type"
                        | "intersection_type"
                        | "object_type"
                )
            })
            .any(|n| n.kind() == "type_annotation");
    }
    if node.kind() == "shorthand_property_identifier" {
        return true;
    }
    match p.kind() {
        "arguments" | "array" => true,
        "pair" => p.child_by_field_name("value") == Some(node),
        "assignment_expression" => {
            p.child_by_field_name("right") == Some(node)
                && p.child_by_field_name("left").is_some_and(|n| {
                    matches!(n.kind(), "member_expression" | "subscript_expression")
                })
        }
        _ => false,
    }
}

fn function_kind(name: &str) -> &'static str {
    if name.starts_with("test_")
        || name.starts_with("Test")
        || name.ends_with("_test")
        || name.ends_with("_spec")
    {
        "Test"
    } else {
        "Function"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_module_bindings_for_named_default_namespace_and_reexports() {
        let file = parse_typescript(
            Path::new("barrel.ts"),
            r#"
            import direct, { real as renamed } from "./original.js";
            import * as group from "./group.js";
            import "./effect.js";
            export { renamed as publicName };
            export { default as run } from "./other.js";
            export * from "./more.js";
            export default function local() { direct(); group.entry(); renamed(); }
            export const task = () => local();
        "#,
        )
        .unwrap();
        let imports: Vec<_> = file
            .imports
            .iter()
            .map(|i| (i.local.as_str(), i.imported.as_str(), i.module.as_str()))
            .collect();
        for import in [
            ("direct", "default", "./original.js"),
            ("renamed", "real", "./original.js"),
            ("group", "*", "./group.js"),
            ("", "", "./effect.js"),
        ] {
            assert!(
                imports.contains(&import),
                "missing {import:?} in {imports:?}"
            );
        }
        let exports: Vec<_> = file
            .exports
            .iter()
            .map(|e| (e.name.as_str(), e.local.as_str(), e.module.as_deref()))
            .collect();
        for export in [
            ("publicName", "renamed", None),
            ("run", "default", Some("./other.js")),
            ("*", "*", Some("./more.js")),
            ("default", "local", None),
            ("task", "task", None),
        ] {
            assert!(
                exports.contains(&export),
                "missing {export:?} in {exports:?}"
            );
        }
        assert!(
            file.calls
                .iter()
                .any(|c| c.caller == "local" && c.callee == "group.entry")
        );
    }

    #[test]
    fn callback_ownership_matches_upstream() {
        let file = parse_typescript(
            Path::new("source.ts"),
            "function outer() {\n \
            [1].map(x => use(x));\n [1].map((x) => use(x));\n \
            [1].map((x,y) => use(x));\n [1].map(() => use(0));\n }\n \
            const assigned = x => use(x);\n use(1);",
        )
        .unwrap();
        let callers: Vec<_> = file
            .calls
            .iter()
            .filter(|c| c.callee == "use")
            .map(|c| (c.caller.as_str(), c.line))
            .collect();
        assert_eq!(
            callers,
            [
                ("x", 2),
                ("outer", 3),
                ("outer", 4),
                ("outer", 5),
                ("assigned", 7),
                ("<module>", 8)
            ]
        );
    }

    #[test]
    fn jsx_methods_and_test_bodies_keep_their_caller() {
        let file = parse_typescript(Path::new("view.test.tsx"), "class View {\n render() { this.refresh(); return <Box title='hi'/>; }\n refresh() {}\n}\n it('mounts view', () => create());").unwrap();
        let calls: Vec<_> = file
            .calls
            .iter()
            .map(|c| (c.caller.as_str(), c.callee.as_str()))
            .collect();
        assert!(calls.contains(&("View.render", "View.refresh")));
        assert!(calls.contains(&("View.render", "Box")));
        assert!(calls.contains(&("it:mounts view@L5", "create")));
        assert!(
            file.symbols
                .iter()
                .any(|s| s.name == "View" && s.start_line == 1 && s.end_line == 4)
        );
    }

    #[test]
    fn static_commonjs_bindings_and_live_branches() {
        let file = parse_typescript(
            Path::new("main.cjs"),
            "const {go: run} = require('./service');\n if (false) skip(); else run();",
        )
        .unwrap();
        assert!(
            file.imports
                .iter()
                .any(|i| i.local == "run" && i.imported == "go" && i.module == "./service")
        );
        assert!(file.calls.iter().all(|c| c.callee != "skip"));
        assert!(file.calls.iter().any(|c| c.callee == "run"));
    }

    #[test]
    fn block_scoped_requires_and_import_promises_do_not_bind_module_namespaces() {
        let source =
            "function setup() { const local = require('./a'); }\nconst pending = import('./b');";
        let file = parse_typescript(Path::new("main.js"), source).unwrap();
        assert_eq!(file.imports.len(), 2);
        assert!(file.imports.iter().all(|i| i.local.is_empty()));
    }
}
