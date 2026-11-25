use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;
use toml_edit::{Array, DocumentMut, Table, value};

pub(super) fn is_ours(entry: &Value, binary: &Path) -> bool {
    let Some(fields) = entry.as_object() else {
        return false;
    };
    if fields.keys().any(|k| {
        !matches!(
            k.as_str(),
            "name" | "type" | "command" | "args" | "cwd" | "tools" | "env"
        )
    }) || fields
        .get("type")
        .is_some_and(|t| !matches!(t.as_str(), Some("stdio" | "local")))
        || fields.get("tools").is_some_and(|t| *t != json!(["*"]))
        || fields
            .get("env")
            .is_some_and(|e| *e != json!({}) && *e != json!([]))
    {
        return false;
    }
    let mut tokens = match &entry["command"] {
        Value::String(command) => vec![command.as_str()],
        Value::Array(list) => {
            let Some(tokens) = list.iter().map(Value::as_str).collect::<Option<Vec<_>>>() else {
                return false;
            };
            tokens
        }
        _ => return false,
    };
    if let Some(args) = entry.get("args") {
        let Some(extra) = args
            .as_array()
            .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        else {
            return false;
        };
        tokens.extend(extra);
    }
    let Some((&command, rest)) = tokens.split_first() else {
        return false;
    };
    let mut args = rest.to_vec();
    if args.len() >= 2 && args[args.len() - 2] == "--repo" {
        args.truncate(args.len() - 2);
    }
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    match args.as_slice() {
        ["serve"] => name == "code-review-graph" || Path::new(command) == binary,
        ["code-review-graph", "serve"] => name == "uvx",
        ["run", "code-review-graph", "serve"] => matches!(name, "uv" | "poetry"),
        ["-m", "code_review_graph", "serve"] => {
            static PYTHON: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"^pythonw?[0-9]*(?:\.[0-9]+)*$").unwrap()
            });
            PYTHON.is_match(name)
        }
        _ => false,
    }
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
    if table.contains_key("code-review-graph") {
        let prior: Value = toml::from_str(before)?;
        if !is_ours(&prior["mcp_servers"]["code-review-graph"], binary) {
            return Ok(before.into());
        }
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

pub(super) fn without_steering(before: &str, template: &str) -> Result<String> {
    if before.is_empty() {
        return Ok(String::new());
    }
    let mut doc: Value = serde_json::from_str(before)?;
    let Some(groups) = doc["hooks"]["PreToolUse"].as_array_mut() else {
        return Ok(before.into());
    };
    let template: Value = serde_json::from_str(template)?;
    let wanted = &template["hooks"]["PreToolUse"][0];
    let mut changed = false;
    for group in groups
        .iter_mut()
        .filter(|g| g["matcher"] == wanted["matcher"])
    {
        let Some(items) = group["hooks"].as_array_mut() else {
            continue;
        };
        items.retain(|item| {
            let single = json!({"matcher":wanted["matcher"],"hooks":[item]});
            let remove = same_hook(&single, wanted, wanted);
            changed |= remove;
            !remove
        });
    }
    if !changed {
        return Ok(before.into());
    }
    groups.retain(|g| !g["hooks"].as_array().is_some_and(Vec::is_empty));
    if groups.is_empty() {
        doc["hooks"].as_object_mut().unwrap().remove("PreToolUse");
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&doc)?))
}
