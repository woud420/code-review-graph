use super::Resolver;
use crate::model::syntax::Site;
use std::path::Path;

impl Resolver {
    pub(super) fn java_constructor(&self, file: &str, site: &Site) -> Option<(String, String)> {
        let name = &site.raw;
        if site.target.is_some()
            || site.extra.get("receiver").is_some()
            || !name.starts_with(char::is_uppercase)
            || !name.chars().all(|c| c == '_' || c.is_alphanumeric())
            || self.modules[file].facts.syntax.edges.iter().any(|e| {
                e.kind == "IMPORTS_FROM" && e.target.rsplit('.').next() == Some(name.as_str())
            })
        {
            return None;
        }
        let directory = Path::new(file).parent()?;
        let filename = format!("{name}.java");
        let adjacent = directory.join(&filename);
        if let Some(path) = adjacent.to_str().filter(|s| self.modules.contains_key(*s)) {
            return Some((path.to_owned(), name.clone()));
        }
        let root = directory.ancestors().find(|p| {
            p.file_name().is_some_and(|n| n == "java")
                && p.parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .is_some_and(|s| s == "src")
        })?;
        let current = root.parent()?;
        if current.file_name()?.to_str()? == "main" {
            return None;
        }
        let under = current.parent()?;
        let target = directory.strip_prefix(root).ok()?.join(filename);
        self.modules
            .keys()
            .filter_map(|p| {
                let path = Path::new(p).strip_prefix(under).ok()?;
                let mut parts = path.components();
                let set = parts.next()?.as_os_str();
                if parts.next()?.as_os_str() == "java" && parts.as_path() == target {
                    Some(((set != "main", set), p))
                } else {
                    None
                }
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, p)| (p.clone(), name.clone()))
    }
}
