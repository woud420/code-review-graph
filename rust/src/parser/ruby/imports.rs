use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

fn normalized(base: &Path, spec: &str) -> Option<String> {
    if spec.is_empty() || Path::new(spec).is_absolute() {
        return None;
    }
    let mut p = PathBuf::from(base);
    for component in Path::new(spec).components() {
        match component {
            Component::Normal(c) => p.push(c),
            Component::CurDir => {}
            Component::ParentDir if p.pop() => {}
            _ => return None,
        }
    }
    Some(p.to_string_lossy().into_owned())
}

pub(crate) fn resolve(
    root: &Path,
    file: &str,
    kind: &str,
    spec: &str,
    files: &BTreeSet<String>,
) -> (String, Value) {
    let parent = Path::new(file).parent().unwrap_or(Path::new(""));
    let mut dirs = if kind == "require_relative" {
        vec![parent.into()]
    } else {
        vec![
            PathBuf::from("lib"),
            PathBuf::from("test"),
            PathBuf::from("spec"),
        ]
    };
    if kind == "require_all" {
        dirs.insert(0, parent.to_owned());
    }
    if kind != "require_relative" && root.join("config/application.rb").is_file() {
        dirs.push("app".into());
        if let Ok(entries) = root.join("app").read_dir() {
            let mut children: Vec<_> = entries
                .flatten()
                .map(|e| Path::new("app").join(e.file_name()))
                .collect();
            children.sort();
            dirs.extend(children);
        }
    }
    let candidates = if spec.ends_with(".rb") {
        vec![spec.into()]
    } else {
        vec![format!("{spec}.rb"), spec.into()]
    };
    for dir in dirs {
        if kind == "require_all"
            && let Some(s) = normalized(&dir, spec)
        {
            let prefix = format!("{s}/");
            if files
                .range(prefix.clone()..)
                .take_while(|f| f.starts_with(&prefix))
                .any(|f| f.ends_with(".rb"))
            {
                return (
                    root.join(s).to_string_lossy().into_owned(),
                    json!({"import_scope":"tree"}),
                );
            }
        }
        for c in &candidates {
            if let Some(s) = normalized(&dir, c)
                && files.contains(&s)
            {
                return (root.join(s).to_string_lossy().into_owned(), json!({}));
            }
        }
    }
    (spec.into(), json!({}))
}
