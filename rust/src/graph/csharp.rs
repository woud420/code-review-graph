use super::{Resolver, Target};
use crate::model::syntax::Site;
use std::collections::BTreeSet;

impl Resolver {
    pub(super) fn csharp_call(&self, file: &str, site: &Site) -> Option<Target> {
        let project = self.csharp.projects.get(file)?;
        let metadata = &site.extra;
        let raw = metadata["csharp_raw_target"].as_str()?;
        let enclosing = metadata["csharp_containing_type"].as_str().unwrap_or("");
        let mut candidates = Vec::new();
        if let Some((ty, method)) = raw.split_once("::") {
            let ty = if ty == "this" { enclosing } else { ty };
            if ty == enclosing {
                candidates.push(vec![format!("{ty}.{method}")]);
            }
            if ty.contains('.') {
                candidates.push(vec![format!("{ty}.{method}")]);
            }
            for scope in metadata["csharp_scopes"].as_array()? {
                let ns = scope[0].as_str()?;
                if !ns.is_empty() {
                    for ns in std::iter::successors(Some(ns), |s| s.rsplit_once('.').map(|p| p.0)) {
                        candidates.push(vec![format!("{ns}.{ty}.{method}")]);
                    }
                }
            }
            let mut imported = Vec::new();
            for edge in self.modules[file].facts.syntax.edges.iter().chain(
                self.csharp
                    .global_imports
                    .get(project)
                    .into_iter()
                    .flatten(),
            ) {
                if edge.kind != "IMPORTS_FROM" {
                    continue;
                }
                let scope = &edge.extra["csharp_scopes"][0];
                if !metadata["csharp_scopes"].as_array()?.contains(scope) {
                    continue;
                }
                if edge.extra["csharp_using_kind"] == "alias" {
                    if edge.extra["csharp_alias"] == ty {
                        imported.push(format!("{}.{method}", edge.target));
                    }
                } else {
                    imported.push(format!("{}.{ty}.{method}", edge.target));
                }
            }
            candidates.push(imported);
            candidates.push(vec![format!("{ty}.{method}")]);
        } else if metadata["csharp_call_kind"] == "unqualified" && !enclosing.is_empty() {
            candidates.push(vec![format!("{enclosing}.{raw}")]);
        } else {
            return None;
        }
        for names in candidates {
            let hits: BTreeSet<_> = names
                .into_iter()
                .filter_map(|name| self.csharp.methods.get(&(project.clone(), name)))
                .flatten()
                .cloned()
                .collect();
            if !hits.is_empty() {
                return (hits.len() == 1).then(|| hits.into_iter().next().unwrap());
            }
        }
        None
    }
}
