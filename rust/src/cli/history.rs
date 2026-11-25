use anyhow::Result;
use chrono::{DateTime, Days, NaiveDateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

const TIMESTAMP: &str = "%Y-%m-%dT%H:%M:%SZ";

thread_local! { static LABEL: Cell<Option<&'static str>> = const { Cell::new(None) }; }

pub(super) fn with_label<T>(label: Option<&'static str>, report: impl FnOnce() -> T) -> T {
    let previous = LABEL.replace(label);
    let result = report();
    LABEL.set(previous);
    result
}

fn enabled(variable: &str) -> bool {
    std::env::var(variable).is_ok_and(|s| {
        matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn now() -> DateTime<Utc> {
    DateTime::from(SystemTime::now())
}

fn options() -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.create(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

fn append(dir: &Path, entry: Value) -> io::Result<()> {
    let path = dir.join("context-history.jsonl");
    let lock = options().open(dir.join("context-history.lock"))?;
    lock.lock()?;
    if path.metadata().is_ok_and(|m| m.len() >= 1024 * 1024) {
        fs::rename(&path, dir.join("context-history.jsonl.1"))?;
    }
    let mut line = serde_json::to_vec(&entry)?;
    line.push(b'\n');
    options().append(true).open(path)?.write_all(&line)
}

pub(super) fn record(
    conn: &rusqlite::Connection,
    tool: &str,
    mode: &str,
    scope: Option<(usize, i64)>,
    baseline: usize,
    returned: usize,
) {
    if !enabled("CRG_CONTEXT_HISTORY") || enabled("CRG_READ_ONLY") {
        return;
    }
    let Some(dir) = conn
        .path()
        .and_then(|p| Path::new(p).parent())
        .filter(|d| d.is_dir())
    else {
        return;
    };
    let mut entry = json!({"v":1,"ts":now().format(TIMESTAMP).to_string(),"tool":LABEL.get().unwrap_or(tool),
        "baseline_tokens":baseline,"returned_tokens":returned,"saved_tokens":baseline.saturating_sub(returned),
        "detail_level":if mode == "minimal" { "minimal" } else { "standard" }});
    if let Some((files, depth)) = scope {
        entry["files"] = json!(files);
        entry["depth"] = json!(depth);
    }
    if let Err(error) = append(dir, entry) {
        eprintln!(
            "WARNING: Could not write context history to {}: {error}",
            dir.join("context-history.jsonl").display()
        );
    }
}

#[derive(Default, Serialize)]
struct Row {
    key: String,
    calls: u64,
    baseline_tokens: u64,
    returned_tokens: u64,
    saved_tokens: i128,
    largest_returned: u64,
}

impl Row {
    fn new(key: String) -> Self {
        Self {
            key,
            ..Default::default()
        }
    }

    fn label_width(&self) -> usize {
        self.key.chars().count()
    }

    fn add(&mut self, baseline: u64, returned: u64) {
        self.calls += 1;
        self.baseline_tokens += baseline;
        self.returned_tokens += returned;
        self.saved_tokens = i128::from(self.baseline_tokens) - i128::from(self.returned_tokens);
        self.largest_returned = self.largest_returned.max(returned);
    }

    fn print(&self, width: usize) {
        let percent = if self.baseline_tokens > 0 {
            (self.saved_tokens as f64 * 100.0 / self.baseline_tokens as f64).round_ties_even()
                as i128
        } else {
            0
        };
        let saved = format!("{} ({percent}%)", grouped(self.saved_tokens));
        println!(
            "{:<width$}  {:>6}  {:>13}  {:>13}  {:>17}  {:>13}",
            self.key,
            grouped(self.calls),
            grouped(self.baseline_tokens),
            grouped(self.returned_tokens),
            saved,
            grouped(self.largest_returned)
        );
    }
}

fn grouped(number: impl Into<i128>) -> String {
    let n = number.into();
    let digits = n.unsigned_abs().to_string();
    let mut output = String::from(if n < 0 { "-" } else { "" });
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            output.push(',');
        }
        output.push(ch);
    }
    output
}

struct Entry {
    ts: String,
    tool: String,
    baseline: u64,
    returned: u64,
}

impl Entry {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_str(&String::from_utf8_lossy(bytes)).ok()?;
        if value["v"] != 1 {
            return None;
        }
        let ts = value["ts"].as_str()?;
        NaiveDateTime::parse_from_str(ts, TIMESTAMP).ok()?;
        value["saved_tokens"].as_u64()?;
        Some(Self {
            ts: ts.to_owned(),
            tool: value["tool"].as_str()?.to_owned(),
            baseline: value["baseline_tokens"].as_u64()?,
            returned: value["returned_tokens"].as_u64()?,
        })
    }
}

pub(super) fn print(path: PathBuf, by: &str, days: Option<u32>, as_json: bool) -> Result<()> {
    let cutoff = days
        .and_then(|d| {
            now()
                .date_naive()
                .checked_sub_days(Days::new(u64::from(d) - 1))
        })
        .map(|d| format!("{d}T00:00:00Z"));
    let mut groups = BTreeMap::<String, Row>::new();
    let mut total = Row::new("Total".to_owned());
    let mut skipped = 0;
    for file in [path.with_extension("jsonl.1"), path.clone()] {
        let input = match File::open(&file) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                eprintln!(
                    "WARNING: Could not read context history from {}: {e}",
                    file.display()
                );
                continue;
            }
        };
        for line in BufReader::new(input).split(b'\n') {
            let line = line?;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let Some(entry) = Entry::parse(&line) else {
                skipped += 1;
                continue;
            };
            if cutoff.as_ref().is_some_and(|s| entry.ts < *s) {
                continue;
            }
            let key = if by == "day" {
                entry.ts.chars().take(10).collect()
            } else {
                entry.tool
            };
            groups
                .entry(key.clone())
                .or_insert_with(|| Row::new(key))
                .add(entry.baseline, entry.returned);
            total.add(entry.baseline, entry.returned);
        }
    }
    let mut rows: Vec<_> = groups.into_values().collect();
    if by == "tool" {
        rows.sort_by_key(|r| std::cmp::Reverse(r.calls));
    }
    let recording = enabled("CRG_CONTEXT_HISTORY");
    if as_json {
        println!(
            "{}",
            super::wire::json_text(
                &json!({"log":path,"recording":recording,"by":by,"days":days,"rows":rows,"total":total,"skipped_lines":skipped}),
                true
            )
        );
        return Ok(());
    }
    let window = days.map_or_else(|| "whole log".to_owned(), |d| format!("last {d} day(s)"));
    if rows.is_empty() {
        if recording {
            println!("No context history ({window}) in {}.", path.display());
        } else {
            println!("No context history ({window}). Recording is off.");
            println!(
                "Set CRG_CONTEXT_HISTORY=1 where the MCP server or the CLI runs, and each call that reports context savings adds one line to {}.",
                path.display()
            );
            println!("The log stays on this machine and holds only token counts and tool names.");
        }
    } else {
        println!(
            "Context history ({window}): {} call(s). Token counts are estimates (about 4 characters per token).",
            grouped(total.calls)
        );
        println!("Log: {}\n", path.display());
        let heading = if by == "day" { "Day (UTC)" } else { "Tool" };
        let width = rows
            .iter()
            .map(Row::label_width)
            .max()
            .unwrap_or(0)
            .max(heading.len())
            .max(5);
        println!(
            "{heading:<width$}  {:>6}  {:>13}  {:>13}  {:>17}  {:>13}",
            "Calls", "Full context", "Graph context", "Saved", "Largest reply"
        );
        for row in rows.iter().chain([&total]) {
            row.print(width);
        }
        if rows.iter().any(|r| r.saved_tokens < 0) {
            println!(
                "\nA negative Saved means the graph replies were bigger than the files they stand for."
            );
        }
    }
    if skipped > 0 {
        if !rows.is_empty() {
            println!();
        }
        println!("Skipped {skipped} line(s) that could not be read.");
    }
    if !rows.is_empty() && !recording {
        println!("\nRecording is off. Set CRG_CONTEXT_HISTORY=1 to add new calls.");
    }
    Ok(())
}
