use super::config::{Config, Embeddings};
use crate::cli::{query, search};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};

pub(super) fn run(
    db: &Connection,
    path: &Path,
    cfg: &Config,
    options: &Embeddings,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for task in &cfg.multi_hop_tasks {
        let params = search::Options {
            query: task.nl_query.clone(),
            provider: options.embed_provider.clone().filter(|_| options.embed),
            model: options.embed_model.clone().filter(|_| options.embed),
            ..Default::default()
        };
        let suffix = task.anchor_qualified_suffix.to_lowercase();
        let hits = match search::ranked(db, path, &params, task.k.unwrap_or(10)) {
            Ok((_, rows)) => rows,
            Err(err) => {
                eprintln!("WARNING: hybrid_search failed on {}: {err}", task.id);
                vec![]
            }
        };
        let hit = hits.iter().enumerate().find_map(|(rank, row)| {
            let name = row["qualified_name"].as_str()?;
            name.to_lowercase()
                .ends_with(&suffix)
                .then_some((rank, name))
        });
        let mut neighbors = Vec::new();
        if let Some((_, name)) = hit {
            let pattern = task.traversal_pattern.as_deref().unwrap_or("callers_of");
            let q = query::Query {
                pattern: pattern.into(),
                target: name.into(),
                detail_level: Some("standard".into()),
                max_results: None,
                resolution: None,
                to: None,
            };
            match query::run(db, path, &q) {
                Ok(value) => {
                    let rows = value["data"]
                        .as_array()
                        .filter(|rows| !rows.is_empty())
                        .or_else(|| value["results"].as_array());
                    if let Some(rows) = rows {
                        neighbors.extend(
                            rows.iter()
                                .map(|row| row["name"].as_str().unwrap_or("").to_lowercase()),
                        );
                    }
                }
                Err(err) => eprintln!(
                    "WARNING: query_graph({pattern}) failed on {}: {err}",
                    task.id
                ),
            }
        }
        let names: HashSet<&str> = neighbors
            .iter()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .collect();
        let matched = task
            .expected_neighbor_names
            .iter()
            .filter(|s| names.contains(s.to_lowercase().as_str()))
            .count();
        let expected = task.expected_neighbor_names.len();
        let recall = if expected == 0 {
            0.0
        } else {
            (1000.0 * matched as f64 / expected as f64).round_ties_even() / 1000.0
        };
        rows.push(json!({"repo":cfg.name,"task_id":task.id,"nl_query":task.nl_query,
            "anchor_found":hit.is_some(),"anchor_rank":hit.map_or(-1,|(rank,_)|rank as i64),"neighbor_count":neighbors.len(),
            "expected_count":expected,"matched_count":matched,"neighbor_recall":recall,"score":recall}));
    }
    Ok(rows)
}
