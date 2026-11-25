use super::{
    agent_baseline,
    config::{Config, Embeddings},
    multi_hop,
};
use crate::cli::{build, search, store};
use anyhow::{Context, Result, ensure};
use code_review_graph_rs::canonical::{index, postprocess::Level};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(clap::Args)]
pub(in crate::cli) struct Options {
    #[arg(long)]
    report: bool,
    #[arg(long, default_value = "evaluate/results")]
    output_dir: PathBuf,
    #[arg(long, value_delimiter = ',')]
    repo: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    benchmark: Vec<String>,
    #[arg(long)]
    all: bool,
    #[command(flatten)]
    embeddings: Embeddings,
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(cwd).args(args).output()?;
    ensure!(
        out.status.success(),
        "git {} failed in {}: {}",
        args[0],
        cwd.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().into())
}

fn checkout(base: &Path, cfg: &Config) -> Result<PathBuf> {
    fs::create_dir_all(base)?;
    let path = base.join(&cfg.name);
    if path.exists() {
        ensure!(!path.is_symlink(), "{} is a symbolic link", path.display());
        let expected = path.canonicalize()?;
        let actual = git(&path, &["rev-parse", "--show-toplevel"])
            .ok()
            .and_then(|s| Path::new(&s).canonicalize().ok());
        ensure!(
            actual.as_ref() == Some(&expected),
            "{} exists but is not a standalone git repository. Refusing to fetch or check out there.",
            path.display()
        );
        git(&path, &["fetch", "--all", "--tags"])?;
    } else {
        git(base, &["clone", "--", &cfg.url, &cfg.name])?;
    }
    if cfg.commit != "HEAD" {
        git(&path, &["checkout", &cfg.commit])?;
    }
    Ok(path.canonicalize()?)
}

fn flow_completeness(db: &Connection, cfg: &Config) -> Result<Vec<Value>> {
    let rows = db
        .prepare(
            "SELECT n.qualified_name,f.depth FROM flows f JOIN nodes n ON n.id=f.entry_point_id",
        )?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let known: BTreeSet<_> = cfg.entry_points.iter().collect();
    let found = known
        .iter()
        .filter(|entry| rows.iter().any(|(name, _)| name.contains(entry.as_str())))
        .count();
    let recall = super::round(found as f64 / known.len().max(1) as f64, 3);
    let avg = super::round(
        rows.iter().map(|v| v.1).sum::<i64>() as f64 / rows.len().max(1) as f64,
        1,
    );
    Ok(vec![
        json!({"repo":cfg.name,"known_entry_points":known.len(),"detected_entry_points":found,"recall":recall,"detected_flows":rows.len(),"avg_flow_depth":avg,"max_flow_depth":rows.iter().map(|v|v.1).max().unwrap_or(0)}),
    ])
}

fn search_quality(
    db: &Connection,
    path: &Path,
    cfg: &Config,
    options: &Embeddings,
) -> Result<Vec<Value>> {
    cfg.search_queries.iter().map(|sq| {
        let params=search::Options { query:sq.query.clone(), provider:options.embed_provider.clone().filter(|_|options.embed), model:options.embed_model.clone().filter(|_|options.embed), ..Default::default() };
        let (_,rows)=search::ranked(db,path,&params,20)?;
        let wanted=sq.expected.to_lowercase();
        let rank=rows.iter().position(|row| {
            let name=row["qualified_name"].as_str().unwrap_or("").to_lowercase();
            wanted.contains(&name) || name.contains(&wanted) || wanted.rsplit("::").next()==name.rsplit("::").next()
        }).map_or(0, |n| n+1);
        let reciprocal=if rank==0 {0.0} else {super::round(1. / rank as f64,3)};
        Ok(json!({"repo":cfg.name,"query":sq.query,"expected":sq.expected,"rank":rank,"reciprocal_rank":reciprocal}))
    }).collect()
}

fn csv(path: &Path, rows: &[Value]) -> Result<()> {
    let Some(first) = rows.first() else {
        return Ok(());
    };
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("missing output directory")?)?;
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(&mut file);
    let fields = first
        .as_object()
        .context("invalid result")?
        .keys()
        .collect::<Vec<_>>();
    writer.write_record(&fields)?;
    for row in rows {
        writer.write_record(fields.iter().map(|k| match &row[*k] {
            Value::String(s) => s.to_owned(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            Value::Null => String::new(),
            v => v.to_string(),
        }))?;
    }
    writer.flush()?;
    drop(writer);
    file.persist(path)?;
    Ok(())
}

impl Options {
    pub(in crate::cli) fn run(&self) -> Result<()> {
        if self.report {
            return super::report(&self.output_dir);
        }
        if !self.all && self.repo.is_empty() && self.benchmark.is_empty() {
            println!("Specify --all, --repo, or --benchmark. See --help.");
            return Ok(());
        }
        let names: Vec<_> = if self.benchmark.is_empty() {
            super::BENCHMARKS.iter().map(|a| a.0).collect()
        } else {
            self.benchmark.iter().map(|s| s.trim()).collect()
        };
        let mut configs: Vec<Config> =
            serde_json::from_str(include_str!("../../../../assets/eval/configs.json"))?;
        if !self.repo.is_empty() {
            let mut selected = Vec::new();
            for name in self.repo.iter().map(|n| n.trim()) {
                let path = PathBuf::from(format!("{name}.yaml"));
                if path.is_absolute() {
                    selected.push(serde_yaml_ng::from_slice(&fs::read(path)?)?);
                } else {
                    let position = configs
                        .iter()
                        .position(|c| c.name == name)
                        .with_context(|| format!("unknown evaluation repository: {name}"))?;
                    selected.push(configs.remove(position));
                }
            }
            configs = selected;
        }
        fs::create_dir_all(&self.output_dir)?;
        let mut completed = BTreeSet::new();
        for cfg in &configs {
            ensure!(
                !cfg.name.is_empty()
                    && Path::new(&cfg.name).components().count() == 1
                    && Path::new(&cfg.name).file_name().is_some(),
                "invalid repository name in evaluation config"
            );
            ensure!(
                cfg.commit == "HEAD"
                    || (matches!(cfg.commit.len(), 40 | 64)
                        && cfg.commit.bytes().all(|c| c.is_ascii_hexdigit())),
                "invalid evaluation commit pin"
            );
            if let Some(last) = cfg.test_commits.last() {
                ensure!(
                    cfg.commit == last.sha,
                    "commit pin must equal latest test_commit {}",
                    last.sha
                );
            }
            eprintln!("INFO: Evaluating {}...", cfg.name);
            let path = checkout(Path::new("evaluate/test_repos"), cfg)?;
            let database = store::database(&path, None);
            index(&path, &database, true, Level::Full, false)?;
            if self.embeddings.embed {
                let value = build::embed(
                    &path,
                    &database,
                    self.embeddings.embed_provider.as_deref(),
                    self.embeddings.embed_model.as_deref(),
                    false,
                )?;
                if value["status"] != "ok" {
                    eprintln!("ERROR:   {}", value["summary"]);
                }
            }
            let db = Connection::open(&database)?;
            let semantic: BTreeSet<_> = names
                .iter()
                .copied()
                .filter(|n| {
                    matches!(
                        *n,
                        "agent_baseline" | "search_quality" | "multi_hop_retrieval"
                    )
                })
                .collect();
            if !semantic.is_empty()
                && !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='embeddings')",
                    [],
                    |r| r.get::<_, bool>(0),
                )?
            {
                eprintln!(
                    "WARNING:   no vector index — {} will score natural-language questions against FTS5 alone and return zero hits. Re-run with --embed.",
                    semantic.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
            let date: String = db.query_row("SELECT date('now','localtime')", [], |r| r.get(0))?;
            for &name in &names {
                if !super::BENCHMARKS.iter().any(|(id, _)| *id == name) {
                    eprintln!("WARNING: Unknown benchmark: {name}");
                    continue;
                }
                eprintln!("INFO:   Running {name}...");
                let rows = match name {
                    "flow_completeness" => flow_completeness(&db, cfg),
                    "search_quality" => search_quality(&db, &path, cfg, &self.embeddings),
                    "build_performance" => super::performance::run(
                        &path,
                        &db,
                        &cfg.name,
                        &cfg.search_queries
                            .iter()
                            .map(|q| q.query.as_str())
                            .collect::<Vec<_>>(),
                    ),
                    "agent_baseline" => agent_baseline::run(&db, &path, cfg, &self.embeddings),
                    "multi_hop_retrieval" => multi_hop::run(&db, &path, cfg, &self.embeddings),
                    "incremental_fidelity" => {
                        super::fidelity::run(&path, &cfg.name, cfg.fidelity_edits.as_deref())
                    }
                    _ => super::changes::run(&db, &path, cfg, name),
                };
                let output = self
                    .output_dir
                    .join(format!("{}_{name}_{date}.csv", cfg.name));
                match rows.and_then(|rows| {
                    csv(&output, &rows)?;
                    Ok(rows.len())
                }) {
                    Ok(count) => eprintln!("INFO:   {name}: {count} result(s)"),
                    Err(e) => eprintln!("ERROR:   {name} failed: {e}"),
                }
                completed.insert((&cfg.name, name));
            }
        }
        println!(
            "\nCompleted {} benchmark(s).\nRun 'code-review-graph eval --report' to generate tables.",
            completed.len()
        );
        Ok(())
    }
}
