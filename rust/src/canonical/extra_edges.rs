use super::{field, import_target, text};
use crate::model::ParsedFile;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

#[derive(Default)]
pub(in crate::canonical) struct ExtraEdges {
    references: Vec<(Option<usize>, String, usize)>,
    defined: BTreeSet<String>,
    imported: BTreeMap<String, (String, String)>,
    submodule_imports: Vec<(String, usize)>,
    implements: Vec<(String, String, usize)>,
}

impl ExtraEdges {
    pub(super) fn collect(root: Node<'_>, source: &str, language: &str) -> Self {
        let mut facts = Self::default();
        if !matches!(language, "python" | "rust") {
            return facts;
        }
        let mut pending = vec![(root, None)];
        while let Some((node, owner)) = pending.pop() {
            let line = node.start_position().row + 1;
            let mut owner = owner;
            match node.kind() {
                "function_definition" | "class_definition" => {
                    let mut parent = node.parent();
                    if parent.is_some_and(|n| n.kind() == "decorated_definition") {
                        parent = parent.and_then(|n| n.parent());
                    }
                    if parent == Some(root)
                        && let Some(name) = field(node, "name", source)
                    {
                        facts.defined.insert(name.to_owned());
                    }
                    owner = if node.kind() == "class_definition" {
                        None
                    } else {
                        Some(line)
                    };
                }
                "import_statement" | "import_from_statement" => {
                    let module = field(node, "module_name", source);
                    let mut c = node.walk();
                    for entry in node.children_by_field_name("name", &mut c) {
                        let name = field(entry, "name", source).unwrap_or(text(entry, source));
                        let local = field(entry, "alias", source).unwrap_or(name);
                        if let Some(module) = module {
                            let separator = if module.ends_with('.') { "" } else { "." };
                            facts
                                .submodule_imports
                                .push((format!("{module}{separator}{name}"), line));
                        }
                        if node.parent() != Some(root) || module.is_none() {
                            continue;
                        }
                        facts.imported.insert(
                            local.to_owned(),
                            (
                                module.unwrap_or(name).to_owned(),
                                if module.is_some() {
                                    name.to_owned()
                                } else {
                                    String::new()
                                },
                            ),
                        );
                    }
                }
                "identifier" if language == "python" && in_value_position(node) => {
                    let name = text(node, source);
                    if name.chars().count() > 1
                        && name.chars().any(|c| c.is_lowercase())
                        && !matches!(
                            name,
                            "true"
                                | "false"
                                | "null"
                                | "undefined"
                                | "self"
                                | "this"
                                | "cls"
                                | "super"
                                | "None"
                                | "True"
                                | "False"
                        )
                    {
                        facts.references.push((owner, name.to_owned(), line));
                    }
                }
                "impl_item" => {
                    if let (Some(ty), Some(tr)) =
                        (field(node, "type", source), field(node, "trait", source))
                    {
                        facts.implements.push((ty.to_owned(), tr.to_owned(), line));
                    }
                }
                _ => {}
            }
            let mut c = node.walk();
            pending.extend(node.named_children(&mut c).map(|child| (child, owner)));
        }
        facts
    }

    pub(in crate::canonical) fn direct_name(&self, name: &str) -> bool {
        self.defined.contains(name) || self.imported.contains_key(name)
    }

    pub(in crate::canonical) fn rows(
        &self,
        root: &Path,
        file: &str,
        facts: &ParsedFile,
        paths: &BTreeSet<String>,
    ) -> Vec<(&'static str, String, String, usize)> {
        let path = root.join(file).to_string_lossy().into_owned();
        let symbols: BTreeMap<_, _> = facts
            .symbols
            .iter()
            .map(|s| (s.start_line, &s.name))
            .collect();
        let mut out = Vec::new();
        for (owner, name, line) in &self.references {
            let target = if self.defined.contains(name) {
                format!("{path}::{name}")
            } else if let Some((module, original)) = self.imported.get(name) {
                import_target(file, module, "python", paths)
                    .map(|f| {
                        let path = root.join(f).to_string_lossy().into_owned();
                        if original.is_empty() {
                            path
                        } else {
                            format!("{path}::{original}")
                        }
                    })
                    .unwrap_or_else(|| name.clone())
            } else {
                continue;
            };
            let source = owner
                .and_then(|line| symbols.get(&line))
                .map(|name| format!("{path}::{name}"))
                .unwrap_or_else(|| path.clone());
            out.push(("REFERENCES", source, target, *line));
        }
        for (module, line) in &self.submodule_imports {
            if let Some(target) = import_target(file, module, "python", paths) {
                let target = root.join(target).to_string_lossy().into_owned();
                out.push(("IMPORTS_FROM", path.clone(), target, *line));
            }
        }
        for (ty, tr, line) in &self.implements {
            let target = if facts.symbols.iter().any(|s| s.name == *tr) {
                format!("{path}::{tr}")
            } else {
                tr.clone()
            };
            out.push(("IMPLEMENTS", format!("{path}::{ty}"), target, *line));
        }
        out
    }
}

fn in_value_position(node: Node<'_>) -> bool {
    let Some(p) = node.parent() else {
        return false;
    };
    match p.kind() {
        "argument_list" | "list" => true,
        "pair" | "keyword_argument" => p.child_by_field_name("value") == Some(node),
        "assignment" | "augmented_assignment" => {
            p.child_by_field_name("right") == Some(node)
                && p.child_by_field_name("left")
                    .is_some_and(|l| matches!(l.kind(), "attribute" | "subscript"))
        }
        _ => false,
    }
}
