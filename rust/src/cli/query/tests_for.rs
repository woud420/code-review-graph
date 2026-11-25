use super::*;
use std::collections::BTreeSet;

fn unresolved(edge: &Edge) -> bool {
    ["ambiguous_targets", "unresolved_targets"]
        .iter()
        .any(|k| edge.extra.get(*k).is_some())
}

fn result(
    rows: &mut Vec<Pair>,
    seen: &mut HashSet<String>,
    mut n: Value,
    indirect: bool,
    why: Option<&str>,
) {
    let key = text(&n, "qualified_name").to_owned();
    if seen.insert(key.clone()) {
        n["indirect"] = json!(indirect);
        if let Some(why) = why {
            n["inferred_by"] = json!(why);
        }
        rows.push(Pair {
            key,
            row: n,
            edge: None,
            unresolved: false,
        });
    }
}

pub(super) fn run(conn: &Connection, selected: &Value, root: &Path) -> Result<Vec<Pair>> {
    let name = text(selected, "name");
    let (mut rows, inputs, limit) = explicit(conn, selected)?;
    let mut seen = rows.iter().map(|p| p.key.clone()).collect::<HashSet<_>>();
    let mut frontier = inputs
        .iter()
        .filter(|s| s.contains("::"))
        .cloned()
        .collect::<BTreeSet<_>>();
    for _ in 0..2 {
        let mut next = BTreeSet::new();
        let mut edges = Vec::new();
        for qn in &frontier {
            let incoming = read_edges(conn, true, qn, "CALLS")?;
            if incoming.len() <= limit {
                edges.extend(incoming);
            }
        }
        edges.sort_by(|a, b| (&a.source, &a.target).cmp(&(&b.source, &b.target)));
        for edge in edges {
            if unresolved(&edge) || !edge.source.contains("::") || frontier.contains(&edge.source) {
                continue;
            }
            if let Some(n) = nodes(conn, "qualified_name=?1", &edge.source)?.pop() {
                if n["kind"] == "Test" {
                    result(&mut rows, &mut seen, n, true, None);
                } else if is_test(&n, root) {
                    next.insert(edge.source);
                }
            }
        }
        frontier = next;
    }
    if selected["language"] == "php" {
        for input in &inputs {
            let Some(member) = nodes(conn, "qualified_name=?1", input)?.pop() else {
                continue;
            };
            for e in read_edges(conn, false, text(&member, "name"), "TESTED_BY")? {
                if e.extra["ambiguous_targets"]
                    .as_array()
                    .is_some_and(|v| v.contains(&json!(input)))
                    && let Some(n) =
                        nodes(conn, "qualified_name=?1 AND is_test=1", &e.target)?.pop()
                {
                    result(&mut rows, &mut seen, n, false, Some("ambiguous_call"));
                }
            }
        }
    }
    if selected["language"] == "cpp"
        && nodes(
            conn,
            "name=?1 AND language='cpp' AND kind IN ('Function','Test')",
            name,
        )?
        .len()
            > 1
    {
        return Ok(rows);
    }
    let mut patterns = vec![
        (format!("test_{name}"), false),
        (format!("Test{name}"), false),
    ];
    if selected["kind"] == "Class" {
        patterns.extend(
            ["Test", "Tests", "IT", "Spec"].map(|suffix| (format!("{name}{suffix}"), true)),
        );
    }
    for (pattern, class_only) in patterns {
        for n in fuzzy(conn, &pattern, 10)?.0 {
            if n["is_test"] != true || class_only && (n["kind"] != "Class" || n["name"] != pattern)
            {
                continue;
            }
            if text(&n, "name").rsplit_once("@L").is_some_and(|(_, tail)| {
                !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit())
            }) {
                let basename = Path::new(text(&n, "file_path"))
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if !regex::Regex::new(&format!(
                    "(?:^|[^a-z0-9]){}(?:[^a-z0-9]|$)",
                    regex::escape(&name.to_lowercase())
                ))?
                .is_match(&basename)
                {
                    continue;
                }
            }
            result(&mut rows, &mut seen, n, false, Some("naming_convention"));
        }
    }
    Ok(rows)
}

pub(super) fn explicit(
    conn: &Connection,
    selected: &Value,
) -> Result<(Vec<Pair>, Vec<String>, usize)> {
    let qn = text(selected, "qualified_name");
    let mut inputs = vec![qn.to_string()];
    if selected["kind"] == "File" {
        inputs.extend(
            nodes(
                conn,
                "file_path=?1 AND kind IN ('Class','Function','Method')",
                text(selected, "file_path"),
            )?
            .iter()
            .map(|n| text(n, "qualified_name").to_string()),
        );
    } else if selected["kind"] == "Class" {
        inputs.extend(
            read_edges(conn, false, qn, "CONTAINS")?
                .into_iter()
                .map(|e| e.target),
        );
    }
    let limit = std::env::var("CRG_MAX_TRANSITIVE_FRONTIER")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(50);
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut callee_targets = BTreeSet::new();
    for input in &inputs {
        for e in read_edges(conn, false, input, "TESTED_BY")? {
            if !unresolved(&e)
                && let Some(n) = nodes(conn, "qualified_name=?1", &e.target)?.pop()
            {
                result(&mut rows, &mut seen, n, false, None);
            }
        }
        for e in read_edges(conn, false, input, "CALLS")? {
            if !unresolved(&e) && !nodes(conn, "qualified_name=?1", &e.target)?.is_empty() {
                callee_targets.insert(e.target);
            }
        }
    }
    let bare = qn.rsplit("::").next().unwrap_or(qn);
    for e in read_edges(conn, false, bare, "TESTED_BY")? {
        if unresolved(&e) || !relationships::receiver_admits(&e.extra, selected) {
            continue;
        }
        let candidates = nodes(
            conn,
            "name=?1 AND kind IN ('Function','Test','Class')",
            bare,
        )?;
        let same: Vec<_> = candidates
            .iter()
            .filter(|n| n["file_path"] == e.file)
            .collect();
        let imports: HashSet<_> = read_edges(conn, false, &e.file, "IMPORTS_FROM")?
            .into_iter()
            .map(|e| e.target)
            .collect();
        let imported: Vec<_> = candidates
            .iter()
            .filter(|n| imports.contains(text(n, "file_path")))
            .collect();
        let matched = if same.is_empty() { &imported } else { &same };
        if matched.len() == 1
            && text(matched[0], "qualified_name") == qn
            && let Some(n) = nodes(conn, "qualified_name=?1", &e.target)?.pop()
        {
            result(&mut rows, &mut seen, n, false, None);
        }
    }
    for callee in callee_targets.into_iter().take(limit) {
        for e in read_edges(conn, false, &callee, "TESTED_BY")? {
            if !unresolved(&e)
                && let Some(n) = nodes(conn, "qualified_name=?1", &e.target)?.pop()
            {
                result(&mut rows, &mut seen, n, true, None);
            }
        }
    }
    Ok((rows, inputs, limit))
}
