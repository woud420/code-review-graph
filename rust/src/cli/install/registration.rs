use super::{
    config, jsonc,
    plan::Plan,
    platforms::{Binding, Form},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;
const NAME: &str = "code-review-graph";

#[derive(PartialEq)]
enum Outcome {
    Changed(String),
    Existing,
    Manual,
    Unsafe,
}
fn entry(platform: &str, root: &Path, binary: &Path) -> Value {
    if matches!(platform, "opencode" | "mimo") {
        return json!({"type":"local","command":[binary,"serve","--repo",root]});
    }
    let mut value = json!({"command":binary,"args":["serve"]});
    if matches!(platform, "hermes" | "dsh") {
        return value;
    }
    value["cwd"] = json!(root);
    if matches!(
        platform,
        "codex"
            | "claude"
            | "codebuddy"
            | "cursor"
            | "continue"
            | "qwen"
            | "kiro"
            | "qoder"
            | "copilot"
    ) {
        value["type"] = "stdio".into()
    }
    if platform == "copilot-cli" {
        value["type"] = "local".into();
        value["tools"] = json!(["*"])
    }
    value
}
fn json_entry(raw: &str, s: &Binding<'_>, desired: &Value, binary: &Path) -> Result<Outcome> {
    let empty = jsonc::only_comments(raw);
    let doc = jsonc::parse(if empty { "{}" } else { raw })?;
    let mut expected = doc.value.clone();
    ensure!(expected.is_object(), "expected top-level object");
    let commented = jsonc::comments(raw);
    let mut cuts = vec![];
    let mut migrated = false;
    if s.owner == "copilot-cli"
        && let Some(old) = doc.member("servers")
        && let Some(i) = old.children.iter().position(|c| c.key == NAME)
    {
        if old.children.len() == 1 {
            doc.cut(
                doc.children
                    .iter()
                    .position(|c| c.key == "servers")
                    .unwrap(),
                &mut cuts,
            );
            expected.as_object_mut().unwrap().remove("servers");
        } else {
            old.cut(i, &mut cuts);
            expected["servers"]
                .as_object_mut()
                .context("invalid legacy servers")?
                .remove(NAME);
        }
        migrated = true;
    }
    let observed = if migrated {
        jsonc::splice(raw, cuts, &expected)?
    } else {
        raw.to_owned()
    };
    let mut new_entry = desired.clone();
    let key = s.key;
    let arr = s.form == Form::Array;
    if arr {
        new_entry
            .as_object_mut()
            .unwrap()
            .shift_insert(0, "name".into(), NAME.into());
        let group = expected
            .as_object_mut()
            .unwrap()
            .entry(key)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("expected an array")?;
        let same: Vec<_> = group.iter().filter(|v| v["name"] == NAME).collect();
        if same.iter().any(|v| !config::is_ours(v, binary)) {
            return Ok(Outcome::Manual);
        }
        if same.len() == 1 && same[0] == &new_entry {
            return Ok(Outcome::Existing);
        }
        if commented && !empty {
            return Ok(Outcome::Unsafe);
        }
        if let Some(i) = group.iter().position(|v| v["name"] == NAME) {
            group[i] = new_entry;
            let mut pos = 0;
            group.retain(|v| {
                let keep = pos <= i || v["name"] != NAME;
                pos += 1;
                keep
            })
        } else {
            group.push(new_entry)
        }
    } else {
        let map = expected
            .as_object_mut()
            .unwrap()
            .entry(key)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("expected an object")?;
        if let Some(current) = map.get(NAME) {
            if !config::is_ours(current, binary) {
                return Ok(if migrated {
                    Outcome::Changed(observed)
                } else {
                    Outcome::Manual
                });
            }
            if current == desired && !migrated {
                return Ok(Outcome::Existing);
            }
        }
        map.insert(NAME.into(), desired.clone());
    }
    let next = if !commented {
        serde_json::to_string_pretty(&expected)? + "\n"
    } else if empty {
        format!(
            "{}\n{}\n",
            raw.trim_end_matches('\n'),
            serde_json::to_string_pretty(&expected)?
        )
    } else {
        let parsed = jsonc::parse(&observed)?;
        if let Some(container) = parsed.member(key) {
            jsonc::set_member(&observed, container, NAME, desired)?
        } else {
            jsonc::set_member(&observed, &parsed, key, &json!({NAME:desired}))?
        }
    };
    ensure!(
        jsonc::parse(&next)?.value == expected,
        "edited value differs"
    );
    Ok(Outcome::Changed(next))
}

pub(super) fn bind(
    plan: &mut Plan,
    s: &Binding<'_>,
    root: &Path,
    home: &Path,
    binary: &Path,
    label: &str,
    dry: bool,
) -> Result<bool> {
    let path = s.root.join(s.path);
    let shown = if let Ok(rel) = path.strip_prefix(root) {
        rel.display().to_string()
    } else if let Ok(rel) = path.strip_prefix(home) {
        format!("~/{}", rel.display())
    } else {
        path.display().to_string()
    };
    let mut configured = true;
    ensure!(
        !s.root.is_symlink(),
        "refusing to edit symlink {}",
        s.root.display()
    );
    plan.edit(&s.root,s.path,shown.clone(),false,|raw|{
        let desired=entry(s.owner,root,binary);
        let outcome=match s.form{
            Form::Array|Form::Object=>json_entry(raw,s,&desired,binary),
            Form::Toml=>(||{
                let parsed:Value=toml::from_str(raw)?;
                if let Some(existing)=parsed.get(s.key).and_then(|group|group.get(NAME)){
                    if existing==&desired{return Ok(Outcome::Existing)}
                    if !config::is_ours(existing,binary){return Ok(Outcome::Manual)}
                }
                Ok(Outcome::Changed(config::codex_config(raw,root,binary,false)?))
            })(),
            Form::Yaml|Form::Patch=>yaml::set(raw,s.form,&desired,binary),
        };
        match outcome {
            Ok(Outcome::Changed(text))=>{if dry {println!("  [dry-run] {label}: would write {shown}")}else{println!("  {label}: configured {shown}")}Ok(text)}
            Ok(Outcome::Existing)=>{println!("  {label}: already configured in {shown}");Ok(raw.into())}
            Ok(Outcome::Manual)=>{println!("  {label}: {} holds a hand-written 'code-review-graph' entry — leaving it as it is.",path.display());configured=false;Ok(raw.into())}
            Ok(Outcome::Unsafe)|Err(_)=>{println!("  {label}: {} cannot be edited while preserving existing settings — left unchanged. Please add the MCP config manually.",path.display());configured=false;Ok(raw.into())}
        }
    })?;
    Ok(configured)
}
mod yaml;
