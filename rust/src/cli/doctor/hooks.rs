use super::{
    Finding, VERSION, config, mismatch,
    probe::{self, NOT_RUN, Probes},
};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env,
    path::{Path, PathBuf},
};

const INSTALL: &str =
    "Install it where your shell finds it, for example: uv tool install code-review-graph";
const REWRITE: &str = "Run code-review-graph install again to rewrite the hooks.";
fn ours(text: &str) -> bool {
    ["code-review-graph", "code_review_graph", "crg-"]
        .iter()
        .any(|s| text.contains(s))
}
fn runners(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for pattern in [
        r#"(?m)(?:^|[\s"'])((?:/|[A-Za-z]:[\\/])[^\s"']*code-review-graph(?:\.exe)?)(?:["'\s]|$)"#,
        r#"(?m)(?:^|[\s"'])((?:/|[A-Za-z]:[\\/])[^\s"']*pythonw?[0-9.]*(?:\.exe)?)["']?\s+-m\s+code_review_graph\b"#,
        r#"'((?:/|[A-Za-z]:[\\/])[^']*pythonw?[0-9.]*(?:\.exe)?)'\s+-m\s+code_review_graph\b"#,
    ] {
        for c in Regex::new(pattern).unwrap().captures_iter(text) {
            found.insert(c[1].to_owned());
        }
    }
    if Regex::new(r"(?m)(?:^|[\s;&|({])uvx\s+code-review-graph\b")
        .unwrap()
        .is_match(text)
    {
        found.insert("uvx".into());
    }
    if found.is_empty() {
        found.insert("PATH".into());
    }
    found
}

fn editor_events(text: &str) -> Vec<String> {
    let Some(doc) = config::jsonc(text) else {
        return Vec::new();
    };
    let Some(hooks) = doc["hooks"].as_object() else {
        return Vec::new();
    };
    hooks
        .iter()
        .filter_map(|(event, entries)| {
            let ours_hook = |v: &Value| v["command"].as_str().is_some_and(ours);
            entries
                .as_array()?
                .iter()
                .any(|v| {
                    ours_hook(v)
                        || v["hooks"]
                            .as_array()
                            .is_some_and(|inner| inner.iter().any(ours_hook))
                })
                .then(|| event.clone())
        })
        .collect()
}
fn check_uvx(probes: &mut Probes, path_copy: bool) -> Option<Finding> {
    if probes.on_path("uvx").is_none() {
        return (!path_copy).then(||Finding::new("warn","The hooks call uvx, or code-review-graph from PATH, but neither is on PATH here, so they do nothing.").fix(INSTALL));
    }
    match probes.uv_tool().as_deref() {
        None => Some(Finding::new(
            "warn",
            "The hooks run uvx code-review-graph. Version unknown: uvx picks a copy when it starts, and finding out could mean a download. To see it, run: uvx code-review-graph --version",
        )),
        Some(VERSION) => None,
        Some(v) => Some(
            Finding::new(
                "fail",
                format!("The hooks run uvx code-review-graph, which is {v}, not {VERSION}."),
            )
            .fix(mismatch(
                v,
                "the copy uvx runs",
                "uv tool upgrade code-review-graph",
            )),
        ),
    }
}

pub(super) fn check(
    root: &Path,
    probes: &mut Probes,
    path_copy: bool,
    checks: &mut Vec<Finding>,
) -> Vec<Value> {
    let mut hooks = Vec::new();
    let mut named = BTreeSet::new();
    let hooks_dir = probes
        .git(&["rev-parse", "--git-path", "hooks"])
        .and_then(|s| s.trim().lines().next().map(|v| root.join(v)))
        .unwrap_or_else(|| root.join(".git/hooks"));
    let mut labels = Vec::new();
    let mut add = |label: &str, file: &Path, events: Vec<String>| {
        if !events.is_empty() {
            labels.push(format!("{label} {}", events.join(", ")));
            hooks.push(json!({"where":label,"file":file,"events":events}));
        }
    };
    let mut events = Vec::new();
    for name in ["pre-commit", "post-checkout", "post-merge", "post-rewrite"] {
        if let Some(text) = config::read(&hooks_dir.join(name)).filter(|v| ours(v)) {
            events.push(name.to_owned());
            named.extend(runners(&text));
        }
    }
    add("git", &hooks_dir, events);
    let mut settings = vec![
        ("Claude Code", root.join(".claude/settings.json")),
        ("Qoder", root.join(".qoder/settings.json")),
        ("CodeBuddy Code", root.join(".codebuddy/settings.json")),
        ("Gemini CLI", root.join(".gemini/settings.json")),
    ];
    let home = env::home_dir();
    if let Some(codex) = env::var_os("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|p| p.join(".codex")))
    {
        settings.push(("Codex", codex.join("hooks.json")));
    }
    if let Some(home) = home {
        settings.push(("Cursor", home.join(".cursor/hooks.json")));
    }
    for (label, path) in settings {
        if let Some(text) = config::read(&path) {
            let events = editor_events(&text);
            if !events.is_empty() {
                named.extend(runners(&text));
                add(label, &path, events);
            }
        }
    }
    if hooks.is_empty() {
        checks.push(Finding::new("info","Hooks: none found, so the graph changes only when you run update or build. code-review-graph install adds them."));
        return hooks;
    }
    checks.push(Finding::new("ok", format!("Hooks: {}.", labels.join("; "))));
    if named.remove("PATH") && !path_copy {
        checks.push(Finding::new("warn","The hooks call code-review-graph from PATH, but there is none on PATH here, so they do nothing.").fix(INSTALL));
    }
    if named.remove("uvx")
        && let Some(f) = check_uvx(probes, path_copy)
    {
        checks.push(f);
    }
    for name in named {
        let Some(path) = probes.allowed(&name) else {
            checks.push(
                Finding::new(
                    "warn",
                    format!("The hooks run {name}. Version unknown: {NOT_RUN}."),
                )
                .fix(REWRITE),
            );
            continue;
        };
        let python = probe::python(probe::launcher(&name));
        if !python && probe::launcher(&name) != "code-review-graph" {
            continue;
        }
        match probes.version(&path, python).as_deref() {
            None => checks.push(
                Finding::new(
                    "fail",
                    format!("The hooks run {name}, which did not answer --version."),
                )
                .fix(REWRITE),
            ),
            Some(VERSION) => {}
            Some(v) => checks.push(
                Finding::new(
                    "fail",
                    format!("The hooks run {name}, which is {v}, not {VERSION}."),
                )
                .fix(mismatch(
                    v,
                    &format!("the copy at {name}"),
                    probe::upgrade(&path),
                )),
            ),
        }
    }
    hooks
}
