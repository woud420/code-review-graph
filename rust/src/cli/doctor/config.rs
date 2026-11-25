use super::{
    Finding, VERSION, mismatch,
    probe::{self, NOT_RUN, Probes},
};
use regex::Regex;
use serde_json::{Value, json};
use std::{fs, io::Read, path::Path};

const CONFIGS: &[(&str, &str, &[&str])] = &[
    (
        ".mcp.json",
        "mcpServers",
        &["Claude Code", "CodeBuddy Code"],
    ),
    (".cursor/mcp.json", "mcpServers", &["Cursor"]),
    ("opencode.jsonc", "mcp", &["OpenCode"]),
    ("opencode.json", "mcp", &["OpenCode"]),
    (".mimocode/mimocode.json", "mcp", &["MiMo Code"]),
    (".mimocode/mimocode.jsonc", "mcp", &["MiMo Code"]),
    (".gemini/settings.json", "mcpServers", &["Gemini CLI"]),
    (".kiro/settings/mcp.json", "mcpServers", &["Kiro"]),
    (".qoder/mcp.json", "mcpServers", &["Qoder"]),
    (".vscode/mcp.json", "servers", &["GitHub Copilot"]),
];

pub(super) fn read(path: &Path) -> Option<String> {
    const LIMIT: u64 = 16 * 1024 * 1024;
    let m = fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > LIMIT {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= LIMIT as usize).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn jsonc(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str(text) {
        return Some(v);
    }
    let (mut quoted, mut escaped, mut comma) = (false, false, None);
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(ch) = it.next() {
        if quoted {
            if ch == '"' && !escaped {
                quoted = false;
            }
            escaped = ch == '\\' && !escaped;
        } else {
            match (ch, it.peek()) {
                ('/', Some('/')) => {
                    it.by_ref().take_while(|c| *c != '\n').for_each(drop);
                    continue;
                }
                ('/', Some('*')) => {
                    it.next();
                    while let Some(c) = it.next() {
                        if c == '*' && it.next_if_eq(&'/').is_some() {
                            break;
                        }
                    }
                    continue;
                }
                ('}', _) | (']', _) => {
                    if let Some(pos) = comma.take() {
                        out.remove(pos);
                    }
                }
                (',', _) => comma = Some(out.len()),
                ('"', _) => {
                    quoted = true;
                    comma = None;
                }
                (c, _) if !c.is_whitespace() => comma = None,
                _ => {}
            }
        }
        out.push(ch);
    }
    serde_json::from_str(&out).ok()
}

fn tokens(entry: &Value) -> Option<Vec<&str>> {
    let mut cmd = if let Some(s) = entry["command"].as_str() {
        vec![s]
    } else {
        entry["command"]
            .as_array()?
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?
    };
    if cmd.is_empty() {
        return None;
    }
    if let Some(args) = entry.get("args") {
        for a in args.as_array()? {
            cmd.push(a.as_str()?);
        }
    }
    Some(cmd)
}

struct Resolved {
    version: Option<String>,
    source: String,
    upgrade: String,
}
impl Resolved {
    fn unknown(source: impl Into<String>) -> Self {
        Self {
            version: None,
            source: source.into(),
            upgrade: String::new(),
        }
    }
    fn found(
        version: impl Into<String>,
        source: impl Into<String>,
        upgrade: impl Into<String>,
    ) -> Self {
        Self {
            version: Some(version.into()),
            source: source.into(),
            upgrade: upgrade.into(),
        }
    }
}

fn resolve(tokens: &[&str], probes: &mut Probes) -> Resolved {
    let exe = tokens[0];
    let name = probe::launcher(exe);
    let rest = &tokens[1..];
    if name == "uvx" || (name == "uv" && rest.starts_with(&["tool", "run"])) {
        let args = if name == "uv" { &rest[2..] } else { rest };
        let spec = args
            .iter()
            .enumerate()
            .find_map(|(i, w)| {
                if *w == "--from" {
                    args.get(i + 1).copied()
                } else {
                    w.strip_prefix("--from=")
                }
            })
            .or_else(|| args.iter().find(|s| !s.starts_with('-')).copied())
            .unwrap_or_default();
        if let Some(c) = Regex::new(r"^code-review-graph(?:\[[^\]]*\])?(?:==|@)(\d[^\s,;]*)$")
            .unwrap()
            .captures(spec)
        {
            return Resolved::found(
                &c[1],
                "pinned in the entry",
                "run code-review-graph install again to rewrite the entry",
            );
        }
        if !args.contains(&"--isolated")
            && let Some(v) = probes.uv_tool()
        {
            return Resolved::found(
                v,
                "the copy uv tool install put in place",
                "uv tool upgrade code-review-graph",
            );
        }
        return Resolved::unknown(
            "uvx picks a copy when it starts, and finding out could mean a download. To see it, run: uvx code-review-graph --version",
        );
    }
    if name == "uv" && rest.first() == Some(&"run") {
        return Resolved::unknown(
            "uv run uses the project's own copy. To see it, run: uv run code-review-graph --version",
        );
    }
    if name == "poetry" {
        return Resolved::unknown(
            "poetry run uses the project's own copy. To see it, run: poetry run code-review-graph --version",
        );
    }
    let python = probe::python(name) && rest.starts_with(&["-m", "code_review_graph"]);
    if !python && name != "code-review-graph" {
        return Resolved::unknown("it is a hand-written command doctor cannot read");
    }
    if !python && !exe.contains(['/', '\\']) && probes.on_path(exe).is_none() {
        return Resolved::unknown(
            "there is no code-review-graph on PATH here, and the AI tool's PATH may differ",
        );
    }
    let Some(p) = probes.allowed(exe) else {
        return Resolved::unknown(NOT_RUN);
    };
    let location = if python {
        exe
    } else {
        p.to_str().unwrap_or(exe)
    };
    let Some(v) = probes.version(&p, python) else {
        return Resolved::unknown(format!("{location} did not answer --version"));
    };
    if python {
        Resolved::found(
            v,
            format!("the copy installed for {exe}"),
            format!("\"{exe}\" -m pip install -U code-review-graph"),
        )
    } else {
        Resolved::found(v, format!("the copy at {location}"), probe::upgrade(&p))
    }
}

pub(super) fn entries(root: &Path, probes: &mut Probes, checks: &mut Vec<Finding>) -> Vec<Value> {
    let mut entries = Vec::new();
    for &(file, key, names) in CONFIGS {
        let path = root.join(file);
        if !path.canonicalize().is_ok_and(|p| p.starts_with(root)) {
            continue;
        }
        let Some(doc) = read(&path).and_then(|s| jsonc(&s)) else {
            continue;
        };
        let entry = &doc[key]["code-review-graph"];
        if !entry.is_object() {
            continue;
        }
        let label = format!("MCP entry in {file} ({})", names.join(", "));
        let Some(cmd) = tokens(entry) else {
            checks.push(Finding::new(
                "warn",
                format!("{label}: no command doctor can read."),
            ));
            continue;
        };
        let found = resolve(&cmd, probes);
        let command = cmd.join(" ");
        let check = match found.version.as_deref() {
            None => Finding::new(
                "warn",
                format!(
                    "{label} runs: {command}. Version unknown: {}.",
                    found.source
                ),
            ),
            Some(VERSION) => {
                Finding::new("ok", format!("{label} runs {VERSION} ({}).", found.source))
            }
            Some(v) => Finding::new(
                "fail",
                format!("{label} runs {v}, not {VERSION} ({}).", found.source),
            )
            .fix(mismatch(v, "the copy the MCP entry runs", &found.upgrade)),
        };
        checks.push(check);
        entries.push(json!({"file":file,"platforms":names,"command":command,"version":found.version,"source":found.source}));
    }
    entries
}
