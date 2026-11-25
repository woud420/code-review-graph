use super::{Finding, UPGRADE, VERSION, probe::Probes};
use crate::cli::{reports::context, store};
use anyhow::{Result, anyhow};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const BUILD: &str = "Run: code-review-graph build";
const SCHEMA: i64 = 15;

fn readonly(path: &Path) -> Result<Connection> {
    let mut url = url::Url::from_file_path(path.canonicalize()?)
        .map_err(|_| anyhow!("invalid database path"))?;
    let has_sidecars = ["-wal", "-shm"].iter().any(|s| {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(s);
        Path::new(&sidecar).exists()
    });
    url.set_query(Some(if has_sidecars {
        "mode=ro"
    } else {
        "mode=ro&immutable=1"
    }));
    let db = Connection::open_with_flags(
        url.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    db.busy_timeout(Duration::from_secs(2))?;
    Ok(db)
}

fn read(
    root: &Path,
    path: &Path,
    info: &mut Value,
    checks: &mut Vec<Finding>,
    probes: &Probes,
) -> Result<()> {
    let conn = readonly(path)?;
    let schema = store::metadata(&conn, "schema_version")?
        .as_deref()
        .unwrap_or("1")
        .parse::<i64>()?;
    let state = store::metadata(&conn, "build_state")?.filter(|s| !s.is_empty());
    let built = store::metadata(&conn, "git_head_sha")?.filter(|s| !s.is_empty());
    let updated = store::metadata(&conn, "last_updated")?;
    let (nodes, files): (i64, i64) = conn.query_row(
        "SELECT count(*), count(*) FILTER (WHERE kind='File') FROM nodes",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    info["schema_version"] = json!(schema);
    info["nodes"] = json!(nodes);
    info["files"] = json!(files);
    if schema > SCHEMA {
        checks.push(Finding::new("fail",format!("The graph is schema v{schema}, written by a newer copy; this copy ({VERSION}) understands v{SCHEMA}.")).fix(format!("Upgrade this copy ({UPGRADE}).")));
        return Ok(());
    }
    if schema < SCHEMA {
        checks.push(Finding::new("warn",format!("The graph is schema v{schema}; the next build or update moves it to v{SCHEMA}, and older copies cannot read it after that.")));
    }
    info["build_state"] = json!(state);
    if matches!(
        state.as_deref(),
        Some("in-progress" | "postprocess-pending")
    ) {
        checks.push(
            Finding::new(
                "fail",
                "The last build did not finish, so files may be missing.",
            )
            .fix(BUILD),
        );
    }
    let head = built
        .as_ref()
        .filter(|_| root.join(".git").exists())
        .and_then(|_| probes.git(&["rev-parse", "--verify", "HEAD"]))
        .and_then(|s| s.trim().lines().next().map(str::to_owned));
    info["built_at_sha"] = json!(built);
    info["head_sha"] = json!(head);
    info["updated_at"] = json!(updated);
    let summary = format!("Graph: schema v{schema}, {files} files, {nodes} nodes");
    let (stale, message) = match (built.as_deref(), head.as_deref()) {
        (Some(b), Some(h)) => {
            let b7 = b.chars().take(7).collect::<String>();
            let h7 = h.chars().take(7).collect::<String>();
            if b == h {
                (
                    Some(false),
                    Finding::new("ok", format!("{summary}, built at HEAD ({h7}).")),
                )
            } else {
                let held = b.bytes().all(|c| c.is_ascii_hexdigit())
                    && probes
                        .git(&[
                            "diff",
                            "--no-ext-diff",
                            "--no-textconv",
                            "--relative",
                            "--name-only",
                            "--no-renames",
                            "-z",
                            b,
                            "HEAD",
                            "--",
                            ".",
                        ])
                        .is_some_and(|raw| {
                            context::holds_paths(&conn, root, raw.as_bytes()).unwrap_or(false)
                        });
                if held {
                    (
                        Some(false),
                        Finding::new(
                            "ok",
                            format!(
                                "{summary}, built at {b7}. HEAD is {h7}, but the graph already holds every file changed since, so it is current."
                            ),
                        ),
                    )
                } else {
                    (
                        Some(true),
                        Finding::new(
                            "fail",
                            format!("{summary}, built at {b7} but HEAD is {h7}."),
                        )
                        .fix("Run: code-review-graph update"),
                    )
                }
            }
        }
        _ => (
            None,
            Finding::new(
                "info",
                format!("{summary}. Cannot tell which commit it was built at."),
            ),
        ),
    };
    info["stale"] = json!(stale);
    checks.push(message);
    Ok(())
}

pub(super) fn check(root: &Path, probes: &Probes, checks: &mut Vec<Finding>) -> Value {
    let path = store::database(root, None);
    let mut info = json!({"path":path,"exists":path.is_file(),"supported_schema_version":SCHEMA});
    if !path.is_file() {
        checks.push(Finding::new("fail", format!("No graph at {}.", path.display())).fix(BUILD));
    } else if let Err(err) = read(root, &path, &mut info, checks, probes) {
        checks.push(
            Finding::new(
                "fail",
                format!("Could not read the graph at {} ({err}).", path.display()),
            )
            .fix(BUILD),
        );
    }
    info
}
