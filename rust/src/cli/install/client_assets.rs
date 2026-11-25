use super::{config, instruction, jsonc, managed_file, plan::Plan, platforms::configured_home};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::LazyLock,
    time::{Duration, Instant},
};
static TEMPLATES: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../assets/install/platforms.json")).unwrap()
});

const STANDARD: [(&str, &str); 4] = [
    (
        "explore-codebase",
        include_str!("../../../assets/install/claude/skills/explore-codebase/SKILL.md"),
    ),
    (
        "review-changes",
        include_str!("../../../assets/install/claude/skills/review-changes/SKILL.md"),
    ),
    (
        "debug-issue",
        include_str!("../../../assets/install/claude/skills/debug-issue/SKILL.md"),
    ),
    (
        "refactor-safely",
        include_str!("../../../assets/install/claude/skills/refactor-safely/SKILL.md"),
    ),
];
type Clients<'a> = &'a [(&'a str, &'a str)];
fn chosen(clients: Clients<'_>, name: &str) -> bool {
    clients.iter().any(|(c, _)| *c == name)
}

pub(super) fn skills(
    plan: &mut Plan,
    root: &Path,
    home: &Path,
    clients: Clients<'_>,
) -> Result<()> {
    let hermes = configured_home("HERMES_HOME", home, ".hermes");
    for (platform, base, folder) in [
        ("claude", root, ".claude/skills"),
        ("gemini-cli", root, ".gemini/skills"),
        ("codebuddy", root, ".codebuddy/skills"),
        ("qoder", root, ".qoder/skills"),
        ("hermes", &hermes, "skills/code-review-graph"),
    ] {
        if !chosen(clients, platform) {
            continue;
        }
        for (slug, body) in STANDARD {
            let path = format!("{folder}/{slug}/SKILL.md");
            managed_file(plan, base, &path, path.clone(), body)?
        }
    }
    if chosen(clients, "qoder") {
        for (path, body) in TEMPLATES
            .iter()
            .filter_map(|(k, v)| k.strip_prefix("repo/.qoder/skills/").map(|p| (p, v)))
        {
            let file = format!(".qoder/skills/{path}");
            managed_file(plan, root, &file, file.clone(), body)?
        }
    }
    Ok(())
}
pub(super) fn instructions(plan: &mut Plan, root: &Path, clients: Clients<'_>) -> Result<()> {
    let standard = include_str!("../../../assets/install/claude/CLAUDE.md");
    let specs: &[(&str, &[&str])] = &[
        ("CLAUDE.md", &["claude"]),
        (
            "AGENTS.md",
            &[
                "cursor",
                "opencode",
                "mimo",
                "antigravity",
                "codex",
                "hermes",
                "dsh",
            ],
        ),
        ("GEMINI.md", &["antigravity", "gemini-cli"]),
        (".cursorrules", &["cursor"]),
        (".windsurfrules", &["windsurf"]),
        ("QODER.md", &["qoder"]),
        (".kiro/steering/code-review-graph.md", &["kiro"]),
        (
            ".github/instructions/code-review-graph.instructions.md",
            &["copilot", "copilot-cli"],
        ),
        ("CODEBUDDY.md", &["codebuddy"]),
    ];
    for (path, owners) in specs {
        if owners.iter().any(|p| chosen(clients, p)) {
            let template = TEMPLATES
                .get(&format!("repo/{path}"))
                .map_or(standard, String::as_str);
            instruction(plan, root, path, template)?;
        }
    }
    if chosen(clients, "copilot") || chosen(clients, "copilot-cli") {
        let legacy = ".github/code-review-graph.instruction.md";
        if let Some(old) = plan.current(root, legacy)? {
            let mut text = old.clone();
            for section in super::uninstall::catalog("instructions") {
                text = text.replace(section, "")
            }
            if text != old && !text.contains("<!-- code-review-graph MCP tools -->") {
                if text.trim().is_empty() {
                    plan.remove_known(root, legacy, legacy.into(), |existing| existing == old)?;
                } else {
                    plan.edit(root, legacy, legacy.into(), false, |_| {
                        Ok(format!("{}\n", text.trim_end()))
                    })?;
                }
            }
        }
    }
    Ok(())
}
fn merge_flat(before: &str, template: &str) -> Result<String> {
    let mut doc: Value = if before.is_empty() {
        json!({})
    } else {
        serde_json::from_str(before)?
    };
    let desired: Value = serde_json::from_str(template)?;
    let root = doc.as_object_mut().context("invalid hooks document")?;
    root.entry("version").or_insert(json!(1));
    let hooks = root
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
        .context("invalid hooks object")?;
    for (event, new) in desired["hooks"]
        .as_object()
        .context("invalid Cursor hooks")?
    {
        let entries = hooks
            .entry(event)
            .or_insert(json!([]))
            .as_array_mut()
            .context("invalid event")?;
        for desired in new.as_array().context("invalid Cursor group")? {
            if !entries.iter().any(|old| old == desired) {
                if let Some(i) = entries
                    .iter()
                    .position(|old| old["command"] == desired["command"])
                {
                    entries[i] = desired.clone()
                } else {
                    entries.push(desired.clone())
                }
            }
        }
    }
    Ok(serde_json::to_string_pretty(&doc)? + "\n")
}

pub(super) fn hook_settings(
    plan: &mut Plan,
    base: &Path,
    file: &str,
    template: &str,
    binary: &Path,
    flat: bool,
) -> Result<bool> {
    let original = plan.current(base, file)?.unwrap_or_default();
    let empty = jsonc::only_comments(&original);
    let parsed = if empty {
        json!({})
    } else {
        let Ok(node) = jsonc::parse(&original) else {
            return Ok(false);
        };
        node.value
    };
    let strict = serde_json::to_string(&parsed)?;
    let rendered = if flat {
        merge_flat(&strict, template)
    } else {
        config::hooks(&strict, template, binary)
    };
    let Ok(rendered) = rendered else {
        return Ok(false);
    };
    let desired: Value = serde_json::from_str(&rendered)?;
    if desired == parsed {
        return Ok(true);
    }
    let next = if !jsonc::comments(&original) {
        rendered
    } else if empty {
        format!("{}\n{rendered}", original.trim_end_matches('\n'))
    } else {
        let mut text = original.clone();
        for key in ["version", "hooks"] {
            if desired.get(key) == parsed.get(key) {
                continue;
            }
            let node = jsonc::parse(&text)?;
            let Ok(new) = jsonc::set_member(&text, &node, key, &desired[key]) else {
                return Ok(false);
            };
            text = new;
        }
        if jsonc::parse(&text)?.value != desired {
            return Ok(false);
        }
        text
    };
    if !original.trim().is_empty() {
        plan.backup(base, file, &original)?;
    }
    plan.edit(base, file, file.into(), false, |_| Ok(next))?;
    Ok(true)
}
fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn major() -> u32 {
    static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?:^|\s)v?(\d+)\.\d+(?:\.\d+)?(?:[-+][0-9A-Za-z.+-]*)?(?:\s|$)")
            .unwrap()
    });
    let Ok(temp) = tempfile::tempfile() else {
        return 2;
    };
    let (Ok(stdout), Ok(stderr)) = (temp.try_clone(), temp.try_clone()) else {
        return 2;
    };
    let Ok(mut child) = Command::new("opencode")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
    else {
        return 2;
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return 2;
            }
        }
    }
    use std::io::Seek;
    let mut temp = temp;
    let _ = temp.rewind();
    let mut output = String::new();
    let _ = temp.take(65536).read_to_string(&mut output);
    VERSION
        .captures(&output)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(2)
}
pub(super) fn hooks(
    plan: &mut Plan,
    root: &Path,
    home: &Path,
    binary: &Path,
    clients: Clients<'_>,
) -> Result<()> {
    for (platform, base, file, template, flat) in [
        (
            "qoder",
            root,
            ".qoder/settings.json",
            include_str!("../../../assets/install/claude/settings.json"),
            false,
        ),
        (
            "codebuddy",
            root,
            ".codebuddy/settings.json",
            &TEMPLATES["repo/.codebuddy/settings.json"],
            false,
        ),
        (
            "gemini-cli",
            root,
            ".gemini/settings.json",
            &TEMPLATES["repo/.gemini/settings.json"],
            false,
        ),
        (
            "cursor",
            home,
            ".cursor/hooks.json",
            &TEMPLATES["home/.cursor/hooks.json"],
            true,
        ),
    ] {
        if !chosen(clients, platform) {
            continue;
        }
        let escaped_home = serde_json::to_string(home)?;
        let template = template.replace("__CRG_HOME__", &escaped_home[1..escaped_home.len() - 1]);
        if !hook_settings(plan, base, file, &template, binary, flat)? {
            continue;
        }
        let prefix = if platform == "gemini-cli" {
            "repo/.gemini/hooks/"
        } else if platform == "cursor" {
            "home/.cursor/hooks/"
        } else {
            continue;
        };
        for (key, template) in TEMPLATES.iter().filter(|(k, _)| k.starts_with(prefix)) {
            let (_, path) = key.split_once('/').unwrap();
            let rendered = template
                .replace("__CRG_SH__", &shell_quote(binary))
                .replace("\"__CRG_REPO__\"", &shell_quote(root));
            plan.edit(base, path, path.into(), true, |_| Ok(rendered))?;
        }
    }
    if chosen(clients, "opencode") {
        let version = major();
        if matches!(version, 1 | 2) {
            let config = configured_home("XDG_CONFIG_HOME", home, ".config");
            let path = "opencode/plugins/crg-plugin.ts";
            let body = TEMPLATES[&format!("plugin/v{version}")]
                .replace("\"__CRG_NATIVE__\"", &serde_json::to_string(binary)?);
            plan.edit(&config, path, path.into(), false, |_| Ok(body))?;
        }
    }
    Ok(())
}
