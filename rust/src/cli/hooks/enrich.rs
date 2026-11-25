use super::{payload, store};
use anyhow::Result;
use regex::Regex;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{path::Path, sync::LazyLock};

fn pattern<'a>(tool: &str, input: &'a Value) -> Option<&'a str> {
    match tool {
        "Grep" => input["pattern"].as_str(),
        "Glob" => {
            static GLOB: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"[*/]([a-zA-Z][a-zA-Z0-9_]{2,})").unwrap());
            GLOB.captures(input["pattern"].as_str()?)?
                .get(1)
                .map(|m| m.as_str())
        }
        "Bash" => {
            static COMMAND: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"\brg$|\bgrep$").unwrap());
            let mut tokens = input["command"]
                .as_str()?
                .split_whitespace()
                .skip_while(|s| !COMMAND.is_match(s));
            tokens.next()?;
            while let Some(token) = tokens.next() {
                if !token.starts_with('-') {
                    return Some(token.trim_matches(['\'', '"']));
                }
                if matches!(
                    token,
                    "-e" | "-f"
                        | "-m"
                        | "-A"
                        | "-B"
                        | "-C"
                        | "-g"
                        | "--glob"
                        | "-t"
                        | "--type"
                        | "--include"
                        | "--exclude"
                        | "--max-count"
                        | "--max-depth"
                        | "--max-filesize"
                        | "--color"
                        | "--colors"
                        | "--context-separator"
                        | "--field-match-separator"
                        | "--path-separator"
                        | "--replace"
                        | "--sort"
                        | "--sortr"
                ) {
                    tokens.next();
                }
            }
            None
        }
        _ => None,
    }
}

fn relative<'a>(path: &'a str, root: &Path) -> &'a str {
    Path::new(path)
        .strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .unwrap_or(path)
}

fn names(
    db: &Connection,
    id: &str,
    outgoing: bool,
    kind: &str,
    unique: bool,
    limit: usize,
) -> Result<Vec<String>> {
    let (by, other) = if outgoing {
        ("source_qualified", "target_qualified")
    } else {
        ("target_qualified", "source_qualified")
    };
    let sql = format!(
        "SELECT n.name FROM edges e JOIN nodes n ON n.qualified_name=e.{other} WHERE e.{by}=? AND e.kind=? ORDER BY e.id"
    );
    let mut stmt = db.prepare(&sql)?;
    let mut rows = stmt.query(params![id, kind])?;
    let mut names = Vec::new();
    while let Some(row) = rows.next()? {
        let s: String = row.get(0)?;
        if !unique || !names.contains(&s) {
            names.push(s);
            if names.len() == limit {
                break;
            }
        }
    }
    Ok(names)
}

fn context(db: &Connection, id: i64, root: &Path) -> Result<String> {
    let node_sql = "SELECT n.name,n.qualified_name,n.file_path,coalesce(n.line_start,0),c.name
        FROM nodes n LEFT JOIN communities c
          ON c.id=coalesce(nullif(json_extract(n.extra,'$.community_id'),0),n.community_id)
        WHERE n.id=?";
    let (name, qn, file, line, community) = db.query_row(node_sql, [id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut loc = relative(&file, root).to_owned();
    if line != 0 {
        loc.push_str(&format!(":{line}"));
    }
    let mut lines = vec![format!("{name} ({loc})")];
    if let Some(c) = community.filter(|s| !s.is_empty()) {
        lines[0].push_str(&format!(" [{c}]"));
    }
    let flow_sql = "SELECT f.name FROM flow_memberships m JOIN flows f ON f.id=m.flow_id
          WHERE m.node_id=? LIMIT 3";
    let flows = db
        .prepare(flow_sql)?
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (label, items) in [
        ("Called by", names(db, &qn, false, "CALLS", true, 5)?),
        ("Calls", names(db, &qn, true, "CALLS", true, 5)?),
        ("Flows", flows),
        ("Tests", names(db, &qn, true, "TESTED_BY", false, 3)?),
    ] {
        if !items.is_empty() {
            lines.push(format!("  {label}: {}", items.join(", ")));
        }
    }
    Ok(lines.join("\n"))
}

fn read_ids(db: &Connection, file: &str) -> Result<Vec<i64>> {
    let sql = "SELECT id FROM nodes WHERE file_path=?
        AND kind IN ('Function','Class','Type','Test') ORDER BY id LIMIT 10";
    Ok(db
        .prepare(sql)?
        .query_map([file], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn enrich(event: &Value) -> Result<Option<String>> {
    let Some(tool) = event["tool_name"].as_str() else {
        return Ok(None);
    };
    let input = &event["tool_input"];
    if !input.is_object() {
        return Ok(None);
    }
    let cwd = if let Some(s) = event["cwd"].as_str() {
        super::resolve(&std::path::absolute(s)?).unwrap_or(std::env::current_dir()?)
    } else {
        std::env::current_dir()?
    };
    let root = std::env::var_os("CRG_REPO_ROOT")
        .and_then(|p| Path::new(&p).canonicalize().ok())
        .or_else(|| {
            cwd.ancestors()
                .find(|p| p.join(".git").exists())
                .map(Path::to_owned)
        })
        .or_else(|| {
            cwd.ancestors()
                .filter(|p| p.join(".svn").exists())
                .last()
                .map(Path::to_owned)
        })
        .unwrap_or(cwd);
    if !store::database(&root, None).is_file() {
        return Ok(None);
    }
    let db_path = root.join(".code-review-graph/graph.db");
    if !db_path.is_file() {
        return Ok(None);
    }
    let db = store::open(&db_path)?;
    let (ids, title) = if tool == "Read" {
        let Some(file) = input["file_path"].as_str().filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let mut ids = read_ids(&db, file)?;
        if ids.is_empty()
            && let Ok(abs) = Path::new(file).canonicalize()
        {
            ids = read_ids(&db, &abs.to_string_lossy())?;
        }
        (ids, format!("in {}", relative(file, &root)))
    } else {
        let Some(text) = pattern(tool, input).filter(|p| p.chars().count() >= 3) else {
            return Ok(None);
        };
        let mut ids = Vec::new();
        for (id, _) in crate::cli::search::fts(&db, text, None, 8, false)? {
            if !db.query_row(
                "SELECT coalesce(is_test,0) FROM nodes WHERE id=?",
                [id],
                |r| r.get::<_, bool>(0),
            )? {
                ids.push(id);
                if ids.len() == 5 {
                    break;
                }
            }
        }
        (ids, format!("matching \"{text}\""))
    };
    if ids.is_empty() {
        return Ok(None);
    }
    let blocks = ids
        .iter()
        .map(|id| context(&db, *id, &root))
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(format!(
        "[code-review-graph] {} symbol(s) {title}:\n{}\n",
        ids.len(),
        blocks.join("\n\n")
    )))
}

pub(in crate::cli) fn search_hook() -> Result<()> {
    let Some(event) = payload() else {
        return Ok(());
    };
    if event.get("tool_input").is_none_or(Value::is_object)
        && event.get("cwd").is_some_and(|cwd| !cwd.is_string())
    {
        anyhow::bail!("hook cwd must be a string");
    }
    match enrich(&event) {
        Ok(Some(context)) => print!(
            "{}",
            crate::cli::wire::json_text(
                &json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":context}}),
                false
            )
        ),
        Ok(None) => {}
        Err(e) => eprintln!("Could not read the graph for hook enrichment: {e:#}"),
    }
    Ok(())
}
