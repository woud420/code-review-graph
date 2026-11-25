use crate::model::{Call, ParsedFile, Symbol, ruby::RubyName, syntax::Site};
use anyhow::{Context, Result};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
use tree_sitter::{Node, Parser};

pub(crate) mod imports;
type Class = (String, Vec<String>);
type Bindings = BTreeMap<String, Option<Class>>;

#[derive(Default, Clone)]
struct Scope {
    classes: Vec<String>,
    caller: String,
    described: Option<Class>,
    bindings: Bindings,
}
impl Scope {
    fn class(&self) -> Option<&str> {
        self.classes.first().map(String::as_str)
    }
    fn caller(&self) -> &str {
        if self.caller.is_empty() {
            "<module>"
        } else {
            &self.caller
        }
    }
}
struct Extractor<'s> {
    src: &'s str,
    file: ParsedFile,
    test: bool,
    shared: Bindings,
}

impl Extractor<'_> {
    fn text(&self, n: Node<'_>) -> &str {
        &self.src[n.byte_range()]
    }
    fn field(&self, n: Node<'_>, key: &str) -> Option<&str> {
        Some(self.text(n.child_by_field_name(key)?))
    }
    fn constant(&self, n: Node<'_>, scope: &Scope) -> Option<Class> {
        if !matches!(n.kind(), "constant" | "scope_resolution") {
            return None;
        }
        let raw = self.text(n);
        let name = raw.trim_start_matches(':').replace("::", ".");
        name.split('.')
            .all(|s| s.chars().next().is_some_and(char::is_uppercase))
            .then(|| {
                (
                    name,
                    if raw.starts_with("::") {
                        vec![]
                    } else {
                        scope.classes.clone()
                    },
                )
            })
    }
    fn constructed(&self, node: Node<'_>, scope: &Scope) -> Option<Class> {
        if node.kind() != "call" || self.field(node, "method") != Some("new") {
            return None;
        }
        let recv = node.child_by_field_name("receiver")?;
        if self.text(recv) == "described_class" {
            return scope.described.clone();
        }
        self.constant(recv, scope)
    }
    fn receiver(&self, node: Node<'_>, scope: &Scope) -> Option<Class> {
        let name = self.text(node);
        if name == "described_class" {
            return scope.described.clone();
        }
        scope
            .bindings
            .get(name)
            .or_else(|| self.shared.get(name))
            .cloned()
            .unwrap_or_else(|| {
                self.constant(node, scope)
                    .or_else(|| self.constructed(node, scope))
            })
    }
    fn scope(&self, n: Node<'_>, s: &Scope) -> Scope {
        let mut scope = s.clone();
        if matches!(n.kind(), "module" | "class")
            && let Some(name) = self.field(n, "name")
        {
            let key = name.trim_start_matches(':').replace("::", ".");
            let key = if !name.starts_with("::")
                && let Some(parent) = s.class()
            {
                format!("{parent}.{key}")
            } else {
                key
            };
            scope.classes.insert(0, key);
        }
        if n.kind() == "call"
            && matches!(self.field(n, "method"), Some("describe" | "context"))
            && let Some(arg) = n
                .child_by_field_name("arguments")
                .and_then(|v| v.named_child(0))
            && let Some(class) = self.constant(arg, s)
        {
            scope.described = Some(class);
        }
        scope
    }
    fn gather(&mut self, n: Node<'_>, s: &Scope) {
        let scope = self.scope(n, s);
        if n.kind() == "assignment"
            && let Some(left) = n
                .child_by_field_name("left")
                .filter(|v| v.kind() == "instance_variable")
        {
            let class = n
                .child_by_field_name("right")
                .and_then(|v| self.constructed(v, s));
            self.share(self.text(left).into(), class);
        }
        if n.kind() == "call"
            && let Some(method @ ("let" | "let!" | "subject" | "subject!")) =
                self.field(n, "method")
        {
            let memo = n
                .child_by_field_name("arguments")
                .and_then(|v| v.named_child(0))
                .map(|v| self.text(v).trim_matches([':', '\'', '"']).to_string())
                .or_else(|| method.starts_with("subject").then(|| "subject".into()));
            if let Some(key) = memo {
                let class = n
                    .child_by_field_name("block")
                    .and_then(|b| b.child_by_field_name("body"))
                    .and_then(|b| b.named_children(&mut b.walk()).last())
                    .and_then(|v| self.constructed(v, &scope));
                self.share(key, class);
            }
        }
        for child in n.named_children(&mut n.walk()) {
            self.gather(child, &scope);
        }
    }
    fn share(&mut self, key: String, class: Option<Class>) {
        self.shared
            .entry(key)
            .and_modify(|old| {
                if *old != class {
                    *old = None
                }
            })
            .or_insert(class);
    }
    fn declare(
        &mut self,
        n: Node<'_>,
        name: &str,
        id: &str,
        parent: Option<&str>,
        container: &str,
        kind: &str,
    ) {
        self.file.symbols.push(Symbol {
            name: id.into(),
            kind: kind.into(),
            start_line: n.start_position().row + 1,
            end_line: n.end_position().row + 1,
        });
        self.file.ruby.names.insert(
            id.into(),
            RubyName {
                name: name.into(),
                parent: parent.map(String::from),
                container: container.into(),
            },
        );
    }
    fn import(&mut self, n: Node<'_>, method: &str) -> bool {
        if !matches!(
            method,
            "require" | "require_relative" | "autoload" | "load" | "require_all"
        ) {
            return false;
        }
        let recv = n.child_by_field_name("receiver");
        if recv.is_some_and(|p| {
            method != "autoload" || !matches!(p.kind(), "constant" | "self" | "scope_resolution")
        }) {
            return false;
        }
        let idx = u32::from(method == "autoload");
        let Some(path) = n
            .child_by_field_name("arguments")
            .and_then(|v| v.named_child(idx))
            .filter(|v| v.kind() == "string")
        else {
            return false;
        };
        if path
            .named_children(&mut path.walk())
            .any(|c| c.kind() == "interpolation")
        {
            return false;
        }
        let raw = self.text(path).trim_matches(['\'', '"']).to_string();
        self.file
            .ruby
            .imports
            .push((method.into(), raw, n.start_position().row + 1));
        true
    }
    fn visit(&mut self, n: Node<'_>, scope: &mut Scope) {
        let mut inner = self.scope(n, scope);
        match n.kind() {
            "module" | "class" => {
                let Some(id) = inner.class() else { return };
                let (parent, name) = id
                    .rsplit_once('.')
                    .map_or((None, id), |(p, s)| (Some(p), s));
                self.declare(
                    n,
                    name,
                    id,
                    parent,
                    scope.class().unwrap_or("<module>"),
                    "Class",
                );
            }
            "method" | "singleton_method" => {
                let Some(name) = self.field(n, "name").map(str::to_owned) else {
                    return;
                };
                let id = scope
                    .class()
                    .map_or_else(|| name.clone(), |p| format!("{p}.{name}"));
                let kind = if name.starts_with("test_")
                    || name.ends_with("_test")
                    || name.ends_with("_spec")
                {
                    "Test"
                } else {
                    "Function"
                };
                self.declare(
                    n,
                    &name,
                    &id,
                    scope.class(),
                    scope.class().unwrap_or("<module>"),
                    kind,
                );
                inner.caller = id;
                inner.bindings.clear();
            }
            "call" => {
                let Some(method) = self.field(n, "method").map(str::to_owned) else {
                    return;
                };
                if self.import(n, &method) {
                    return;
                }
                let recv = n.child_by_field_name("receiver");
                let line = n.start_position().row + 1;
                if self.test
                    && n.child_by_field_name("block").is_some()
                    && recv.is_none_or(|r| self.text(r) == "RSpec")
                    && matches!(
                        method.as_str(),
                        "describe"
                            | "context"
                            | "it"
                            | "specify"
                            | "example"
                            | "test"
                            | "feature"
                            | "scenario"
                            | "shared_examples"
                            | "shared_context"
                            | "before"
                            | "after"
                            | "around"
                            | "setup"
                            | "teardown"
                            | "let"
                            | "let!"
                            | "subject"
                            | "subject!"
                    )
                {
                    let desc = n
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                        .filter(|p| {
                            matches!(
                                p.kind(),
                                "string" | "constant" | "scope_resolution" | "simple_symbol"
                            )
                        })
                        .map(|p| {
                            self.text(p)
                                .trim_matches(['\'', '"', ':'])
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                                .replace("::", ".")
                                .chars()
                                .take(200)
                                .collect::<String>()
                        })
                        .filter(|v| !v.is_empty());
                    let name = desc.map_or_else(
                        || format!("{method}@L{line}"),
                        |s| format!("{method}:{s}@L{line}"),
                    );
                    let id = scope
                        .class()
                        .map_or_else(|| name.clone(), |p| format!("{p}.{name}"));
                    let container = if scope.caller.is_empty() {
                        scope.class().unwrap_or("<module>")
                    } else {
                        scope.caller()
                    };
                    self.declare(n, &name, &id, scope.class(), container, "Test");
                    inner.caller = id;
                } else {
                    let mut extra = json!({});
                    if self.test
                        && let Some(r) = recv.filter(|v| v.kind() != "self")
                    {
                        extra = if let Some((class, scopes)) = self.receiver(r, scope) {
                            let mut m = json!({"receiver_class":class,"receiver_binding":"class","receiver_constant":true});
                            if !scopes.is_empty() {
                                m["receiver_scopes"] = json!(scopes);
                            }
                            m
                        } else {
                            json!({"receiver_binding":"unknown","receiver_form":"expression"})
                        };
                    }
                    let caller = scope.caller().to_owned();
                    self.file.calls.push(Call {
                        caller: caller.clone(),
                        callee: method.clone(),
                        line,
                    });
                    self.file.syntax.sites.entry(line).or_default().push(Site {
                        caller,
                        raw: method,
                        target: None,
                        extra,
                    });
                }
            }
            _ => {}
        }
        if matches!(
            n.kind(),
            "method" | "singleton_method" | "block" | "do_block"
        ) && let Some(params) = n.child_by_field_name("parameters")
        {
            for p in params
                .named_children(&mut params.walk())
                .filter(|c| c.kind() == "identifier")
            {
                inner.bindings.insert(self.text(p).into(), None);
            }
        }
        for child in n.named_children(&mut n.walk()) {
            self.visit(child, &mut inner);
        }
        if n.kind() == "assignment"
            && let Some(left) = n
                .child_by_field_name("left")
                .filter(|l| l.kind() == "identifier")
        {
            let class = n
                .child_by_field_name("right")
                .and_then(|v| self.constructed(v, scope));
            scope.bindings.insert(self.text(left).into(), class);
        }
        if !matches!(
            n.kind(),
            "method"
                | "singleton_method"
                | "block"
                | "do_block"
                | "class"
                | "module"
                | "assignment"
        ) {
            scope.bindings = inner.bindings;
        }
    }
    fn bind(&mut self) {
        let mut by_name = BTreeMap::<_, Vec<_>>::new();
        for (id, d) in &self.file.ruby.names {
            by_name.entry(d.name.as_str()).or_default().push(id);
        }
        for site in self.file.syntax.sites.values_mut().flatten() {
            if site.extra.get("receiver_binding").is_some() {
                continue;
            }
            let Some(candidates) = by_name.get(site.raw.as_str()) else {
                continue;
            };
            if candidates.len() == 1 {
                site.target = Some(candidates[0].clone());
                continue;
            }
            let parent = self
                .file
                .ruby
                .names
                .get(&site.caller)
                .and_then(|s| s.parent.as_ref());
            site.target = candidates
                .iter()
                .find(|id| {
                    self.file
                        .ruby
                        .names
                        .get(id.as_str())
                        .and_then(|s| s.parent.as_ref())
                        == parent
                })
                .map(|n| (*n).clone());
        }
    }
}

pub(super) fn parse(path: &Path, src: &str) -> Result<ParsedFile> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_ruby::LANGUAGE.into())?;
    let tree = parser.parse(src, None).context("parsing Ruby")?;
    let mut ruby = Extractor {
        src,
        file: ParsedFile::default(),
        test: crate::model::test_file(&path.to_string_lossy(), None),
        shared: BTreeMap::new(),
    };
    if ruby.test {
        ruby.gather(tree.root_node(), &Scope::default());
    }
    ruby.visit(tree.root_node(), &mut Scope::default());
    ruby.bind();
    Ok(ruby.file)
}
