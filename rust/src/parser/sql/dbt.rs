use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(crate) struct Projects {
    configs: HashMap<PathBuf, Option<Vec<PathBuf>>>,
}

impl Projects {
    pub(crate) fn membership(&mut self, root: &Path, file: &Path) -> Option<bool> {
        let full = root.join(file);
        for directory in full
            .parent()?
            .ancestors()
            .take_while(|p| p.starts_with(root))
        {
            let config = self.configs.entry(directory.into()).or_insert_with(|| {
                let project = directory.join("dbt_project.yml");
                if !project.is_file() {
                    return None;
                }
                let content = fs::read_to_string(project).unwrap_or_default();
                Some(
                    crate::parser::yaml::model_paths(&content)
                        .into_iter()
                        .map(|v| {
                            let dir = directory.join(v);
                            dir.canonicalize().unwrap_or(dir)
                        })
                        .collect(),
                )
            });
            if let Some(paths) = config {
                return Some(paths.iter().any(|d| full.starts_with(d)));
            }
        }
        None
    }
}
