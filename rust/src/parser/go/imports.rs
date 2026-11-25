use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
};

fn suffix<'a>(import: &'a str, module: &str) -> Option<&'a str> {
    if import == module {
        Some("")
    } else {
        import.strip_prefix(module)?.strip_prefix('/')
    }
}

pub(crate) fn package_dir(
    root: &Path,
    file: &str,
    import: &str,
    paths: &BTreeSet<String>,
) -> Option<String> {
    if !import.split('/').next()?.contains('.')
        || import.starts_with('.')
        || Path::new(import)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let from = root.join(file);
    let mut candidates = Vec::new();
    let mut vendors = Vec::new();
    for dir in from
        .parent()?
        .ancestors()
        .take_while(|d| d.starts_with(root))
    {
        let manifest = dir.join("go.mod");
        if !manifest.is_file() {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&manifest) else {
            continue;
        };
        let mut replace_block = false;
        let mut replacements = Vec::new();
        let mut own = None;
        for raw in contents.lines() {
            let raw = raw.split("//").next().unwrap_or("").trim();
            if let Some(p) = raw.strip_prefix("module ") {
                own = Some(p.trim().trim_matches(['"', '`']));
            }
            if raw == "replace (" {
                replace_block = true;
                continue;
            }
            if raw == ")" {
                replace_block = false;
                continue;
            }
            let rule = raw
                .strip_prefix("replace ")
                .or_else(|| replace_block.then_some(raw));
            if let Some((lhs, rhs)) = rule.and_then(|r| r.split_once("=>")) {
                let Some(module) = lhs.split_whitespace().next() else {
                    continue;
                };
                let path = rhs.trim().trim_matches(['"', '`']);
                if matches!(path.split('/').next(), Some("." | ".."))
                    || Path::new(path).is_absolute()
                {
                    replacements.push((module.trim_matches(['"', '`']), path));
                }
            }
        }
        if let Some(suffix) = own.and_then(|m| suffix(import, m)) {
            candidates.push(dir.join(suffix));
        }
        replacements.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
        if let Some(target) = replacements
            .into_iter()
            .find_map(|(m, p)| suffix(import, m).map(|s| dir.join(p).join(s)))
        {
            candidates.push(target);
        }
        vendors.push(dir.join("vendor").join(import));
    }
    for candidate in candidates.into_iter().chain(vendors) {
        let Ok(absolute) = candidate.canonicalize() else {
            continue;
        };
        let Ok(relative) = absolute.strip_prefix(root) else {
            continue;
        };
        if paths.iter().any(|p| {
            Path::new(p).parent() == Some(relative)
                && p.ends_with(".go")
                && !p.ends_with("_test.go")
        }) {
            return Some(absolute.to_string_lossy().into_owned());
        }
    }
    None
}
