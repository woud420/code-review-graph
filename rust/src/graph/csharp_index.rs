use super::{Module, Target};
use crate::model::syntax::Edge;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Default)]
pub(super) struct Bindings {
    pub projects: HashMap<String, String>,
    pub methods: HashMap<(String, String), Vec<Target>>,
    pub global_imports: HashMap<String, Vec<Edge>>,
}

impl Bindings {
    pub(super) fn collect(root: &Path, modules: &BTreeMap<String, Module>) -> Self {
        let mut index = Self::default();
        let mut seen_dirs: HashMap<String, bool> = HashMap::new();
        for (file, module) in modules.iter().filter(|(f, _)| f.ends_with(".cs")) {
            let project = Path::new(file)
                .parent()
                .into_iter()
                .flat_map(Path::ancestors)
                .find(|dir| {
                    *seen_dirs
                        .entry(dir.to_string_lossy().into_owned())
                        .or_insert_with(|| {
                            std::fs::read_dir(root.join(dir))
                                .into_iter()
                                .flatten()
                                .filter_map(Result::ok)
                                .any(|f| f.path().extension().is_some_and(|e| e == "csproj"))
                        })
                })
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default();
            index.projects.insert(file.clone(), project.clone());
            for symbol in &module.facts.symbols {
                if matches!(symbol.kind.as_str(), "Function" | "Test") {
                    index
                        .methods
                        .entry((project.clone(), symbol.name.clone()))
                        .or_default()
                        .push((file.clone(), symbol.name.clone()));
                }
            }
            for edge in &module.facts.syntax.edges {
                if edge.kind == "IMPORTS_FROM" && edge.extra["csharp_global"] == true {
                    index
                        .global_imports
                        .entry(project.clone())
                        .or_default()
                        .push(edge.clone());
                }
            }
        }
        index
    }
}
