mod compare;
mod edits;
mod sources;

use anyhow::{Result, ensure};
use code_review_graph_rs::canonical::{Sources, index, index_sources, postprocess::Level};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

const KINDS: [&str; 7] = [
    "trailing_comment",
    "rename_function",
    "delete_file",
    "add_file",
    "move_function",
    "change_import",
    "revert",
];

fn git(tree: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(tree)
        .args([
            "-c",
            "user.email=benchmark@code-review-graph.invalid",
            "-c",
            "user.name=crg-benchmark",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    ensure!(
        out.status.success(),
        "git {} failed in {}: {}",
        args.join(" "),
        tree.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct Scratch {
    tree: PathBuf,
    dir: PathBuf,
    baseline: PathBuf,
    base: String,
    targets: edits::Targets,
}
impl Scratch {
    fn create(repo: &Path, dir: &Path) -> Result<Self> {
        let tracked = git(repo, &["ls-files", "-z"])?;
        ensure!(
            !tracked.is_empty(),
            "{} has no git-tracked files to copy",
            repo.display()
        );
        let tree = dir.join("tree");
        fs::create_dir(&tree)?;
        let tree = tree.canonicalize()?;
        for name in tracked.split('\0').filter(|s| !s.is_empty()) {
            let name = Path::new(name);
            ensure!(
                name.components().all(|c| matches!(c, Component::Normal(_))),
                "invalid tracked path"
            );
            let source = repo.join(name);
            if !source.is_file() || source.is_symlink() {
                continue;
            }
            let target = tree.join(name);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::copy(source, target)?;
        }
        git(&tree, &["init", "-q"])?;
        git(&tree, &["add", "-A"])?;
        git(&tree, &["commit", "-q", "--no-verify", "-m", "baseline"])?;
        let base = git(&tree, &["rev-parse", "HEAD"])?.trim().to_owned();
        // The native writer creates a .gitignore next to its database. Keep that
        // directory outside every ancestor of the throwaway source tree.
        let graphs = dir.join("graphs");
        fs::create_dir(&graphs)?;
        let baseline = graphs.join("baseline.db");
        index(&tree, &baseline, true, Level::Full, false)?;
        let targets = edits::Targets::load(&Connection::open(&baseline)?, &tree)?;
        Ok(Self {
            tree,
            dir: graphs,
            baseline,
            base,
            targets,
        })
    }

    fn reset(&self) -> Result<()> {
        git(&self.tree, &["reset", "-q", "--hard", &self.base])?;
        git(&self.tree, &["clean", "-qfd"])?;
        Ok(())
    }

    fn commit(&self, message: &str) -> Result<String> {
        git(&self.tree, &["add", "-A"])?;
        git(
            &self.tree,
            &[
                "commit",
                "-q",
                "--no-verify",
                "--allow-empty",
                "-m",
                message,
            ],
        )?;
        Ok(git(&self.tree, &["rev-parse", "HEAD"])?.trim().to_owned())
    }

    fn update(&self, db: &Path, base: &str) -> Result<()> {
        index_sources(
            &self.tree,
            db,
            false,
            Level::Full,
            false,
            None,
            Sources::Build {
                quiet: true,
                follow: None,
                base: Some(base),
                recurse_submodules: None,
            },
        )?;
        Ok(())
    }

    fn scenario(&self, kind: &str, row: &mut Value) -> Result<()> {
        let edit = self.targets.apply(&self.tree, kind)?;
        row["note"] = json!(edit.note);
        if edit.target.is_empty() {
            return Ok(());
        }
        row["target"] = json!(edit.target);
        let updated = self.dir.join(format!("{kind}_incremental.db"));
        let rebuilt = self.dir.join(format!("{kind}_rebuild.db"));
        // The writer closed its transaction at the end of the baseline build.
        for suffix in ["", "-wal", "-shm"] {
            let old = PathBuf::from(format!("{}{suffix}", self.baseline.display()));
            if old.is_file() {
                fs::copy(old, format!("{}{suffix}", updated.display()))?;
            }
        }
        let edited = self.commit(&format!("benchmark: {kind}"))?;
        self.update(&updated, &self.base)?;
        if kind == "revert" {
            self.reset()?;
            self.commit("benchmark: revert")?;
            self.update(&updated, &edited)?;
            row["note"] = json!("appended then removed one comment line");
        }
        index(&self.tree, &rebuilt, true, Level::Full, false)?;
        compare::record(row, &updated, &rebuilt, &self.tree)
    }
}

fn blank(name: &str, kind: &str) -> Value {
    let mut row = json!({"repo":name,"edit_kind":kind,"status":"skipped","known_failure":1,"target":"","total_differing_rows":0,"worst_table":"",
        "snapshot_nodes_only_in_rebuild":0,"snapshot_nodes_only_in_incremental":0,"snapshot_edges_only_in_rebuild":0,"snapshot_edges_only_in_incremental":0,
        "unchecked_tables":"","example":"","note":"","seconds":0.0,"benchmark_seconds":0.0});
    for table in compare::TABLES {
        row[format!("{table}_diff")] = json!(0);
    }
    row
}

fn seconds(start: Instant) -> f64 {
    (start.elapsed().as_secs_f64() * 1000.).round_ties_even() / 1000.
}

pub(super) fn run(repo: &Path, name: &str, edits: Option<&[String]>) -> Result<Vec<Value>> {
    let kinds = KINDS
        .into_iter()
        .filter(|kind| edits.is_none_or(|list| list.iter().any(|s| s == kind)));
    let start = Instant::now();
    let temp = tempfile::Builder::new().prefix("crg-fidelity-").tempdir()?;
    let scratch = Scratch::create(repo, &temp.path().canonicalize()?);
    if let Err(e) = &scratch {
        eprintln!("ERROR: incremental_fidelity setup failed: {e}");
    }
    let mut result = Vec::new();
    for kind in kinds {
        let mut row = blank(name, kind);
        let error = match &scratch {
            Ok(s) => {
                let start = Instant::now();
                let run = s.scenario(kind, &mut row);
                row["seconds"] = json!(seconds(start));
                if let Err(e) = s.reset() {
                    eprintln!("WARNING: could not reset the benchmark tree: {e}");
                }
                run.err().map(|e| {
                    eprintln!("WARNING: incremental_fidelity {kind} failed: {e}");
                    e.to_string()
                })
            }
            Err(e) => Some(e.to_string()),
        };
        if let Some(e) = error {
            row["status"] = json!("error");
            row["note"] = json!(
                format!("RuntimeError: {e}")
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        result.push(row);
    }
    let elapsed = seconds(start);
    for row in &mut result {
        row["benchmark_seconds"] = json!(elapsed);
    }
    Ok(result)
}
