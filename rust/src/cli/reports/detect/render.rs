use super::*;

fn label(score: f64) -> &'static str {
    if score > 0.7 {
        "high"
    } else if score > 0.4 {
        "medium"
    } else {
        "low"
    }
}
fn name_list(names: impl Iterator<Item = String>) -> String {
    let mut seen = HashSet::new();
    let names: Vec<_> = names
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect();
    let list = names[..names.len().min(2)].join(", ");
    if names.len() > 2 {
        format!("{list} and {} more", names.len() - 2)
    } else {
        list
    }
}
fn grouped(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',')
        }
        out.push(c);
    }
    out
}
fn compact(n: usize) -> String {
    if n >= 10000 {
        format!("{}k", grouped(n / 1000))
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.)
    } else {
        n.to_string()
    }
}

pub(super) fn panel(result: &Value, baseline: usize) {
    if baseline == 0 {
        return;
    }
    let saved = result["context_savings"]["saved_tokens"]
        .as_u64()
        .unwrap_or_default() as usize;
    let percent = result["context_savings"]["saved_percent"]
        .as_u64()
        .unwrap_or_default();
    let returned = baseline.saturating_sub(saved);
    let mut lines = vec![
        format!("Full context would be:  {:>9} tokens", grouped(baseline)),
        format!("Graph context used:     {:>9} tokens", grouped(returned)),
        format!(
            "Saved:                  {:>9} tokens (~{percent}%)",
            grouped(saved)
        ),
    ];
    let mut terms = Vec::new();
    let mut total = 0;
    for (key, label) in [
        ("changed_functions", "Functions"),
        ("affected_flows", "Flows"),
        ("test_gaps", "Tests"),
        ("review_priorities", "Risk"),
    ] {
        if result[key].as_array().is_some_and(|xs| !xs.is_empty()) {
            let tokens = super::super::architecture::tokens(&result[key]);
            total += tokens;
            terms.push(format!("{label} {}", compact(tokens)));
        }
    }
    if !terms.is_empty() {
        if total < returned {
            terms.push(format!("Other {}", compact(returned - total)));
        }
        lines.push(format!("Breakdown: {}", terms.join(" · ")));
    }
    let width = 62.max(
        lines
            .iter()
            .map(|s| s.chars().count() + 2)
            .max()
            .unwrap_or_default(),
    );
    let left = (width - 15) / 2;
    println!(
        "┌{} Token Savings {}┐",
        "─".repeat(left),
        "─".repeat(width - 15 - left)
    );
    for line in &lines {
        println!("│ {line}{}│", " ".repeat(width - line.chars().count() - 1));
    }
    println!("└{}┘", "─".repeat(width));
}

pub(super) fn brief(conn: &Connection, root: &Path, out: &Value) -> Result<()> {
    let score = out["risk_score"].as_f64().unwrap_or_default();
    let line = format!("  - Overall risk score: {score:.2}");
    println!(
        "{}",
        text(out, "summary").replacen(&line, &format!("{line} of 1.00 ({})", label(score)), 1)
    );
    let priorities = out["review_priorities"]
        .as_array()
        .into_iter()
        .flatten()
        .take(3);
    for (i, n) in priorities.enumerate() {
        if i == 0 {
            println!("\nReview first (risk out of 1.00):");
        }
        let path = Path::new(text(n, "file_path"));
        let file = path.strip_prefix(root).unwrap_or(path);
        let tests = crate::cli::query::explicit_tests(conn, n)?;
        let test_qns = tests
            .iter()
            .map(|n| text(n, "qualified_name"))
            .collect::<HashSet<_>>();
        let mut stmt=conn.prepare("SELECT source_qualified FROM edges WHERE target_qualified=?1 AND kind='CALLS' ORDER BY id")?;
        let rows = stmt
            .query_map([text(n, "qualified_name")], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let callers = name_list(
            rows.into_iter()
                .filter(|c| !test_qns.contains(c.as_str()))
                .map(|c| clean(c.rsplit("::").next().unwrap_or(""))),
        );
        let tests = name_list(tests.into_iter().map(|n| text(&n, "name").into()));
        let callers = if callers.is_empty() {
            "no callers found".to_owned()
        } else {
            format!("called by {callers}")
        };
        let tests = if tests.is_empty() {
            "no tests found".to_owned()
        } else {
            format!("tested by {tests}")
        };
        let score = n["risk_score"].as_f64().unwrap_or_default();
        let line = n["line_start"]
            .as_i64()
            .map(|i| format!(":{i}"))
            .unwrap_or_default();
        println!(
            "  {}. {}  {}{line}  risk {score:.2} ({})  {callers}  {tests}",
            i + 1,
            text(n, "name"),
            file.display(),
            label(score)
        );
    }
    Ok(())
}
