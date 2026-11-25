use super::*;
use std::collections::HashMap;
use syntax::{is_field, parents, walk, within};
use tree_sitter::Node;

fn scope(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "module"
            | "function_definition"
            | "class_definition"
            | "lambda"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression"
    )
}
pub(super) fn definition<'a>(root: Node<'a>, src: &Source, t: &Target) -> Option<(Node<'a>, bool)> {
    let mut found = None;
    walk(root, |n| {
        if found.is_none()
            && matches!(n.kind(), "function_definition" | "class_definition")
            && src.field(n, "name") == t.name
        {
            let first = n
                .parent()
                .filter(|p| p.kind() == "decorated_definition")
                .unwrap_or(n)
                .start_position()
                .row
                + 1;
            if first <= t.first && t.first <= n.start_position().row + 1 {
                found = Some((
                    n,
                    parents(n)
                        .find(|n| scope(*n))
                        .is_some_and(|p| p.kind() == "class_definition"),
                ));
            }
        }
        found.is_none()
    });
    found
}
fn binding_scope(n: Node<'_>) -> Node<'_> {
    parents(n).find(|p| scope(*p)).unwrap_or(n)
}
fn module_path(file: &str) -> &str {
    let stem = file
        .strip_suffix(".pyi")
        .or_else(|| file.strip_suffix(".pyw"))
        .or_else(|| file.strip_suffix(".py"))
        .unwrap_or(file);
    stem.strip_suffix("/__init__").unwrap_or(stem)
}
fn suffix(path: &str, tail: &str) -> bool {
    path == tail || path.strip_suffix(tail).is_some_and(|p| p.ends_with('/'))
}
#[derive(Default)]
struct Names {
    bindings: HashMap<usize, (bool, usize)>,
    globals: BTreeSet<usize>,
    nonlocals: BTreeSet<usize>,
    aliases: HashMap<String, String>,
}
impl Names {
    fn bind(&mut self, scope: Node<'_>, target: bool, start: usize) {
        let bind = self.bindings.entry(scope.id()).or_insert((false, start));
        bind.0 |= target;
        bind.1 = bind.1.min(start);
    }
    fn resolves(&self, n: Node<'_>, root: Node<'_>) -> bool {
        let mut entered = false;
        for s in parents(n).filter(|s| scope(*s)) {
            // Defaults and annotations are evaluated before the function scope exists.
            if !declared_name(n)
                && matches!(s.kind(), "function_definition" | "lambda")
                && !s
                    .child_by_field_name("body")
                    .is_some_and(|b| within(&b.byte_range(), &n.byte_range()))
            {
                continue;
            }

            if self.globals.contains(&s.id()) {
                return self.bindings.get(&root.id()).is_some_and(|b| b.0);
            }
            let b = self.bindings.get(&s.id());
            if !self.nonlocals.contains(&s.id())
                && let Some((is_target, start)) = b
                && (s.kind() != "class_definition"
                    || (!entered && (*start <= n.start_byte() || declared_name(n))))
            {
                return *is_target;
            }
            entered = true;
        }
        false
    }
}
fn import(
    n: Node<'_>,
    src: &Source,
    t: &Target,
    exporters: &BTreeSet<String>,
    names: &mut Names,
    scan: &mut Scan,
) {
    let module = n.child_by_field_name("module_name");
    let scope = binding_scope(n);
    let targets = src.imports.get(&(n.start_position().row + 1));
    let raw = module.map(|m| src.value(m)).unwrap_or("");
    let mut base = String::new();
    let dotted = raw.trim_start_matches('.').replace('.', "/");
    let dots = raw.len() - raw.trim_start_matches('.').len();
    if dots > 0 {
        let mut p = Path::new(&src.path).parent().unwrap_or(Path::new(""));
        for _ in 1..dots {
            p = p.parent().unwrap_or(p);
        }
        base = if dotted.is_empty() {
            p.to_string_lossy().into_owned()
        } else {
            p.join(&dotted).to_string_lossy().into_owned()
        };
    } else if let Some(targets) = targets {
        for file in targets {
            let p = module_path(file);
            if suffix(p, &dotted) {
                base = p.to_owned();
                break;
            }
            if let Some((before, _)) = p.rsplit_once(&format!("/{dotted}/")) {
                base = format!("{before}/{dotted}");
            }
        }
    }
    let exports = exporters.iter().any(|f| {
        if !base.is_empty() {
            module_path(f) == base
        } else {
            targets.is_some_and(|v| v.contains(f)) && suffix(module_path(f), &dotted)
        }
    });
    let mut cursor = n.walk();
    for item in n
        .named_children(&mut cursor)
        .filter(|c| Some(*c) != module && c.kind() != "comment")
    {
        if item.kind() == "wildcard_import" {
            if exports && scope == binding_scope(n) && scope.kind() == "module" {
                names.bind(scope, true, item.start_byte());
            }
            continue;
        }
        let original = item.child_by_field_name("name").unwrap_or(item);
        let alias = item.child_by_field_name("alias");
        let original_name = src.value(original);
        let mut bound = alias.map(|a| src.value(a)).unwrap_or(original_name);
        if module.is_none() && alias.is_none() {
            bound = bound.split('.').next().unwrap_or(bound);
        }
        let target = module.is_some() && exports && original_name == t.name;
        if target {
            scan.add(src, original, "import", None);
        }
        if bound == t.name {
            names.bind(scope, target, item.start_byte());
            if target && let Some(a) = alias {
                scan.add(src, a, "import", None);
            }
        }
        if !base.is_empty() {
            names.aliases.insert(
                bound.into(),
                format!("{base}/{}", original_name.replace('.', "/")),
            );
        }
        if module.is_none()
            && let Some(targets) = targets
        {
            let spec = original_name.replace('.', "/");
            if let Some(file) = targets
                .iter()
                .map(|s| module_path(s))
                .find(|f| suffix(f, &spec))
            {
                let base = file.strip_suffix(&spec).unwrap_or("");
                names.aliases.insert(
                    bound.into(),
                    format!("{base}{}", if alias.is_some() { &spec } else { bound }),
                );
            }
        }
    }
}
fn declared_name(n: Node<'_>) -> bool {
    let Some(parent) = n.parent() else {
        return false;
    };
    if matches!(parent.kind(), "function_definition" | "class_definition") {
        return is_field(parent, "name", n);
    }
    for ancestor in parents(n) {
        match ancestor.kind() {
            "parameters"
            | "lambda_parameters"
            | "list_splat_pattern"
            | "dictionary_splat_pattern" => return true,
            "typed_parameter" | "typed_default_parameter" | "default_parameter" => {
                return ancestor
                    .child_by_field_name("name")
                    .or(ancestor.named_child(0))
                    == Some(n);
            }
            "pattern_list" | "tuple_pattern" | "list_pattern" | "as_pattern_target" => continue,
            "as_pattern" => {
                return ancestor
                    .child_by_field_name("alias")
                    .is_some_and(|p| within(&p.byte_range(), &n.byte_range()));
            }
            "assignment"
            | "augmented_assignment"
            | "for_statement"
            | "for_in_clause"
            | "named_expression" => {
                return ["left", "name"]
                    .iter()
                    .filter_map(|k| ancestor.child_by_field_name(k))
                    .any(|p| within(&p.byte_range(), &n.byte_range()));
            }
            _ => return false,
        }
    }
    false
}
pub(super) fn scan(root: Node<'_>, src: &Source, t: &Target, exporters: &BTreeSet<String>) -> Scan {
    let mut scan = Scan {
        authoritative: true,
        ..Default::default()
    };
    let mut names = Names::default();
    let def = (src.path == t.file)
        .then(|| definition(root, src, t))
        .flatten()
        .map(|(n, _)| n);
    let mut uses = Vec::new();
    walk(root, |n| {
        match n.kind() {
            "import_statement" | "import_from_statement" => {
                import(n, src, t, exporters, &mut names, &mut scan);
                return false;
            }
            "assignment" => {
                if let (Some(var), Some(call)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) && var.kind() == "identifier"
                    && call.kind() == "call"
                    && matches!(
                        src.field(call, "function"),
                        "import_module" | "importlib.import_module"
                    )
                    && let Some(args) = call
                        .child_by_field_name("arguments")
                        .filter(|a| a.named_child_count() == 1)
                    && let Some(literal) = args.named_child(0).filter(|s| s.kind() == "string")
                    && let Some(content) = literal
                        .named_children(&mut literal.walk())
                        .find(|s| s.kind() == "string_content")
                    && let Some(file) = exporters
                        .iter()
                        .find(|f| suffix(module_path(f), &src.value(content).replace('.', "/")))
                {
                    names
                        .aliases
                        .insert(src.value(var).into(), module_path(file).into());
                }
            }
            "global_statement" | "nonlocal_statement"
                if n.named_children(&mut n.walk())
                    .any(|c| src.value(c) == t.name) =>
            {
                if n.kind() == "global_statement" {
                    names.globals.insert(binding_scope(n).id());
                } else {
                    names.nonlocals.insert(binding_scope(n).id());
                }
            }
            "string_content" if src.value(n) == t.name => {
                if parents(n)
                    .take_while(|p| !scope(*p) || p.kind() == "module")
                    .any(|p| {
                        matches!(p.kind(), "assignment" | "augmented_assignment")
                            && src.field(p, "left") == "__all__"
                            || p.kind() == "call"
                                && src.field(p, "function").starts_with("__all__.")
                    })
                {
                    uses.push((n, "__all__"));
                }
            }
            "identifier" if src.value(n) == t.name => {
                let Some(p) = n.parent() else {
                    return false;
                };
                if p.kind() == "attribute" && is_field(p, "attribute", n) {
                    uses.push((n, "attribute"));
                    return false;
                }
                if p.kind() == "keyword_argument" && is_field(p, "name", n) {
                    return false;
                }
                let is_def = matches!(p.kind(), "function_definition" | "class_definition")
                    && is_field(p, "name", n);
                if is_def {
                    names.bind(binding_scope(p), Some(p) == def, n.start_byte());
                    if Some(p) == def {
                        scan.add(src, n, "definition", None);
                    }
                    return false;
                }
                if declared_name(n) {
                    let owner = binding_scope(n);
                    let effective =
                        if owner.kind() == "class_definition" && p.kind() == "assignment" {
                            p.end_byte()
                        } else {
                            n.start_byte()
                        };
                    names.bind(owner, false, effective);
                }
                uses.push((
                    n,
                    if p.kind() == "call" && is_field(p, "function", n) {
                        "call"
                    } else {
                        "reference"
                    },
                ));
            }
            _ => (),
        }
        true
    });
    for (n, site) in uses {
        let bound = if site == "attribute" {
            let p = n.parent().unwrap();
            let prefix = src.field(p, "object");
            let mut parts = prefix.split('.');
            let name = parts.next().unwrap_or("");
            names.aliases.get(name).is_some_and(|base| {
                let mut path = base.clone();
                for part in parts {
                    path.push('/');
                    path.push_str(part);
                }
                exporters.iter().any(|s| module_path(s) == path)
            })
        } else {
            names.resolves(n, root)
        };
        if bound {
            scan.add(src, n, site, None);
        }
    }
    scan.exports = names.bindings.get(&root.id()).is_some_and(|b| b.0);
    scan
}
