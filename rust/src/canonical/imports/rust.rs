use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use toml::Value;
use tree_sitter::Node;

pub(crate) fn segments<'a>(node: Node<'_>, source: &'a str) -> Vec<&'a str> {
    match node.kind() {
        "identifier" | "type_identifier" | "crate" | "self" | "super" => {
            vec![&source[node.byte_range()]]
        }
        "scoped_identifier" | "scoped_type_identifier" => ["path", "name"]
            .into_iter()
            .filter_map(|f| node.child_by_field_name(f))
            .flat_map(|n| segments(n, source))
            .collect(),
        "generic_type" | "generic_function" => node
            .child_by_field_name(if node.kind() == "generic_type" {
                "type"
            } else {
                "function"
            })
            .map(|n| segments(n, source))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub(crate) fn bindings(node: Node<'_>, source: &str) -> Vec<(String, String)> {
    fn visit<'a>(
        node: Node<'_>,
        source: &'a str,
        prefix: &[&'a str],
        out: &mut Vec<(String, String)>,
    ) {
        let mut path = prefix.to_vec();
        match node.kind() {
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    visit(arg, source, prefix, out);
                }
            }
            "scoped_use_list" => {
                if let Some(head) = node.child_by_field_name("path") {
                    path.extend(segments(head, source));
                }
                if let Some(list) = node.child_by_field_name("list") {
                    visit(list, source, &path, out);
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                for item in node.named_children(&mut cursor) {
                    visit(item, source, prefix, out);
                }
            }
            "identifier" | "type_identifier" | "scoped_identifier" | "scoped_type_identifier" => {
                let parts = segments(node, source);
                if let Some(last) = parts.last().copied() {
                    path.extend(parts);
                    out.push((last.to_owned(), path.join("::")));
                }
            }
            "use_as_clause" => {
                if let (Some(original), Some(alias)) = (
                    node.child_by_field_name("path"),
                    node.child_by_field_name("alias"),
                ) {
                    let parts = segments(original, source);
                    if !parts.is_empty() {
                        path.extend(parts);
                        out.push((source[alias.byte_range()].to_owned(), path.join("::")));
                    }
                }
            }
            "self" => {
                if let Some(last) = prefix.last() {
                    out.push((last.to_string(), prefix.join("::")));
                }
            }
            "use_wildcard" => {
                if let Some(head) = node.child_by_field_name("path") {
                    path.extend(segments(head, source));
                }
                if !path.is_empty() {
                    out.push(("*".into(), path.join("::")));
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    visit(node, source, &[], &mut out);
    out
}

#[derive(Clone)]
struct Layout {
    root: PathBuf,
    entry: PathBuf,
}
impl Layout {
    fn load(dir: &Path, manifest: &Value) -> Option<Self> {
        manifest.get("package")?.as_table()?;
        let custom = manifest
            .get("lib")
            .and_then(|lib| lib.get("path"))
            .and_then(Value::as_str);
        let entry = custom
            .into_iter()
            .chain(["src/lib.rs", "src/main.rs"])
            .map(|p| dir.join(p))
            .find(|p| p.is_file())?;
        Some(Self {
            root: entry.parent()?.to_owned(),
            entry,
        })
    }
    fn module(&self, parts: &[&str]) -> Option<PathBuf> {
        if parts.is_empty() {
            return Some(self.entry.clone());
        }
        let dir: PathBuf = parts.iter().collect();
        [dir.with_extension("rs"), dir.join("mod.rs")]
            .into_iter()
            .map(|p| self.root.join(p))
            .find(|p| {
                if !p.is_file() {
                    return false;
                }
                let mut dir = self.root.clone();
                for part in p.strip_prefix(&self.root).ok().into_iter().flatten() {
                    if !fs::read_dir(&dir).is_ok_and(|mut entries| {
                        entries.any(|e| e.is_ok_and(|e| e.file_name() == part))
                    }) {
                        return false;
                    }
                    dir.push(part);
                }
                true
            })
    }
}

pub(crate) struct Project {
    boundary: PathBuf,
    caller: PathBuf,
    layout: Layout,
    dependencies: BTreeMap<String, Layout>,
}

fn manifest(dir: &Path) -> Option<Value> {
    let path = dir.join("Cargo.toml");
    if !path.is_file() {
        return None;
    }
    let source = fs::read_to_string(path).ok()?;
    toml::from_str(&source).ok()
}

impl Project {
    pub(crate) fn load(root: &Path, caller: &Path) -> Option<Self> {
        let caller = root.join(caller).canonicalize().ok()?;
        if !caller.starts_with(root) {
            return None;
        }
        let manifests = caller
            .parent()?
            .ancestors()
            .take_while(|p| p.starts_with(root))
            .take(64)
            .filter_map(|dir| Some((dir, manifest(dir)?)))
            .collect::<Vec<_>>();
        let &(dir, ref data) = manifests
            .iter()
            .find(|(_, m)| m.get("package").is_some_and(Value::is_table))?;
        let layout = Layout::load(dir, data)?;
        let workspace = manifests
            .iter()
            .find(|(_, m)| m.get("workspace").is_some_and(Value::is_table));
        let mut specs = BTreeMap::new();
        for tables in std::iter::once(data).chain(
            data.get("target")
                .and_then(Value::as_table)
                .into_iter()
                .flat_map(|t| t.values()),
        ) {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = tables.get(section).and_then(Value::as_table) {
                    specs.extend(table.iter());
                }
            }
        }
        let mut dependencies = BTreeMap::new();
        for (name, spec) in specs {
            let (base, spec) = if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
                let Some((base, inherited)) = workspace.and_then(|(p, w)| {
                    Some((*p, w.get("workspace")?.get("dependencies")?.get(name)?))
                }) else {
                    continue;
                };
                (base, inherited)
            } else {
                (dir, spec)
            };
            let Some(path) = spec.get("path").and_then(Value::as_str) else {
                continue;
            };
            let Ok(other) = base.join(path).canonicalize() else {
                continue;
            };
            if !other.starts_with(root) {
                continue;
            }
            if let Some(dep) = manifest(&other).and_then(|m| Layout::load(&other, &m)) {
                dependencies.insert(name.clone(), dep.clone());
                dependencies.insert(name.replace('-', "_"), dep);
            }
        }
        Some(Self {
            boundary: root.to_owned(),
            caller,
            layout,
            dependencies,
        })
    }

    pub(crate) fn resolve(&self, module: &str) -> Option<PathBuf> {
        let mut input = module.split("::").filter(|s| !s.is_empty()).peekable();
        let anchor = *input.peek()?;
        let mut layout = &self.layout;
        let mut file = layout.entry.clone();
        let mut current = Vec::<&str>::new();
        let mut anchored = true;
        match anchor {
            "crate" => {
                input.next();
            }
            "super" | "self" => {
                if let Ok(rel) = self.caller.strip_prefix(&layout.root) {
                    let parent = rel.parent()?;
                    for part in parent {
                        current.push(part.to_str()?);
                    }
                    if !matches!(rel.file_name()?.to_str()?, "main.rs" | "lib.rs" | "mod.rs") {
                        current.push(rel.file_stem()?.to_str()?);
                    }
                }
                if anchor == "self" {
                    input.next();
                    file = self.caller.clone();
                } else {
                    while input.peek() == Some(&"super") {
                        input.next();
                        current.pop();
                    }
                    file = layout.module(&current)?;
                }
            }
            other => {
                if let Some(dep) = self.dependencies.get(other) {
                    layout = dep;
                    file = layout.entry.clone();
                    input.next();
                } else {
                    anchored = false;
                }
            }
        }
        let mut reached = false;
        while let Some(part) = input.next() {
            current.push(part);
            if let Some(next) = layout.module(&current) {
                file = next;
                reached = true;
                continue;
            }
            if input.peek().is_some() || (!anchored && !reached) {
                return None;
            }
        }
        let found = file.canonicalize().ok()?;
        found.starts_with(&self.boundary).then_some(found)
    }
}
