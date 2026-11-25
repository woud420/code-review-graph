use super::*;

pub(super) fn collect(n: Node<'_>, s: &str, file: &mut ParsedFile) {
    let mut prefix = String::new();
    let mut ast = n;
    if let Some(group) = child(n, "namespace_use_group") {
        if let Some(pre) = child(n, "namespace_name") {
            prefix = format!("{}\\", text(pre, s));
        }
        ast = group;
    }
    for d in children(ast)
        .into_iter()
        .filter(|d| d.kind() == "namespace_use_clause")
    {
        let Some(path) = d.named_child(0) else {
            continue;
        };
        let target = format!("{prefix}{}", text(path, s).trim_start_matches('\\'));
        let alias = field(d, "alias", s)
            .or_else(|| target.rsplit('\\').next())
            .unwrap_or(&target)
            .to_owned();
        file.php_imports.insert(alias, target.clone());
        edge(file, n, "IMPORTS_FROM", "<module>", &target);
    }
}

pub(crate) fn local_file(
    root: &std::path::Path,
    caller: &str,
    module: &str,
    indexed: impl Fn(&str) -> bool,
) -> Option<String> {
    use std::path::Path;
    let parent = Path::new(caller).parent()?;
    let relative = |path: std::path::PathBuf| {
        let absolute = root.join(path).canonicalize().ok()?;
        let local = absolute.strip_prefix(root).ok()?.to_str()?;
        indexed(local).then(|| local.to_owned())
    };
    if module.contains('/') || module.ends_with(".php") {
        return relative(parent.join(module));
    }
    let namespace = module.trim_start_matches('\\');
    let parts = format!("{}.php", namespace.replace('\\', "/"));
    for dir in parent.ancestors() {
        let manifest = root.join(dir).join("composer.json");
        if !manifest.exists() {
            continue;
        }
        let mut mappings = Vec::new();
        if let Ok(config) = manifest.canonicalize()
            && config.starts_with(root)
            && let Ok(contents) = std::fs::read(config)
            && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&contents)
        {
            for group in ["autoload", "autoload-dev"] {
                if let Some(psr) = json[group]["psr-4"].as_object() {
                    for (prefix, dest) in psr {
                        if !namespace.starts_with(prefix) {
                            continue;
                        }
                        let suffix =
                            format!("{}.php", namespace[prefix.len()..].replace('\\', "/"));
                        for item in dest
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v.as_str())
                            .chain(dest.as_str())
                        {
                            mappings.push((
                                std::cmp::Reverse(prefix.len()),
                                dir.join(item).join(&suffix),
                            ));
                        }
                    }
                }
            }
        }
        mappings.sort_by_key(|(length, _)| *length);
        if let Some(found) = mappings.into_iter().find_map(|(_, p)| relative(p)) {
            return Some(found);
        }
        break;
    }
    parent
        .ancestors()
        .find_map(|dir| relative(dir.join(&parts)))
}
