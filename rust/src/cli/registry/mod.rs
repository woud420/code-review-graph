mod search;
use anyhow::{Context, Result};
pub use code_review_graph_rs::registry::list;
pub(super) use search::run as cross_search;
use std::path::Path;
pub fn register(path: &Path, alias: Option<String>) -> Result<()> {
    let entry = code_review_graph_rs::registry::register_repo(path, alias)?;
    let path = entry["path"].as_str().context("missing registered path")?;
    let alias = entry["alias"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| format!(" (alias: {s})"))
        .unwrap_or_default();
    println!("Registered: {path}{alias}");
    Ok(())
}
pub fn unregister(raw: &str) -> Result<()> {
    let found = code_review_graph_rs::registry::unregister_repo(raw)?;
    if !found {
        println!("Not found: {raw}");
        std::process::exit(1);
    }
    println!("Unregistered: {raw}");
    Ok(())
}

pub fn print() -> Result<()> {
    let entries = list()?;
    if entries.is_empty() {
        println!(
            "No repositories registered.\nUse: code-review-graph register <path> [--alias name]"
        );
    }
    for v in entries {
        let path = v["path"]
            .as_str()
            .context("registered repository is missing its path")?;
        let alias = v["alias"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!("  ({s})"))
            .unwrap_or_default();
        println!("  {path}{alias}");
        if let Some(data_dir) = v["data_dir"].as_str().filter(|s| !s.is_empty()) {
            let suffix = if Path::new(data_dir).is_dir() {
                ""
            } else {
                " [MISSING]"
            };
            println!("    data_dir: {data_dir}{suffix}");
        }
    }
    Ok(())
}
