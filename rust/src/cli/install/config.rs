use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;
use toml_edit::{Array, DocumentMut, Table, value};

fn is_ours(command: &str, args: impl Iterator<Item = String>) -> bool {
    let args: Vec<_> = args.collect();
    let name = Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let native =
        name == "code-review-graph" && args.first().is_some_and(|s| s == "serve" || s == "mcp");
    let python =
        name.starts_with("python") && args.windows(2).any(|w| w == ["-m", "code_review_graph"]);
    native
        || python
        || (matches!(name, "uv" | "uvx" | "pipx") && args.iter().any(|s| s == "code-review-graph"))
}

pub(super) fn json_config(
    before: &str,
    root: &Path,
    binary: &Path,
    remove: bool,
) -> Result<String> {
    let mut doc = if before.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(before)?
    };
    let obj = doc.as_object_mut().context("expected a JSON object")?;
    if remove && !obj.contains_key("mcpServers") {
        return Ok(before.into());
    }
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("mcpServers must be an object")?;
    if let Some(previous) = servers.get("code-review-graph") {
        ensure!(
            is_ours(
                previous["command"].as_str().unwrap_or(""),
                previous["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
            ),
            "existing MCP entry belongs to another command"
        );
    } else if remove {
        return Ok(before.into());
    }
    if remove {
        servers.remove("code-review-graph");
    } else {
        let desired = json!({"command":binary,"args":["serve"],"cwd":root,"type":"stdio"});
        if servers.get("code-review-graph") == Some(&desired) {
            return Ok(before.into());
        }
        servers.insert("code-review-graph".into(), desired);
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&doc)?))
}

pub(super) fn codex_config(
    before: &str,
    root: &Path,
    binary: &Path,
    remove: bool,
) -> Result<String> {
    let mut doc = before.parse::<DocumentMut>()?;
    if !doc.as_table().contains_key("mcp_servers") {
        if remove {
            return Ok(before.into());
        }
        doc["mcp_servers"] = Table::new().into();
        doc["mcp_servers"]
            .as_table_mut()
            .unwrap()
            .set_implicit(true);
    }
    let table = doc["mcp_servers"]
        .as_table_mut()
        .context("mcp_servers must be a table")?;
    if let Some(previous) = table.get("code-review-graph") {
        let old = previous
            .as_table_like()
            .context("MCP entry must be a table")?;
        ensure!(
            is_ours(
                old.get("command").and_then(|v| v.as_str()).unwrap_or(""),
                old.get("args")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flat_map(|a| a.iter())
                    .filter_map(|v| v.as_str().map(str::to_owned))
            ),
            "existing MCP entry belongs to another command"
        );
        if remove {
            table.remove("code-review-graph");
            return Ok(doc.to_string());
        }
    } else if remove {
        return Ok(before.into());
    }
    let entry = table
        .entry("code-review-graph")
        .or_insert_with(|| Table::new().into())
        .as_table_mut()
        .context("MCP entry must be a table")?;
    let mut args = Array::new();
    args.push("serve");
    for (key, field) in [
        ("command", value(binary.to_string_lossy().to_string())),
        ("args", value(args)),
        ("cwd", value(root.to_string_lossy().to_string())),
        ("type", value("stdio")),
    ] {
        entry.insert(key, field);
    }
    Ok(doc.to_string())
}

fn same_hook(existing: &Value, desired: &Value, template: &Value) -> bool {
    if existing == desired {
        return true;
    }
    let mut normalized = existing.clone();
    let Some(entries) = normalized["hooks"].as_array_mut() else {
        return false;
    };
    for entry in entries {
        let Some(command) = entry["command"].as_str() else {
            return false;
        };
        let Some((_, tail)) = command.split_once("[ -x ") else {
            return false;
        };
        let Some((program, _)) = tail.split_once(" ];") else {
            return false;
        };
        if !program.starts_with('\'') || !program.ends_with('\'') {
            return false;
        }
        entry["command"] = command
            .replace(program, "__CRG_SH__")
            .replace("__CRG_SH__ -m code_review_graph ", "__CRG_SH__ ")
            .into();
    }
    normalized == *template
}

pub(super) fn hooks(before: &str, template: &str, binary: &Path) -> Result<String> {
    let mut doc = if before.is_empty() {
        json!({})
    } else {
        serde_json::from_str(before)?
    };
    let target = doc
        .as_object_mut()
        .context("invalid hook settings")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("invalid hooks")?;
    let template: Value = serde_json::from_str(template)?;
    let quoted = format!("'{}'", binary.to_string_lossy().replace('\'', "'\\''"));
    for (event, values) in template["hooks"]
        .as_object()
        .context("missing hook template")?
    {
        let entries = target
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("hook entries must be an array")?;
        for group in values.as_array().context("invalid hook template")? {
            let original = group;
            let mut group = group.clone();
            for hook in group["hooks"]
                .as_array_mut()
                .context("invalid hook group")?
            {
                hook["command"] = hook["command"]
                    .as_str()
                    .unwrap_or("")
                    .replace("__CRG_SH__", &quoted)
                    .into();
            }
            if let Some(previous) = entries
                .iter_mut()
                .find(|entry| same_hook(entry, &group, original))
            {
                *previous = group;
            } else {
                entries.push(group);
            }
        }
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&doc)?))
}
