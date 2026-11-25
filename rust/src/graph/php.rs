use super::{Module, Resolver, Target};
use crate::model::syntax::Site;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Default)]
pub(super) struct Methods(HashMap<(String, String), Vec<Target>>, std::path::PathBuf);

impl Methods {
    pub(super) fn collect(root: &std::path::Path, modules: &BTreeMap<String, Module>) -> Self {
        let mut index = Self(Default::default(), root.into());
        for (file, m) in modules.iter().filter(|(p, _)| p.ends_with(".php")) {
            for sym in &m.facts.symbols {
                if !matches!(sym.kind.as_str(), "Function" | "Test") {
                    continue;
                }
                if let Some((ty, name)) = sym.name.rsplit_once('.') {
                    index
                        .0
                        .entry((ty.to_lowercase(), name.to_lowercase()))
                        .or_default()
                        .push((file.clone(), sym.name.clone()));
                }
            }
        }
        index
    }
}
fn namespaces(module: &Module) -> HashSet<String> {
    module.facts.syntax.file["php_namespaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_lowercase))
        .collect()
}
impl Resolver {
    pub(super) fn php_call(&self, file: &str, site: &Site) -> Option<Target> {
        if let Some(local) = &site.target {
            return Some((file.into(), local.clone()));
        }
        let (scope, method) = if let Some(pair) = site.raw.split_once("::") {
            pair
        } else {
            (
                site.extra["receiver_scope"].as_str()?,
                site.raw.rsplit('.').next()?,
            )
        };
        if matches!(
            scope.to_ascii_lowercase().as_str(),
            "self" | "static" | "parent"
        ) {
            return None;
        }
        let module = &self.modules[file];
        let name = module
            .facts
            .php_imports
            .get(scope)
            .map(String::as_str)
            .unwrap_or(scope);
        let (namespace, ty) = name.rsplit_once('\\').unwrap_or(("", name));
        let candidates = self
            .php
            .0
            .get(&(ty.to_lowercase(), method.to_lowercase()))?;
        let imported = module
            .facts
            .php_imports
            .values()
            .any(|fqn| fqn == name)
            .then(|| {
                crate::parser::php::imports::local_file(&self.php.1, file, name, |p| {
                    self.modules.contains_key(p)
                })
            })
            .flatten();
        if let Some(path) = imported {
            let mut found = candidates.iter().filter(|(f, _)| *f == path);
            let item = found.next()?;
            return found.next().is_none().then(|| item.clone());
        }
        if candidates.len() != 1 {
            return None;
        }
        let (target, id) = &candidates[0];
        let theirs = namespaces(&self.modules[target]);
        let ours = namespaces(module);
        let supported = if namespace.is_empty() {
            !theirs.is_disjoint(&ours) || (ours.is_empty() && theirs.is_empty())
        } else {
            theirs.contains(&namespace.to_lowercase())
                || ours
                    .iter()
                    .any(|p| theirs.contains(&format!("{p}\\{}", namespace.to_lowercase())))
        };
        (target == file || supported).then(|| (target.clone(), id.clone()))
    }
}
