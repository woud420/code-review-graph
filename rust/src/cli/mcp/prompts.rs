use anyhow::{Context, Result, bail, ensure};
use rmcp::model::{GetPromptResult, Prompt};
use serde_json::{Map, Value, json};

pub fn list() -> Result<Vec<Prompt>> {
    Ok(serde_json::from_str(include_str!(
        "../../../assets/cli/prompts/descriptors.json"
    ))?)
}

pub fn get(name: &str, arguments: &Map<String, Value>) -> Result<GetPromptResult> {
    let descriptor = list()?
        .into_iter()
        .find(|p| p.name == name)
        .context("unknown prompt")?;
    let (template, key, default) = match name {
        "review_changes" => (
            include_str!("../../../assets/cli/prompts/review_changes.md"),
            Some("base"),
            "HEAD~1",
        ),
        "architecture_map" => (
            include_str!("../../../assets/cli/prompts/architecture_map.md"),
            None,
            "",
        ),
        "debug_issue" => (
            include_str!("../../../assets/cli/prompts/debug_issue.md"),
            Some("description"),
            "<description>",
        ),
        "onboard_developer" => (
            include_str!("../../../assets/cli/prompts/onboard_developer.md"),
            None,
            "",
        ),
        "pre_merge_check" => (
            include_str!("../../../assets/cli/prompts/pre_merge_check.md"),
            Some("base"),
            "HEAD~1",
        ),
        _ => bail!("unknown prompt {name}"),
    };
    ensure!(
        arguments.keys().all(|k| Some(k.as_str()) == key),
        "unexpected prompt argument"
    );
    let mut value = match key.and_then(|k| arguments.get(k)) {
        Some(v) => v.as_str().context("expected a string prompt argument")?,
        None => default,
    };
    if name == "debug_issue" && value.is_empty() {
        value = default
    }
    let body = template
        .strip_suffix('\n')
        .unwrap_or(template)
        .replace("{{argument}}", value);
    let text = format!(
        "{}\n{body}",
        include_str!("../../../assets/cli/prompts/preamble.md")
    );
    Ok(serde_json::from_value(json!({
        "description": descriptor.description,
        "messages":[{"role":"user", "content":{"type":"text","text":text}}]
    }))?)
}
