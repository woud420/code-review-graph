use super::{Edge, Node, patterns::COMMON_WORDS};
use anyhow::Result;
use rusqlite::{Transaction, params};
use std::collections::{HashMap, HashSet};

struct Group {
    key: String,
    members: Vec<usize>,
    name: String,
    keywords: Vec<String>,
    language: String,
}

fn ranked<I: IntoIterator<Item = String>>(words: I) -> Vec<(String, usize)> {
    let mut counts = HashMap::new();
    for (i, word) in words.into_iter().enumerate() {
        let entry = counts.entry(word).or_insert((0usize, i));
        entry.0 += 1;
    }
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by_key(|(_, (n, order))| (std::cmp::Reverse(*n), *order));
    rows.into_iter()
        .map(|(word, (count, _))| (word, count))
        .collect()
}

fn words(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut lower = false;
    for c in name.chars() {
        if c == '_' || c == '-' || c == '.' || c.is_whitespace() {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
        } else {
            if lower && c.is_ascii_uppercase() && !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            current.push(c);
        }
        lower = c.is_ascii_lowercase();
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn slug(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect();
    let mut name = words(&cleaned).join("-").to_lowercase();
    if name.len() > 30 {
        let end = name[..=30].rfind('-').filter(|&v| v > 0).unwrap_or(30);
        name.truncate(end);
    }
    name
}

impl Group {
    fn new(key: String, members: Vec<usize>, nodes: &[Node]) -> Self {
        let prod: Vec<_> = members
            .iter()
            .copied()
            .filter(|&i| !nodes[i].test && nodes[i].kind != "Test")
            .collect();
        let naming = if prod.is_empty() { &members } else { &prod };
        let prefix = ranked(naming.iter().map(|&i| {
            let file = &nodes[i].file;
            file.rsplit_once('/')
                .map(|(p, _)| p.rsplit('/').next().unwrap_or(p))
                .unwrap_or_else(|| file.rsplit_once('.').map_or(file.as_str(), |(s, _)| s))
                .to_owned()
        }))
        .first()
        .map(|(prefix, _)| slug(prefix))
        .unwrap_or_default();
        let class = ranked(
            naming
                .iter()
                .filter(|&&i| nodes[i].kind == "Class")
                .map(|&i| nodes[i].name.clone()),
        )
        .into_iter()
        .next()
        .filter(|(_, count)| *count as f64 > 0.4 * naming.len() as f64)
        .map(|(name, _)| slug(&name));
        let keywords: Vec<String> = ranked(
            naming
                .iter()
                .filter(|&&i| {
                    matches!(
                        nodes[i].kind.as_str(),
                        "Function" | "Class" | "Test" | "Type"
                    )
                })
                .flat_map(|&i| words(&nodes[i].name))
                .map(|word| word.to_lowercase())
                .filter(|word| {
                    word.chars().count() > 1 && COMMON_WORDS.binary_search(&word.as_str()).is_err()
                }),
        )
        .into_iter()
        .take(5)
        .map(|(word, _)| word)
        .collect();
        let suffix = class
            .or_else(|| keywords.first().cloned())
            .unwrap_or_default();
        let name = match (prefix.is_empty(), suffix.is_empty()) {
            (true, true) => "cluster".to_owned(),
            (true, false) => suffix,
            (false, true) => prefix,
            (false, false) => format!("{prefix}-{suffix}"),
        };
        let language = ranked(
            members
                .iter()
                .map(|&i| nodes[i].language.clone())
                .filter(|s| !s.is_empty()),
        )
        .into_iter()
        .next()
        .map(|(s, _)| s)
        .unwrap_or_default();
        Self {
            key,
            members,
            name,
            keywords,
            language,
        }
    }
}

fn groups(nodes: &[Node]) -> Vec<Group> {
    let entities: Vec<_> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind != "File")
        .collect();
    let paths: Vec<Vec<&str>> = entities
        .iter()
        .map(|(_, n)| n.file.split('/').filter(|s| !s.is_empty()).collect())
        .collect();
    let shortest = paths
        .iter()
        .map(|p| p.len().saturating_sub(1))
        .min()
        .unwrap_or(0);
    let common = (0..shortest)
        .take_while(|&i| paths.iter().all(|p| p[i] == paths[0][i]))
        .count();
    let max = paths
        .iter()
        .map(|p| p.len().saturating_sub(common + 1))
        .max()
        .unwrap_or(0);
    let mut assigned = Vec::new();
    for depth in 1..=max.max(1) {
        let mut map: HashMap<String, usize> = HashMap::new();
        assigned.clear();
        for ((i, _), parts) in entities.iter().zip(&paths) {
            let directories = &parts[common..parts.len() - 1];
            let key = if directories.is_empty() {
                let name = parts.last().unwrap();
                name.rsplit_once('.').map_or(*name, |(s, _)| s).to_owned()
            } else {
                directories[..depth.min(directories.len())].join("/")
            };
            let group = *map.entry(key.clone()).or_insert_with(|| {
                assigned.push((key, Vec::new()));
                assigned.len() - 1
            });
            assigned[group].1.push(*i);
        }
        if assigned
            .iter()
            .filter(|(_, group)| group.len() >= 2)
            .count()
            >= 10
        {
            break;
        }
    }
    assigned
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(key, m)| Group::new(key, m, nodes))
        .collect()
}

fn dedupe(groups: &mut [Group]) {
    let mut taken: HashSet<String> = groups.iter().map(|g| g.name.clone()).collect();
    let mut names = Vec::<(String, Vec<usize>)>::new();
    let mut positions = HashMap::new();
    for (i, g) in groups.iter().enumerate() {
        let pos = *positions.entry(g.name.clone()).or_insert_with(|| {
            names.push((g.name.clone(), Vec::new()));
            names.len() - 1
        });
        names[pos].1.push(i);
    }
    for (name, mut ids) in names {
        ids.sort_by_key(|&i| (std::cmp::Reverse(groups[i].members.len()), i));
        for i in ids.into_iter().skip(1) {
            let candidate = groups[i]
                .keywords
                .iter()
                .map(|s| slug(s))
                .filter(|s| !s.is_empty() && !name.split('-').any(|w| w == s))
                .map(|s| format!("{name}-{s}"))
                .find(|n| !taken.contains(n))
                .unwrap_or_else(|| {
                    (2..)
                        .map(|n| format!("{name}-{n}"))
                        .find(|n| !taken.contains(n))
                        .unwrap()
                });
            taken.insert(candidate.clone());
            groups[i].name = candidate;
        }
    }
}

pub(super) fn store(tx: &Transaction<'_>, nodes: &[Node], edges: &[Edge]) -> Result<usize> {
    let mut groups = groups(nodes);
    dedupe(&mut groups);
    let membership: HashMap<_, _> = groups
        .iter()
        .enumerate()
        .flat_map(|(g, group)| {
            group
                .members
                .iter()
                .map(move |&n| (nodes[n].qn.as_str(), g))
        })
        .collect();
    let mut internal = vec![0usize; groups.len()];
    let mut external = internal.clone();
    for e in edges {
        let from = membership.get(e.src.as_str()).copied();
        let to = membership.get(e.dst.as_str()).copied();
        if from == to {
            if let Some(g) = from {
                internal[g] += 1;
            }
        } else {
            if let Some(g) = from {
                external[g] += 1;
            }
            if let Some(g) = to {
                external[g] += 1;
            }
        }
    }
    tx.execute_batch("DELETE FROM communities; UPDATE nodes SET community_id=NULL WHERE community_id IS NOT NULL; DELETE FROM metadata WHERE key='community_detector'")?;
    let mut insert=tx.prepare("INSERT INTO communities(name,level,cohesion,size,dominant_language,description) VALUES(?,0,?,?,?,?)")?;
    let mut member = tx.prepare("UPDATE nodes SET community_id=? WHERE id=?")?;
    for (i, group) in groups.iter().enumerate() {
        let total = internal[i] + external[i];
        let score = if total > 0 {
            super::round_score(internal[i] as f64 / total as f64)
        } else {
            0.0
        };
        insert.execute(params![
            group.name,
            score,
            group.members.len() as i64,
            group.language,
            format!("Directory-based community: {}", group.key)
        ])?;
        let id = tx.last_insert_rowid();
        for &index in &group.members {
            member.execute(params![id, nodes[index].id])?;
        }
    }
    if !groups.is_empty() {
        tx.execute(
            "INSERT INTO metadata VALUES('community_detector','file')",
            [],
        )?;
    }
    Ok(groups.len())
}
