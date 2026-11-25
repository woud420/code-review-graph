use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use tree_sitter::Node;

#[derive(Default)]
pub(super) struct ImportDetails {
    statements: BTreeMap<usize, Statement>,
}

struct Statement {
    module: Option<String>,
    imported: Vec<String>,
    context: Option<&'static str>,
}

impl ImportDetails {
    pub(super) fn unscoped(line: usize) -> Self {
        Self {
            statements: BTreeMap::from([(
                line,
                Statement {
                    module: None,
                    imported: Vec::new(),
                    context: None,
                },
            )]),
        }
    }

    pub(super) fn observe(&mut self, node: Node<'_>, source: &str, language: &str) {
        if language != "python"
            || !matches!(node.kind(), "import_statement" | "import_from_statement")
        {
            return;
        }
        let mut context = None;
        let mut child = node;
        while let Some(parent) = child.parent() {
            if parent.kind() == "function_definition" && context.is_none() {
                context = Some("deferred");
            }
            if parent.kind() == "if_statement"
                && let Some(condition) = parent.child_by_field_name("condition")
            {
                let expr = source[condition.byte_range()].trim();
                let (negated, term) = expr
                    .strip_prefix("not ")
                    .map(|v| (true, v))
                    .unwrap_or((false, expr));
                let type_only = matches!(
                    term,
                    "False"
                        | "0"
                        | "TYPE_CHECKING"
                        | "typing.TYPE_CHECKING"
                        | "typing_extensions.TYPE_CHECKING"
                );
                let consequence = parent.child_by_field_name("consequence") == Some(child);
                if type_only && consequence != negated {
                    context = Some("type_only");
                }
            }
            child = parent;
        }
        let mut cursor = node.walk();
        let imported = node
            .children_by_field_name("name", &mut cursor)
            .map(|n| source[n.child_by_field_name("name").unwrap_or(n).byte_range()].to_owned())
            .collect();
        let module = node
            .child_by_field_name("module_name")
            .map(|n| source[n.byte_range()].to_owned());
        self.statements.insert(
            node.start_position().row + 1,
            Statement {
                module,
                imported,
                context,
            },
        );
    }

    pub(super) fn resolve(
        &self,
        root: &Path,
        relative: &str,
        raw: &str,
        target: String,
        line: usize,
        paths: &BTreeSet<String>,
    ) -> (String, Value) {
        let Some(stmt) = self.statements.get(&line) else {
            return (target, json!({}));
        };
        let mut context = stmt.context;
        if context != Some("type_only")
            && stmt.module.as_deref() == Some(raw)
            && target.ends_with("/__init__.py")
            && let Some(package) = Path::new(&target).parent()
            && root.join(relative).starts_with(package)
            && !stmt.imported.is_empty()
            && stmt.imported.iter().all(|n| {
                let spec = format!(
                    "{}{n}",
                    if raw.ends_with('.') {
                        raw.to_owned()
                    } else {
                        format!("{raw}.")
                    }
                );
                import_target(relative, &spec, "python", paths).is_some()
            })
        {
            context = Some("parent_package");
        }
        let mut extra = json!({});
        if let Some(value) = context {
            extra["import_context"] = value.into();
        }
        if target != raw || raw.starts_with('.') || raw.contains(['/', '\\']) {
            return (target, extra);
        }
        let module_path = PathBuf::from(raw.replace('.', "/"));
        let is_stdlib = include_str!("stdlib-modules.txt")
            .split_whitespace()
            .any(|name| Some(name) == raw.split('.').next());
        let mut matches = Vec::new();
        for file in paths.iter().filter(|p| p.ends_with(".py")) {
            let path = Path::new(file);
            let stem = if path.ends_with("__init__.py") {
                path.parent().map(Path::to_owned).unwrap_or_default()
            } else {
                path.with_extension("")
            };
            if !stem.ends_with(&module_path) {
                continue;
            }
            if is_stdlib
                && let Some(outer) = path.ancestors().nth(module_path.components().count())
                && paths.contains(&outer.join("__init__.py").to_string_lossy().into_owned())
            {
                continue;
            }
            matches.push(root.join(path).to_string_lossy().into_owned());
        }
        extra["python_module"] = raw.into();
        if matches.len() == 1 {
            extra["import_resolution"] = "repository_suffix".into();
            return (matches.pop().unwrap(), extra);
        }
        extra["import_resolution"] = if matches.is_empty() {
            "unresolved"
        } else {
            "ambiguous"
        }
        .into();
        extra["import_candidate_count"] = matches.len().into();
        extra["import_candidates_truncated"] = (matches.len() > 20).into();
        matches.truncate(20);
        extra["import_candidates"] = json!(matches);
        (target, extra)
    }
}

pub(super) fn import_target(
    file: &str,
    module: &str,
    language: &str,
    paths: &BTreeSet<String>,
) -> Option<String> {
    let parent = Path::new(file).parent()?;
    let mut candidates = Vec::new();
    if language == "python" {
        if !module.starts_with('.') {
            return crate::model::python_modules::absolute_module(file, module, |p| {
                paths.contains(p)
            });
        }
        let module_name = module.trim_start_matches('.');
        let depth = module.len() - module_name.len();
        let mut dir = if depth == 0 {
            PathBuf::new()
        } else {
            parent.to_owned()
        };
        for _ in 1..depth {
            if !dir.pop() {
                return None;
            }
        }
        if !module_name.is_empty() {
            dir.extend(module_name.split('.'));
        }
        candidates.push(dir.with_extension("py"));
        candidates.push(dir.join("__init__.py"));
    } else if module.starts_with("./") || module.starts_with("../") {
        let mut dir = parent.to_owned();
        for part in Path::new(module).components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir if dir.pop() => {}
                Component::Normal(name) => dir.push(name),
                _ => return None,
            }
        }
        candidates.push(dir.clone());
        if matches!(
            dir.extension().and_then(|s| s.to_str()),
            Some("js" | "jsx" | "mjs" | "cjs")
        ) {
            for extension in ["ts", "tsx", "mts", "cts"] {
                candidates.push(dir.with_extension(extension));
            }
        } else if dir.extension().is_none() {
            for extension in ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"] {
                candidates.push(dir.with_extension(extension));
                candidates.push(dir.join("index").with_extension(extension));
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|p| {
            p.to_str()
                .map(|s| s.replace(std::path::MAIN_SEPARATOR, "/"))
        })
        .find(|p| paths.contains(p))
}
