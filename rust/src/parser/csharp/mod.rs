mod calls;
use crate::model::{
    ParsedFile, Symbol,
    syntax::{Definition, Edge},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tree_sitter::Node;

fn children(n: Node<'_>) -> Vec<Node<'_>> {
    n.named_children(&mut n.walk()).collect()
}
fn child<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    children(n).into_iter().find(|c| c.kind() == kind)
}
fn text<'s>(n: Node<'_>, s: &'s str) -> &'s str {
    &s[n.byte_range()]
}
fn compact(t: &str) -> String {
    t.chars()
        .filter(|c| !c.is_whitespace() && *c != '@')
        .collect()
}
fn named(n: Node<'_>, key: &str, s: &str) -> Option<String> {
    n.child_by_field_name(key).map(|v| compact(text(v, s)))
}
fn join(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.into()
    } else {
        format!("{scope}.{name}")
    }
}
fn is_type(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "enum_declaration"
    )
}
fn is_method(n: Node<'_>) -> bool {
    matches!(n.kind(), "method_declaration" | "constructor_declaration")
}
fn scopes(n: Node<'_>, s: &str) -> Vec<(String, i64)> {
    let mut names = Vec::new();
    let mut parent = n.parent();
    while let Some(p) = parent {
        if matches!(
            p.kind(),
            "namespace_declaration" | "file_scoped_namespace_declaration"
        ) {
            names.push(p);
        }
        if p.parent().is_none() && names.is_empty() {
            for c in children(p) {
                if c.start_byte() >= n.start_byte()
                    || c.kind() == "namespace_declaration"
                    || is_type(c)
                {
                    break;
                }
                if c.kind() == "file_scoped_namespace_declaration" {
                    names.push(c);
                    break;
                }
            }
        }
        parent = p.parent();
    }
    let mut scopes = vec![(String::new(), -1)];
    for d in names.into_iter().rev() {
        if let Some(name) = named(d, "name", s) {
            let prefix = &scopes.last().unwrap().0;
            scopes.push((join(prefix, &name), d.start_byte() as i64));
        }
    }
    scopes.reverse();
    scopes
}

struct Input<'s> {
    file: ParsedFile,
    source: &'s str,
    fields: HashMap<(String, String), String>,
    namespaces: Vec<String>,
}
pub fn extract(n: Node<'_>, s: &str) -> ParsedFile {
    let mut input = Input {
        file: Default::default(),
        source: s,
        fields: Default::default(),
        namespaces: Vec::new(),
    };
    input.declarations(n, "");
    input.calls(n, "", "<module>", &mut HashMap::new());
    if !input.namespaces.is_empty() {
        input.file.syntax.file = json!({"csharp_namespaces":input.namespaces});
    }
    input.file
}
impl Input<'_> {
    fn edge(&mut self, n: Node<'_>, kind: &str, owner: &str, target: &str, extra: Value) {
        self.file.syntax.edges.push(Edge {
            owner: owner.into(),
            kind: kind.into(),
            target: target.into(),
            line: n.start_position().row + 1,
            extra,
        });
    }
    fn type_name(&self, n: Node<'_>, class: &str) -> String {
        let scope = scopes(n, self.source);
        let owner = if class.is_empty() { &scope[0].0 } else { class };
        join(owner, &named(n, "name", self.source).unwrap_or_default())
    }
    fn declarations(&mut self, n: Node<'_>, class: &str) {
        let s = self.source;
        let mut nested = class.to_owned();
        if matches!(
            n.kind(),
            "namespace_declaration" | "file_scoped_namespace_declaration"
        ) && let Some(name) = named(n, "name", s)
        {
            self.namespaces.push(join(&scopes(n, s)[0].0, &name));
        }
        if n.kind() == "using_directive"
            && let Some(last) = children(n).last()
        {
            let scope = scopes(n, s);
            let alias = named(n, "name", s);
            let global = n.children(&mut n.walk()).any(|c| c.kind() == "global");
            let is_static = n.children(&mut n.walk()).any(|c| c.kind() == "static");
            let mut extra = json!({"source_offset":n.start_byte(),"csharp_scopes":scope,"csharp_global":global,"csharp_using_kind":if alias.is_some() {"alias"}else if is_static{"static"}else{"namespace"}});
            if let Some(a) = alias {
                extra["csharp_alias"] = json!(a);
            }
            self.edge(
                n,
                "IMPORTS_FROM",
                "<module>",
                &compact(text(*last, s)),
                extra,
            );
        }
        if is_type(n) || is_method(n) {
            let identity = self.type_name(n, class);
            let decorators: Vec<_> = children(n)
                .into_iter()
                .filter(|v| v.kind() == "attribute_list")
                .flat_map(children)
                .filter(|v| v.kind() == "attribute")
                .filter_map(|a| named(a, "name", s))
                .collect();
            let mut extra = json!({"csharp_namespace":scopes(n,s)[0].0});
            let kind = if is_type(n) {
                nested = identity.clone();
                if let Some(types) = child(n, "type_parameter_list") {
                    extra["csharp_arity"] = json!(
                        children(types)
                            .iter()
                            .filter(|v| v.kind() == "type_parameter")
                            .count()
                    );
                }
                if n.kind() != "enum_declaration"
                    && let Some(bases) = child(n, "base_list")
                {
                    for b in children(bases)
                        .into_iter()
                        .filter(|c| c.kind() != "argument_list")
                    {
                        self.edge(n, "INHERITS", &identity, &compact(text(b, s)), json!({}));
                    }
                }
                "Class"
            } else {
                extra["csharp_static"] = json!(
                    children(n)
                        .iter()
                        .any(|v| v.kind() == "modifier" && text(*v, s) == "static")
                );
                let name = named(n, "name", s).unwrap_or_default();
                if decorators
                    .iter()
                    .any(|d| d == "Test" || d == "Fact" || d == "TestMethod")
                    || name.starts_with("Test")
                    || name.starts_with("test_")
                {
                    "Test"
                } else {
                    "Function"
                }
            };
            let params = is_method(n)
                .then(|| {
                    n.child_by_field_name("parameters")
                        .map(|x| text(x, s).into())
                })
                .flatten();
            self.file.syntax.declarations.insert(
                identity.clone(),
                Definition {
                    returns: None,
                    modifiers: None,
                    decorators,
                    extra,
                    params,
                    doc: None,
                },
            );
            self.file.symbols.push(Symbol {
                name: identity,
                kind: kind.into(),
                start_line: n.start_position().row + 1,
                end_line: n.end_position().row + 1,
            });
        }
        if n.kind() == "field_declaration"
            && let Some(v) = child(n, "variable_declaration")
            && let Some(ty) = named(v, "type", s)
        {
            for d in children(v)
                .into_iter()
                .filter(|d| d.kind() == "variable_declarator")
            {
                if let Some(name) = named(d, "name", s) {
                    self.fields.insert((class.into(), name), ty.clone());
                }
            }
        }
        if n.kind() == "property_declaration"
            && let (Some(ty), Some(name)) = (named(n, "type", s), named(n, "name", s))
        {
            self.fields.insert((class.into(), name), ty);
        }
        for c in children(n) {
            self.declarations(c, &nested);
        }
    }
}
