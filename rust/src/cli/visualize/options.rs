use super::super::{changes, index::location::Location};
use anyhow::{Result, ensure};
use clap::Args;
use std::path::{Path, PathBuf};
#[derive(Args)]
pub(in crate::cli) struct Options {
    #[command(flatten)]
    location: Location,
    #[arg(long,default_value="auto",value_parser=["auto","full","community","file"])]
    pub(super) mode: String,
    #[arg(long,default_value="html",value_parser=["html","json","graphml","cypher","obsidian","svg"])]
    pub(super) format: String,
    #[arg(long)]
    pub(super) serve: bool,
    #[arg(long)]
    pub(super) sidecar: bool,
    #[arg(long)]
    seed_symbol: Vec<String>,
    #[arg(long)]
    pub(super) seed_file: Vec<String>,
    #[arg(long)]
    pub(super) seed_changed: bool,
    #[arg(long)]
    seed_changed_base: Option<String>,
    #[arg(long)]
    pub(super) seed_flow: Option<String>,
    #[arg(long)]
    pub(super) path_from: Option<String>,
    #[arg(long)]
    pub(super) path_to: Option<String>,
    #[arg(long, allow_negative_numbers = true)]
    depth: Option<i64>,
    #[arg(long, allow_negative_numbers = true)]
    render_depth: Option<i64>,
    #[arg(long, allow_negative_numbers = true)]
    max_nodes: Option<i64>,
}
impl Options {
    pub(super) fn database(&self) -> Result<(PathBuf, PathBuf)> {
        let root = crate::cli::root(self.location.repo.as_deref(), false)?;
        let db = crate::cli::store::database(&root, self.location.data_dir.as_deref());
        Ok((root, db))
    }

    pub(super) fn symbols(&self) -> &[String] {
        &self.seed_symbol
    }

    pub(super) fn depth(&self) -> usize {
        self.depth.unwrap_or(2) as usize
    }

    pub(super) fn render_depth(&self) -> usize {
        self.render_depth
            .map(|d| d as usize)
            .unwrap_or(self.depth().min(1))
    }

    pub(super) fn limit(&self) -> usize {
        self.max_nodes.unwrap_or(1500) as usize
    }

    pub(super) fn seeded(&self) -> bool {
        !self.seed_symbol.is_empty()
            || !self.seed_file.is_empty()
            || self.seed_changed
            || self.seed_flow.as_ref().is_some_and(|s| !s.is_empty())
            || self.path_from.is_some()
            || self.path_to.is_some()
    }

    pub(super) fn flags(&self) -> Result<()> {
        let seeded = self.seeded();
        let tune = [
            ("--depth", self.depth),
            ("--render-depth", self.render_depth),
            ("--max-nodes", self.max_nodes),
        ]
        .into_iter()
        .find(|(_, v)| v.is_some())
        .map(|(k, _)| k);
        ensure!(
            self.seed_changed_base.is_none() || self.seed_changed,
            "--seed-changed-base requires --seed-changed"
        );
        if self.format != "html" {
            ensure!(
                !seeded,
                "the seed flags build an interactive page and have no effect on --format {}; drop the seed or use --format html",
                self.format
            );
            if let Some(flag) = tune {
                anyhow::bail!("{flag} has no effect on --format {}", self.format)
            }
            ensure!(
                !self.sidecar,
                "--sidecar has no effect on --format {}",
                self.format
            );
        }
        if let Some(flag) = tune {
            ensure!(
                seeded,
                "{flag} only applies to a seeded view; add --seed-symbol, --seed-file, --seed-changed, --seed-flow or --path-from/--path-to"
            );
        }
        ensure!(
            !seeded || matches!(self.mode.as_str(), "auto" | "full"),
            "--mode {} aggregates the graph into bubbles, which is the opposite of a seeded neighbourhood; use --mode full or drop the seed",
            self.mode
        );
        Ok(())
    }

    pub(super) fn validate_depth(&self) -> Result<()> {
        ensure!(
            self.path_from.is_some() == self.path_to.is_some(),
            "--path-from and --path-to must be given together"
        );
        let d = self.depth.unwrap_or(2);
        ensure!(d >= 0, "depth must be >= 0, got {d}");
        if let Some(r) = self.render_depth {
            ensure!(r >= 0, "render-depth must be >= 0, got {r}");
            ensure!(
                r <= d,
                "render-depth {r} exceeds depth {d}; the payload stops at hop {d}"
            );
        }
        if let Some(n) = self.max_nodes {
            ensure!(n >= 1, "max-nodes must be >= 1, got {n}");
        }
        Ok(())
    }

    pub(super) fn paths(&self, root: &Path) -> Result<Option<Vec<String>>> {
        let mut paths = self.seed_file.clone();
        if self.seed_changed {
            let base = self.seed_changed_base.as_deref().unwrap_or("HEAD~1");
            let changed = if base.starts_with('-')
                || !regex::Regex::new(r"^[A-Za-z0-9_.~^/@{}-]+$")?.is_match(base)
            {
                eprintln!("WARNING: Invalid git ref rejected: {base}");
                Vec::new()
            } else {
                changes::discover(root, base).or_else(|_| changes::discover(root, "HEAD"))?
            };
            if changed.is_empty() {
                eprintln!("No changed files against {base}; nothing to seed.");
                return Ok(None);
            }
            paths.extend(changed);
        }
        Ok(Some(paths))
    }

    pub(super) fn label(&self) -> String {
        let depth = self.depth.unwrap_or(2);
        if let (Some(s), Some(t)) = (&self.path_from, &self.path_to) {
            return format!("path {s} -> {t}, depth {depth}");
        }
        let mut names = self.seed_symbol.clone();
        names.extend(self.seed_file.clone());
        if self.seed_changed {
            names.push("changed files".into())
        }
        if let Some(f) = &self.seed_flow {
            names.push(format!("flow {f}"))
        }
        if names.is_empty() {
            return self.mode.clone();
        }
        let mut shown = names[..names.len().min(3)].join(", ");
        if names.len() > 3 {
            shown.push_str(&format!(", +{} more", names.len() - 3));
        }
        format!("neighbourhood of {shown}, depth {depth}")
    }
}

pub(super) fn print_budget(data: &serde_json::Value) {
    let n = &data["neighbourhood"];
    if n.is_null() {
        return;
    }
    let dropped = n["seeds_dropped"].as_u64().unwrap_or(0);
    if dropped > 0 {
        println!(
            "  --max-nodes {} kept the {} best-connected of {} seed nodes; {dropped} seeds and every context hop were dropped",
            n["max_nodes"],
            n["seeds"].as_array().unwrap().len(),
            n["seeds_requested"]
        );
    } else if n["truncated"] == true {
        println!(
            "  --max-nodes {} trimmed the outer hops; every seed kept, {} nodes shipped",
            n["max_nodes"],
            data["nodes"].as_array().unwrap().len()
        );
    }
}
