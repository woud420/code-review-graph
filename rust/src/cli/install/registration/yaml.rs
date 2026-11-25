use super::{Form, Outcome, config};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use serde_yaml_ng::{Mapping, Value as Yaml};
use std::path::Path;

fn string(v: &Value) -> String {
    serde_json::to_string(v).unwrap()
}
fn nl(raw: &str) -> String {
    if raw.is_empty() || raw.ends_with('\n') {
        raw.to_owned()
    } else {
        raw.to_owned() + "\n"
    }
}
fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}
fn normal(y: &Yaml) -> Result<Value> {
    Ok(serde_json::to_value(y)?)
}
fn pad(s: &str, n: usize) -> String {
    s.lines()
        .map(|l| format!("{}{}\n", " ".repeat(n), l))
        .collect()
}
fn check(next: String, expected: Yaml) -> Result<Outcome> {
    ensure!(
        serde_yaml_ng::from_str::<Yaml>(&next)? == expected,
        "YAML splice changed unrelated values"
    );
    Ok(Outcome::Changed(next))
}
fn hermes(raw: &str, mut doc: Yaml, entry: &Value, binary: &Path) -> Result<Outcome> {
    if doc.is_null() {
        doc = Yaml::Mapping(Mapping::new())
    }
    let servers = doc
        .as_mapping_mut()
        .context("expected a top-level YAML map")?
        .entry("mcp_servers".into())
        .or_insert(Yaml::Null);
    if servers.is_null() {
        *servers = Yaml::Mapping(Mapping::new())
    }
    let map = servers.as_mapping_mut().context("expected MCP map")?;
    let old = map.get("code-review-graph");
    if let Some(old) = old {
        let old = normal(old)?;
        if &old == entry {
            return Ok(Outcome::Existing);
        }
        if !config::is_ours(&old, binary) {
            return Ok(Outcome::Manual);
        }
    }
    let existing = old.is_some();
    map.insert("code-review-graph".into(), serde_yaml_ng::to_value(entry)?);
    let body = serde_yaml_ng::to_string(&json!({"code-review-graph":entry}))?;
    let raw = nl(raw);
    let lines: Vec<_> = raw.split_inclusive('\n').collect();
    let header = lines
        .iter()
        .position(|l| l.trim_end().strip_suffix(':') == Some("mcp_servers"));
    let next = if let Some(i) = header {
        let end = (i + 1..lines.len())
            .find(|j| {
                let s = lines[*j];
                !s.trim().is_empty() && !s.trim_start().starts_with('#') && indent(s) == 0
            })
            .unwrap_or(lines.len());
        let width = lines[i + 1..end]
            .iter()
            .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .map(|l| indent(l))
            .unwrap_or(2);
        let range = if existing {
            let j = (i + 1..end)
                .find(|j| {
                    indent(lines[*j]) == width
                        && lines[*j].trim_start().starts_with("code-review-graph:")
                })
                .context("uncertain entry span")?;
            let mut k = (j + 1..end)
                .find(|k| !lines[*k].trim().is_empty() && indent(lines[*k]) <= width)
                .unwrap_or(end);
            while k > j + 1 && lines[k - 1].trim().is_empty() {
                k -= 1
            }
            j..k
        } else {
            end..end
        };
        lines[..range.start].concat() + &pad(&body, width) + &lines[range.end..].concat()
    } else {
        let separate = if !raw.is_empty() && lines.last().is_some_and(|v| !v.trim().is_empty()) {
            "\n"
        } else {
            ""
        };
        format!("{raw}{separate}mcp_servers:\n{}", pad(&body, 2))
    };
    check(next, doc)
}
fn dsh(raw: &str, mut doc: Yaml, entry: &Value, binary: &Path) -> Result<Outcome> {
    if doc.is_null() {
        doc = Yaml::Sequence(vec![])
    }
    let ops = doc
        .as_sequence_mut()
        .context("expected a list of DSH operations")?;
    let row = json!({"id":"mcp-code-review-graph","name":"@deepseek-ai/dsh-mcp-client","config":{"serverName":"code-review-graph","transport":"stdio","command":entry["command"],"args":entry["args"]}});
    let operation = serde_yaml_ng::to_value(json!({"insert":[row.clone()]}))?;
    let mut found = None;
    for (i, op) in ops.iter().enumerate() {
        if let Some(rows) = op.get("insert").and_then(Yaml::as_sequence) {
            for v in rows {
                let ours = v["id"].as_str() == Some("mcp-code-review-graph")
                    || (v["name"].as_str() == Some("@deepseek-ai/dsh-mcp-client")
                        && v["config"]["serverName"].as_str() == Some("code-review-graph"));
                if !ours {
                    continue;
                }
                if found.is_some()
                    || rows.len() != 1
                    || op.as_mapping().is_none_or(|m| m.len() != 1)
                {
                    return Ok(Outcome::Manual);
                }
                if v == &serde_yaml_ng::to_value(&row)? {
                    return Ok(Outcome::Existing);
                }
                let old = normal(v)?;
                let cfg = &old["config"];
                if old.as_object().is_none_or(|m| m.len() != 3)
                    || old["id"] != row["id"]
                    || old["name"] != row["name"]
                    || cfg["serverName"] != "code-review-graph"
                    || cfg["transport"] != "stdio"
                    || cfg.as_object().is_none_or(|m| {
                        m.keys().any(|k| {
                            !matches!(k.as_str(), "serverName" | "transport" | "command" | "args")
                        })
                    })
                    || !config::is_ours(
                        &json!({"command":cfg["command"],"args":cfg["args"]}),
                        binary,
                    )
                {
                    return Ok(Outcome::Manual);
                }
                found = Some(i);
            }
        }
    }
    let text = format!(
        "- insert:\n    - id: mcp-code-review-graph\n      name: '@deepseek-ai/dsh-mcp-client'\n      config:\n        serverName: code-review-graph\n        transport: stdio\n        command: {}\n        args:\n          - serve\n",
        string(&entry["command"])
    );
    let raw = nl(raw);
    let lines: Vec<_> = raw.split_inclusive('\n').collect();
    let width = lines
        .iter()
        .find(|l| l.trim_start().starts_with("- "))
        .map(|l| indent(l))
        .unwrap_or(0);
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| (l.trim_start().starts_with("- ") && indent(l) == width).then_some(i))
        .collect();
    let next = if let Some(i) = found {
        ensure!(
            starts.len() == ops.len(),
            "cannot safely locate DSH operation"
        );
        let a = starts[i];
        let mut b = starts.get(i + 1).copied().unwrap_or(lines.len());
        while b > a + 1
            && (lines[b - 1].trim().is_empty()
                || (lines[b - 1].trim_start().starts_with('#') && indent(lines[b - 1]) <= width))
        {
            b -= 1
        }
        ops[i] = operation;
        lines[..a].concat() + &pad(&text, width) + &lines[b..].concat()
    } else {
        let empty = ops.is_empty();
        ops.push(operation);
        if empty && let Some(i) = lines.iter().position(|l| l.trim() == "[]") {
            lines[..i].concat() + &text + &lines[i + 1..].concat()
        } else {
            raw + &pad(&text, width)
        }
    };
    check(next, doc)
}
pub(super) fn set(raw: &str, form: Form, entry: &Value, binary: &Path) -> Result<Outcome> {
    let parsed = serde_yaml_ng::from_str(raw)?;
    if form == Form::Yaml {
        hermes(raw, parsed, entry, binary)
    } else {
        dsh(raw, parsed, entry, binary)
    }
}
