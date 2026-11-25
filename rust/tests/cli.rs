use std::{
    fs::{self, File},
    path::Path,
    process::{Command, Output},
};

use rusqlite::Connection;
use tempfile::tempdir;

fn cli(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_code-review-graph"))
        .env_clear()
        .env("PATH", repo.join("empty-bin"))
        .env("HOME", repo)
        .env("CRG_HOME", repo.join("registry"))
        .args(args)
        .arg("--repo")
        .arg(repo)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn builds_and_queries_without_external_programs() {
    let repo = tempdir().unwrap();
    let root = repo.path().canonicalize().unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(
        root.join("math.py"),
        "def inc(x):\n    return x + 1\n\ndef twice(value):\n    return inc(inc(value))\n",
    )
    .unwrap();
    success(cli(&root, &["build"]));
    let db = Connection::open(root.join(".code-review-graph/graph.db")).unwrap();
    let state: String = db
        .query_row(
            "SELECT value FROM metadata WHERE key='build_state'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "complete");
    let signature: String = db
        .query_row("SELECT signature FROM nodes WHERE name='twice'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(signature.contains("twice") && signature.contains("value"));
    let answer: serde_json::Value =
        serde_json::from_str(&success(cli(&root, &["query", "callers_of", "inc"]))).unwrap();
    assert_eq!(answer["status"], "ok");
    assert!(
        answer["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "twice")
    );
    fs::write(root.join("math.py"), [0xff]).unwrap();
    assert!(!cli(&root, &["update"]).status.success());
    let previous: i64 = db
        .query_row("SELECT count(*) FROM nodes WHERE name='twice'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(previous, 1);
}

#[test]
fn concurrent_build_reports_the_process_that_holds_the_lock() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    let dir = root.join(".code-review-graph");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("write.lock"), "12345").unwrap();
    let holder = File::open(dir.join("write.lock")).unwrap();
    holder.lock().unwrap();
    let busy = cli(root, &["build", "--skip-postprocess"]);
    assert_eq!(busy.status.code(), Some(75));
    assert!(String::from_utf8_lossy(&busy.stderr).contains("PID 12345"));
    assert!(!dir.join("graph.db").exists());
}

#[test]
fn json_paths_merge_array_locations_and_exclude_values() {
    let repo = tempdir().unwrap();
    let root = repo.path().canonicalize().unwrap();
    fs::write(
        root.join("settings.json"),
        r#"{"items":[{"price":"private-value"},{"price":9}]}"#,
    )
    .unwrap();
    success(cli(&root, &["build", "--skip-flows"]));
    let db = Connection::open(root.join(".code-review-graph/graph.db")).unwrap();
    let extra: String = db
        .query_row(
            "SELECT extra FROM nodes WHERE kind='JsonPath' AND name='$.items[*].price'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let meta: serde_json::Value = serde_json::from_str(&extra).unwrap();
    assert_eq!(meta["value_type"], "mixed");
    assert_eq!(meta["occurrence_count"], 2);
    assert_eq!(meta["value_types"], serde_json::json!(["number", "string"]));
    assert!(!extra.contains("private-value"));
    let cache = Connection::open(root.join(".code-review-graph/graph.rust-cache")).unwrap();
    let facts: String = cache
        .query_row(
            "SELECT facts FROM files WHERE path='settings.json'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!facts.contains("private-value"));
}
