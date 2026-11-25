use super::*;
use rusqlite::OptionalExtension;

pub(super) fn compatible(a: &str, b: &str) -> bool {
    const JS: [&str; 4] = ["typescript", "tsx", "javascript", "html"];
    a.is_empty() || a == b || (JS.contains(&a) && JS.contains(&b))
}

pub(super) fn importer(conn: &Connection, selected: &Value) -> Result<Vec<Pair>> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let file = text(selected, "file_path");
    let mut collect = |target: &str, depth: Option<usize>| -> Result<()> {
        for e in read_edges(conn, true, target, "IMPORTS_FROM")? {
            if let Some(d) = depth
                && !(e.extra["import_scope"] == "tree"
                    || (d == 0 && e.extra["import_scope"] == "package"))
            {
                continue;
            }
            if !seen.insert(e.source.clone()) {
                continue;
            }
            let mut row = json!({"importer":e.source,"file":e.file});
            if depth.is_some() {
                row["via_package"] = json!(target);
            }
            result.push(Pair {
                key: e.source,
                row,
                edge: Some(e.wire),
                unresolved: false,
            });
        }
        Ok(())
    };
    collect(file, None)?;
    for (depth, parent) in Path::new(file).ancestors().skip(1).take(13).enumerate() {
        if let Some(dir) = parent.to_str() {
            collect(dir, Some(depth))?;
        }
    }
    if selected["language"] == "csharp" {
        let extra: Option<String> = conn
            .query_row(
                "SELECT extra FROM nodes WHERE file_path=?1 AND kind='File' LIMIT 1",
                [file],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(extra) = extra.and_then(|s| serde_json::from_str::<Value>(&s).ok())
            && let Some(names) = extra["csharp_namespaces"].as_array()
        {
            for ns in names.iter().filter_map(Value::as_str) {
                collect(ns, None)?;
            }
        }
    }
    Ok(result)
}

pub(super) fn inherits(conn: &Connection, selected: &Value) -> Result<Vec<Pair>> {
    let qn = text(selected, "qualified_name");
    let name = text(selected, "name");
    let lang = text(selected, "language");
    let mut result = Vec::new();
    for (lookup, inferred) in [(qn, false), (name, true)] {
        let ambiguous = inferred
            && nodes(conn, "name=?1 AND kind IN ('Class','Type')", name)?
                .iter()
                .filter(|n| compatible(lang, text(n, "language")))
                .count()
                > 1;
        let mut edges = read_edges(conn, true, lookup, "INHERITS")?;
        edges.extend(read_edges(conn, true, lookup, "IMPLEMENTS")?);
        if !inferred {
            edges.sort_by_key(|e| e.wire["id"].as_i64());
        }
        for edge in edges {
            let Some(mut row) = nodes(conn, "qualified_name=?1", &edge.source)?.pop() else {
                continue;
            };
            if inferred && !compatible(lang, text(&row, "language")) {
                continue;
            }
            if ambiguous {
                row["inferred_by"] = json!("bare_name");
            }
            result.push(Pair {
                key: edge.source,
                row,
                edge: Some(edge.wire),
                unresolved: false,
            });
        }
        if !result.is_empty() {
            break;
        }
    }
    Ok(result)
}

pub(super) fn config(conn: &Connection, raw: &str) -> Result<Vec<Pair>> {
    let raw = raw.strip_prefix("config:").unwrap_or(raw);
    let raw = raw.strip_suffix(".*").unwrap_or(raw);
    let mut segments = Vec::new();
    for segment in raw.trim().split('.') {
        let (base, index) = if let Some((before, index)) = segment.rsplit_once('[')
            && let Some(digits) = index.strip_suffix(']')
            && !digits.is_empty()
            && digits.bytes().all(|ch| ch.is_ascii_digit())
        {
            (before, format!("[{digits}]"))
        } else {
            (segment, String::new())
        };
        let tokens: Vec<_> = base.split(['-', '_']).filter(|s| !s.is_empty()).collect();
        let first = tokens.first().copied().unwrap_or_default();
        let mut key = if tokens.len() > 1 || base.to_uppercase() == base {
            first.to_lowercase()
        } else {
            first.to_string()
        };
        for token in tokens.iter().skip(1) {
            let mut chars = token.chars();
            if let Some(c) = chars.next() {
                key.extend(c.to_uppercase());
                key.push_str(&chars.as_str().to_lowercase());
            }
        }
        key.push_str(&index);
        segments.push(key);
    }
    let key = segments.join(".");
    let mut targets = vec![format!("config:{key}"), format!("config:{key}.*")];
    targets.extend((1..segments.len()).map(|i| format!("config:{}.*", segments[..i].join("."))));
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for target in targets {
        for e in read_edges(conn, true, &target, "DEPENDS_ON_CONFIG")? {
            if seen.insert(e.source.clone())
                && let Some(row) = nodes(conn, "qualified_name=?1", &e.source)?.pop()
            {
                result.push(Pair {
                    key: e.source,
                    row,
                    edge: Some(e.wire),
                    unresolved: false,
                });
            }
        }
    }
    Ok(result)
}

pub(super) fn receiver_admits(extra: &Value, node: &Value) -> bool {
    let parent = node["parent_name"].as_str();
    match extra["receiver_binding"].as_str() {
        Some("module") => {
            extra["receiver_module"].is_string()
                && extra["receiver_module"] == node["file_path"]
                && parent.is_none()
        }
        Some("class") => {
            let Some(class) = extra["receiver_class"].as_str().filter(|s| !s.is_empty()) else {
                return false;
            };
            if !extra["receiver_class_file"].is_null()
                && extra["receiver_class_file"] != node["file_path"]
            {
                return false;
            }
            parent == Some(class)
                || extra["receiver_scopes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|scope| parent == Some(&format!("{scope}.{class}")))
        }
        Some(_) => false,
        None => {
            let owner = extra["claimed_endpoint"]
                .as_str()
                .and_then(|s| {
                    s.rsplit("::")
                        .next()?
                        .rsplit_once('.')
                        .map(|(owner, _)| owner)
                })
                .or_else(|| {
                    (extra["receiver_resolution"] == "typed_receiver_unresolved")
                        .then(|| extra["receiver_type"].as_str())
                        .flatten()
                });
            owner.is_none_or(|owner| {
                parent.is_some_and(|parent| {
                    parent
                        .rsplit('.')
                        .zip(owner.rsplit('.'))
                        .all(|(a, b)| a == b)
                })
            })
        }
    }
}
