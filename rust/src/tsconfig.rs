use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    base_url: Option<String>,
    #[serde(default)]
    paths: BTreeMap<String, Vec<String>>,
}

pub(crate) struct TsPaths(BTreeMap<PathBuf, Options>);
impl TsPaths {
    pub(crate) fn load<'a>(root: &Path, files: impl Iterator<Item = &'a String>) -> Self {
        let mut configs = BTreeMap::new();
        for file in files {
            let path = Path::new(file);
            if !path
                .file_name()
                .is_some_and(|n| n == "tsconfig.json" || n == "jsconfig.json")
            {
                continue;
            }
            let Some(data) = std::fs::read(root.join(path))
                .ok()
                .and_then(|s| serde_json::from_slice::<serde_json::Value>(&s).ok())
            else {
                continue;
            };
            if let Some(options) = data
                .get("compilerOptions")
                .and_then(|v| serde_json::from_value::<Options>(v.clone()).ok())
            {
                configs.insert(path.parent().unwrap_or(Path::new("")).to_owned(), options);
            }
        }
        Self(configs)
    }
    pub(crate) fn resolve(
        &self,
        from: &str,
        spec: &str,
        exists: impl Fn(&str) -> bool,
    ) -> Option<String> {
        if spec.starts_with('.') || Path::new(spec).is_absolute() {
            return None;
        }
        let (dir, config) = Path::new(from)
            .parent()?
            .ancestors()
            .find_map(|dir| self.0.get(dir).map(|c| (dir, c)))?;
        let mut matches = config
            .paths
            .iter()
            .filter_map(|(pattern, targets)| {
                if pattern == spec {
                    return Some((usize::MAX, "", targets));
                }
                let (a, b) = pattern.split_once('*')?;
                let middle = spec.strip_prefix(a)?.strip_suffix(b)?;
                Some((a.len(), middle, targets))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|m| std::cmp::Reverse(m.0));
        let mut probes = matches
            .into_iter()
            .next()
            .map(|(_, middle, targets)| {
                targets
                    .iter()
                    .map(|t| t.replace('*', middle))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if config.base_url.is_some() {
            probes.push(spec.to_owned());
        }
        for probe in probes {
            let src = dir
                .join(config.base_url.as_deref().unwrap_or("."))
                .join(probe);
            let mut base = PathBuf::new();
            for part in src.components() {
                match part {
                    Component::CurDir => {}
                    Component::ParentDir if base.pop() => {}
                    Component::Normal(n) => base.push(n),
                    _ => return None,
                }
            }
            let stem = base.to_str()?;
            let mut candidates = vec![stem.to_owned()];
            for ext in ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"] {
                candidates.push(format!("{stem}.{ext}"));
                candidates.push(format!("{stem}/index.{ext}"));
            }
            if let Some(file) = candidates.into_iter().find(|p| exists(p)) {
                return Some(file);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::canonical::{index, postprocess::Level};
    use anyhow::Result;
    use rusqlite::Connection;
    use std::fs;

    #[test]
    fn edited_alias_refreshes_callers_even_when_their_source_is_unchanged() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        for name in ["one", "two"] {
            fs::create_dir(root.join(name))?;
            fs::write(
                root.join(name).join("worker.ts"),
                "export function run() {}",
            )?;
        }
        fs::write(
            root.join("app.ts"),
            "import {run} from '@/worker'; export function start(){run();}",
        )?;
        let config = |folder: &str| {
            fs::write(
                root.join("tsconfig.json"),
                serde_json::json!({"compilerOptions":{"paths":{"@/*":[format!("{folder}/*")]}}})
                    .to_string(),
            )
        };
        let db_path = root.join(".code-review-graph/graph.db");
        config("one")?;
        index(&root, &db_path, true, Level::Full, false)?;
        let db = Connection::open(&db_path)?;
        let target = || -> Result<String> {
            Ok(db.query_row("SELECT target_qualified FROM edges WHERE kind='CALLS' AND source_qualified LIKE '%/app.ts::start'",[],|r|r.get(0))?)
        };
        assert!(target()?.ends_with("/one/worker.ts::run"));
        config("two")?;
        index(&root, &db_path, false, Level::Full, false)?;
        assert!(target()?.ends_with("/two/worker.ts::run"));
        Ok(())
    }
}
