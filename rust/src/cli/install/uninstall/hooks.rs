use super::super::jsonc::{self, Node};
use super::{
    catalog,
    report::{self, Report},
};
use serde_json::Value;
use std::{ops::Range, path::Path};

fn known(command: &str, group: &str, root: &Path) -> bool {
    let quoted = command
        .split_once("[ -x ")
        .and_then(|(_, s)| s.split_once(" ];"))
        .map(|(program, _)| program);
    let canonical = if let Some(p) = quoted.filter(|p| p.starts_with('\'') && p.ends_with('\'')) {
        command.replace(p, "__EXE__")
    } else {
        command.to_owned()
    };
    catalog(group).any(|s| s.replace("/CRG_REPOSITORY", &root.to_string_lossy()) == canonical)
}
fn owned(command: Option<&str>, group: &str, root: &Path, event: &str, matcher: &Value) -> bool {
    let Some(command) = command else { return false };
    if group == "claude"
        && event == "PreToolUse"
        && matcher == "Grep|Glob|Bash|Read"
        && known(command, "steering_commands", root)
    {
        return true;
    }
    match group {
        "claude" | "project" => known(command, "project_commands", root),
        "codex" => known(command, "codex_commands", root),
        "cursor" => std::env::home_dir().is_some_and(|home| {
            catalog("cursor_scripts")
                .any(|f| home.join(".cursor/hooks").join(f) == Path::new(command))
        }),
        "gemini" => catalog("gemini_scripts").any(|c| format!("bash .gemini/hooks/{c}") == command),
        _ => false,
    }
}
fn filtered(doc: &Node, group: &str, root: &Path) -> (Value, Vec<Range<usize>>) {
    let mut expected = doc.value.clone();
    let mut cuts = vec![];
    let Some(hooks) = doc.member("hooks").filter(|n| n.value.is_object()) else {
        return (expected, cuts);
    };
    let mut empty_events = vec![];
    for (ei, event) in hooks.children.iter().enumerate() {
        if !event.value.value.is_array() {
            continue;
        }
        let mut kept = vec![];
        let mut event_cuts = vec![];
        for (i, item) in event.value.children.iter().enumerate() {
            let n = &item.value;
            let matcher = &n.value["matcher"];
            let owns = |v: &Value| owned(v["command"].as_str(), group, root, &event.key, matcher);
            if owns(&n.value) {
                event.value.cut(i, &mut event_cuts);
                continue;
            }
            let mut val = n.value.clone();
            if let Some(sub) = n.member("hooks").filter(|n| n.value.is_array()) {
                let keep = sub
                    .children
                    .iter()
                    .filter(|c| !owns(&c.value.value))
                    .map(|c| c.value.value.clone())
                    .collect::<Vec<_>>();
                if keep.len() != sub.children.len() {
                    if keep.is_empty() {
                        event.value.cut(i, &mut event_cuts);
                        continue;
                    }
                    for (j, h) in sub.children.iter().enumerate() {
                        if owns(&h.value.value) {
                            sub.cut(j, &mut event_cuts)
                        }
                    }
                    val["hooks"] = keep.into();
                }
            }
            kept.push(val);
        }
        if !event_cuts.is_empty() {
            if kept.is_empty() {
                empty_events.push(ei);
                expected["hooks"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&event.key);
            } else {
                expected["hooks"][&event.key] = kept.into();
                cuts.extend(event_cuts)
            }
        }
    }
    if !empty_events.is_empty() && expected["hooks"].as_object().is_some_and(|m| m.is_empty()) {
        expected.as_object_mut().unwrap().remove("hooks");
        cuts.clear();
        doc.cut(
            doc.children.iter().position(|c| c.key == "hooks").unwrap(),
            &mut cuts,
        );
    } else {
        for i in empty_events {
            hooks.cut(i, &mut cuts)
        }
    }
    (expected, cuts)
}
pub(super) fn clean(r: &mut Report, path: &Path, base: &Path, group: &str, root: &Path) {
    let backup = path.with_extension("json.bak");
    if !backup.is_symlink()
        && let Some(raw) = r.read(&backup, base)
        && let Ok(doc) = jsonc::parse(&raw)
        && !report::empty(&doc.value)
    {
        let (mut rest, _) = filtered(&doc, group, root);
        if let Some(obj) = rest.as_object_mut() {
            for key in [
                "mcp_servers",
                "mcpServers",
                "servers",
                "mcp",
                "context_servers",
                "insert",
            ] {
                if let Some(val) = obj.get_mut(key) {
                    if let Some(map) = val.as_object_mut() {
                        map.remove("code-review-graph");
                    } else if let Some(items) = val.as_array_mut() {
                        items.retain(|v| v["name"] != "code-review-graph")
                    }
                }
            }
        }
        if report::empty(&rest) {
            r.delete(
                &backup,
                base,
                "remove backup holding only code-review-graph settings",
                false,
            )
        }
    }
    let Some(raw) = r.read(path, base) else {
        return;
    };
    let doc = match jsonc::parse(&raw) {
        Ok(d) => d,
        Err(e) => {
            r.skip(path, &format!("parse failed; left unchanged: {e}"));
            return;
        }
    };
    let (expected, cuts) = filtered(&doc, group, root);
    if !cuts.is_empty() {
        match jsonc::splice(&raw, cuts, &expected) {
            Ok(out) => r.write(
                path,
                base,
                &raw,
                &out,
                "remove code-review-graph hook entries",
            ),
            Err(e) => r.skip(path, &e.to_string()),
        }
    }
}
