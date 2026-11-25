use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const BENCHMARKS: [(&str, &str); 8] = [
    ("token_efficiency", "Token Efficiency"),
    ("impact_accuracy", "Impact Accuracy"),
    ("agent_baseline", "Agent Baseline"),
    ("flow_completeness", "Flow Completeness"),
    ("search_quality", "Search Quality"),
    ("build_performance", "Build Performance"),
    ("multi_hop_retrieval", "Multi Hop Retrieval"),
    ("incremental_fidelity", "Incremental Fidelity"),
];
const METHODOLOGY: &str = "# Evaluation Report\n\n## Methodology\n\nBenchmarks are run against real open-source repositories.\nToken counts use a consistent `len(text) // 4` approximation.\nImpact accuracy reports two ground-truth modes: graph-derived (circular — upper bound) and co-change (files co-changed in the same commit, seed excluded).\nRows with `status=error` are kept for forensics but excluded from all aggregates.\nIncremental fidelity compares an incrementally updated graph against a clean rebuild of the same tree; `status=known_failure` marks a divergence that is already recorded in `incremental_fidelity.KNOWN_FAILURES`.\n";
type Row = BTreeMap<String, Option<String>>;

#[derive(Default)]
struct Results {
    headers: Vec<String>,
    rows: Vec<Row>,
}

fn newest_files(results: &Path, benchmark: &str) -> Result<Vec<PathBuf>> {
    let mut by_repo = BTreeMap::<String, (String, PathBuf)>::new();
    if !results.is_dir() {
        return Ok(Vec::new());
    }
    let marker = format!("_{benchmark}_");
    for entry in fs::read_dir(results)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "csv") {
            continue;
        }
        let Some((repo, stamp)) = path
            .file_stem()
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit_once(&marker))
        else {
            continue;
        };
        if by_repo
            .get(repo)
            .is_none_or(|(prev, _)| prev.as_str() < stamp)
        {
            by_repo.insert(repo.into(), (stamp.into(), path));
        }
    }
    Ok(by_repo.into_values().map(|(_, p)| p).collect())
}

impl Results {
    fn read(directory: &Path, benchmark: &str) -> Result<Self> {
        let mut out = Self::default();
        for path in newest_files(directory, benchmark)? {
            let mut csv = csv::ReaderBuilder::new().flexible(true).from_path(&path)?;
            let columns = csv.headers()?.clone();
            for record in csv.records() {
                let record = record.with_context(|| format!("reading {}", path.display()))?;
                if out.rows.is_empty() {
                    if record.len() > columns.len() {
                        bail!(
                            "{}: first record has more fields than its header",
                            path.display()
                        );
                    }
                    for field in &columns {
                        if !out.headers.iter().any(|s| s == field) {
                            out.headers.push(field.into());
                        }
                    }
                }
                let fields = columns
                    .iter()
                    .enumerate()
                    .map(|(i, key)| (key.into(), record.get(i).map(str::to_owned)))
                    .collect();
                out.rows.push(fields);
            }
        }
        Ok(out)
    }
}

fn cell<'a>(row: &'a Row, key: &str, default: &'a str) -> Result<&'a str> {
    match row.get(key) {
        None => Ok(default),
        Some(Some(s)) => Ok(s),
        Some(None) => bail!("missing CSV field for column {key}"),
    }
}

fn table(headers: &[&str], rows: &[Vec<&str>]) -> String {
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(format!("| {} |", headers.join(" | ")));
    lines.push(format!("| {} |", vec!["---"; headers.len()].join(" | ")));
    lines.extend(rows.iter().map(|r| format!("| {} |", r.join(" | "))));
    lines.join("\n")
}

struct Report {
    data: BTreeMap<&'static str, Results>,
}
impl Report {
    fn read(dir: &Path) -> Result<Self> {
        let mut data = BTreeMap::new();
        for (id, _) in BENCHMARKS {
            data.insert(id, Results::read(dir, id)?);
        }
        Ok(Self { data })
    }

    fn full(&self) -> Result<String> {
        let mut blocks = vec![METHODOLOGY.to_owned()];
        for (id, title) in BENCHMARKS {
            let set = &self.data[id];
            if set.rows.is_empty() {
                continue;
            }
            let header: Vec<_> = set.headers.iter().map(String::as_str).collect();
            let rows = set
                .rows
                .iter()
                .map(|row| header.iter().map(|h| cell(row, h, "-")).collect())
                .collect::<Result<Vec<_>>>()?;
            blocks.push(format!("## {title}\n\n{}\n", table(&header, &rows)));
        }
        Ok(blocks.join("\n"))
    }

    fn section(&self, id: &str, title: &str, fields: &[(&str, &str)]) -> Result<Option<String>> {
        let source = &self.data[id];
        if source.rows.is_empty() {
            return Ok(None);
        }
        let headings = fields.iter().map(|(_, h)| *h).collect::<Vec<_>>();
        let mut rows = Vec::new();
        for r in &source.rows {
            let mut cells = Vec::new();
            for &(key, _) in fields {
                let text = if id == "agent_baseline" && key == "status" {
                    r.get(key)
                        .and_then(Option::as_deref)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("ok")
                } else if id == "incremental_fidelity" && key == "worst_table" {
                    r.get(key)
                        .and_then(Option::as_deref)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("-")
                } else {
                    cell(r, key, "-")?
                };
                cells.push(text);
            }
            rows.push(cells);
        }
        Ok(Some(format!(
            "### {title}\n\n{}\n",
            table(&headings, &rows)
        )))
    }

    fn accuracy(&self) -> Result<Option<String>> {
        let (impact, flow, search) = (
            &self.data["impact_accuracy"],
            &self.data["flow_completeness"],
            &self.data["search_quality"],
        );
        if impact.rows.is_empty() && flow.rows.is_empty() && search.rows.is_empty() {
            return Ok(None);
        }
        let mut repos = BTreeMap::<&str, Quality>::new();
        for row in &impact.rows {
            if row
                .get("status")
                .is_some_and(|s| !matches!(s.as_deref(), Some("" | "ok")))
            {
                continue;
            }
            let mode = row
                .get("ground_truth_mode")
                .and_then(Option::as_deref)
                .unwrap_or("");
            if !mode.is_empty() && !mode.starts_with("graph-derived") {
                continue;
            }
            let q = repos.entry(cell(row, "repo", "?")?).or_default();
            if let Some(n) = metric(row, "f1", "") {
                q.f1.push(n);
            }
        }
        for row in &flow.rows {
            repos.entry(cell(row, "repo", "?")?).or_default().recall =
                Some(cell(row, "recall", "-")?);
        }
        for row in &search.rows {
            let q = repos.entry(cell(row, "repo", "?")?).or_default();
            if let Some(n) = metric(row, "reciprocal_rank", "0") {
                q.ranks.push(n);
            }
        }
        let values = repos
            .into_iter()
            .map(|(repo, q)| {
                [
                    repo.to_string(),
                    average(&q.f1),
                    q.recall.unwrap_or("-").to_string(),
                    average(&q.ranks),
                ]
            })
            .collect::<Vec<_>>();
        let rows: Vec<Vec<_>> = values
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        Ok(Some(format!(
            "### Accuracy & Quality\n\n{}\n",
            table(
                &[
                    "Repo",
                    "Impact F1 (graph-derived)",
                    "Flow Recall",
                    "Search MRR"
                ],
                &rows
            )
        )))
    }

    fn readme(&self) -> Result<String> {
        let sections = [
            self.section(
                "token_efficiency",
                "Token Efficiency",
                &[
                    ("repo", "Repo"),
                    ("changed_files", "Files"),
                    ("naive_tokens", "Naive Tokens"),
                    ("standard_tokens", "Standard Tokens"),
                    ("graph_tokens", "Graph Tokens"),
                    ("naive_to_graph_ratio", "Naive/Graph"),
                    ("standard_to_graph_ratio", "Std/Graph"),
                ],
            )?,
            self.accuracy()?,
            self.section(
                "agent_baseline",
                "Agent Baseline (grep top-k vs graph query)",
                &[
                    ("repo", "Repo"),
                    ("question", "Question"),
                    ("baseline_tokens", "Baseline Tokens"),
                    ("graph_tokens", "Graph Tokens"),
                    ("baseline_to_graph_ratio", "Baseline/Graph"),
                    ("status", "Status"),
                ],
            )?,
            self.section(
                "incremental_fidelity",
                "Incremental Fidelity (update vs clean rebuild)",
                &[
                    ("repo", "Repo"),
                    ("edit_kind", "Edit"),
                    ("status", "Status"),
                    ("total_differing_rows", "Differing Rows"),
                    ("worst_table", "Worst Table"),
                    ("seconds", "Seconds"),
                ],
            )?,
            self.section(
                "build_performance",
                "Performance",
                &[
                    ("repo", "Repo"),
                    ("file_count", "Files"),
                    ("node_count", "Nodes"),
                    ("flow_detection_seconds", "Flow Det. (s)"),
                    ("search_avg_ms", "Search (ms)"),
                ],
            )?,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        Ok(if sections.is_empty() {
            "No benchmark results found.\n".into()
        } else {
            sections.join("\n")
        })
    }
}

#[derive(Default)]
struct Quality<'a> {
    f1: Vec<f64>,
    recall: Option<&'a str>,
    ranks: Vec<f64>,
}

fn metric(row: &Row, name: &str, default: &str) -> Option<f64> {
    row.get(name)
        .map_or(Some(default), Option::as_deref)?
        .trim()
        .parse()
        .ok()
}

fn average(values: &[f64]) -> String {
    if values.is_empty() {
        return "-".into();
    }
    let value = values.iter().sum::<f64>() / values.len() as f64;
    if value.is_nan() {
        return "nan".into();
    }
    if !value.is_finite() {
        return value.to_string();
    }
    let rounded = format!("{value:.3}");
    let trimmed = rounded.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.into()
    }
}

pub(in crate::cli) fn report(results: &Path) -> Result<()> {
    let data = Report::read(results)?;
    let content = data.full()?;
    let report = Path::new("evaluate/reports/summary.md");
    fs::create_dir_all(report.parent().unwrap())?;
    fs::write(report, content)?;
    println!("Report written to {}", report.display());
    let tables = data.readme()?;
    println!("\n--- README Tables (copy-paste) ---\n");
    println!("{tables}");
    Ok(())
}
