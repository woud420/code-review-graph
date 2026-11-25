use crate::cli::{query, store::Counts};
use anyhow::Result;
use code_review_graph_rs::canonical::postprocess;
use rusqlite::{Connection, params_from_iter, types::Value as Sql};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};

fn search(conn: &Connection, term: &str) -> Result<()> {
    let words: Vec<_> = term.split_whitespace().collect();
    if words.is_empty() {
        return Ok(());
    }
    let phrase = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let expr = if words.len() == 1 {
        phrase(term)
    } else {
        words
            .iter()
            .map(|w| phrase(w))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let sql = "SELECT n.* FROM nodes_fts f JOIN nodes n ON f.rowid=n.id
               WHERE nodes_fts MATCH ? AND n.kind!='Output' LIMIT 20";
    let indexed = (|| -> rusqlite::Result<Vec<Value>> {
        conn.prepare(sql)?.query_map([expr], query::node)?.collect()
    })();
    if indexed.is_ok_and(|rows| !rows.is_empty()) {
        return Ok(());
    }
    let mut values = Vec::new();
    let mut terms = Vec::new();
    for word in words {
        let pattern = Sql::Text(format!("%{}%", word.to_lowercase()));
        values.extend([pattern.clone(), pattern]);
        terms.push("(lower(name) LIKE ? OR lower(qualified_name) LIKE ?)");
    }
    let sql = format!(
        "SELECT * FROM nodes WHERE kind!='Output' AND {} LIMIT 20",
        terms.join(" AND ")
    );
    conn.prepare(&sql)?
        .query_map(params_from_iter(values), query::node)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(())
}

pub(super) fn run(
    _repo: &Path,
    db: &Connection,
    name: &str,
    queries: &[&str],
) -> Result<Vec<Value>> {
    let counts = Counts::read(db)?;
    let measure = |label: &str, stage: fn(&rusqlite::Transaction<'_>) -> Result<usize>| -> f64 {
        let start = Instant::now();
        let result = (|| -> Result<()> {
            let tx = db.unchecked_transaction()?;
            stage(&tx)?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("WARNING: {label} detection failed: {e}");
            0.
        } else {
            start.elapsed().as_secs_f64()
        }
    };
    let flow = measure("Flow", postprocess::refresh_flows);
    let community = measure("Community", postprocess::refresh_communities);
    let mut samples = Vec::new();
    for query in queries.iter().take(10) {
        let begin = Instant::now();
        search(db, query)?;
        samples.push(begin.elapsed().as_secs_f64());
    }
    let avg_ms = samples.iter().sum::<f64>() * 1000. / samples.len().max(1) as f64;
    Ok(vec![json!({
        "repo":name,"file_count":counts.files(),"node_count":counts.nodes(),"edge_count":counts.edges(),
        "flow_detection_seconds":(flow*1000.).round_ties_even()/1000.,
        "community_detection_seconds":(community*1000.).round_ties_even()/1000.,
        "search_avg_ms":(avg_ms*10.).round_ties_even()/10.,
        "nodes_per_second":(counts.nodes() as f64/flow.max(0.001)).round_ties_even() as u64
    })])
}
