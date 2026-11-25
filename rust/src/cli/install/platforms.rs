use std::{
    env,
    path::{Path, PathBuf},
};

pub(super) fn configured_home(key: &str, home: &Path, default: &str) -> PathBuf {
    let value = env::var(key).unwrap_or_default();
    let text = value.trim();
    if text.is_empty() {
        return home.join(default);
    }
    if text == "~" {
        return home.to_owned();
    }
    let path = text
        .strip_prefix("~/")
        .map(|s| home.join(s))
        .unwrap_or_else(|| PathBuf::from(text));
    std::path::absolute(&path).unwrap_or(path)
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Form {
    Object,
    Array,
    Toml,
    Yaml,
    Patch,
}
pub(super) struct Binding<'a> {
    pub(super) owner: &'a str,
    pub(super) root: PathBuf,
    pub(super) path: &'a str,
    pub(super) key: &'a str,
    pub(super) form: Form,
}
pub(super) fn paths(base: &Path, repo: bool) -> Vec<Binding<'static>> {
    use Form::*;
    let mut specs = Vec::new();
    let mut add = |owner, root: PathBuf, path, key, form| {
        specs.push(Binding {
            owner,
            root,
            path,
            key,
            form,
        })
    };
    if repo {
        for (p, file, key) in [
            ("claude", ".mcp.json", "mcpServers"),
            ("cursor", ".cursor/mcp.json", "mcpServers"),
            ("opencode", "opencode.jsonc", "mcp"),
            ("opencode", "opencode.json", "mcp"),
            ("mimo", ".mimocode/mimocode.json", "mcp"),
            ("mimo", ".mimocode/mimocode.jsonc", "mcp"),
            ("gemini-cli", ".gemini/settings.json", "mcpServers"),
            ("kiro", ".kiro/settings/mcp.json", "mcpServers"),
            ("qoder", ".qoder/mcp.json", "mcpServers"),
            ("copilot", ".vscode/mcp.json", "servers"),
            ("codebuddy", ".mcp.json", "mcpServers"),
            ("legacy", ".opencode.json", "mcpServers"),
        ] {
            add(p, base.to_owned(), file, key, Object)
        }
    } else {
        let zed = if cfg!(target_os = "macos") {
            "Library/Application Support/Zed/settings.json"
        } else {
            ".config/zed/settings.json"
        };
        add(
            "codex",
            configured_home("CODEX_HOME", base, ".codex"),
            "config.toml",
            "mcp_servers",
            Toml,
        );
        add(
            "windsurf",
            base.to_owned(),
            ".codeium/windsurf/mcp_config.json",
            "mcpServers",
            Object,
        );
        add("zed", base.to_owned(), zed, "context_servers", Object);
        add(
            "continue",
            base.to_owned(),
            ".continue/config.json",
            "mcpServers",
            Array,
        );
        for (p, file, key) in [
            (
                "antigravity",
                ".gemini/antigravity/mcp_config.json",
                "mcpServers",
            ),
            ("qwen", ".qwen/settings.json", "mcpServers"),
            ("copilot-cli", ".copilot/mcp-config.json", "mcpServers"),
            ("copilot-cli", ".copilot/mcp-config.json", "servers"),
        ] {
            add(p, base.to_owned(), file, key, Object)
        }
        add(
            "hermes",
            configured_home("HERMES_HOME", base, ".hermes"),
            "config.yaml",
            "mcp_servers",
            Yaml,
        );
        add(
            "dsh",
            configured_home("DSH_HOME", base, ".dsh"),
            "cordis.patch.yml",
            "insert",
            Patch,
        );
        add(
            "legacy",
            base.to_owned(),
            ".cursor/mcp.json",
            "mcpServers",
            Object,
        );
    }
    specs
}

pub(super) const CLIENTS: [(&str, &str); 18] = [
    ("codex", "Codex"),
    ("claude", "Claude Code"),
    ("cursor", "Cursor"),
    ("windsurf", "Windsurf"),
    ("zed", "Zed"),
    ("continue", "Continue"),
    ("opencode", "OpenCode"),
    ("mimo", "MiMo Code"),
    ("antigravity", "Antigravity"),
    ("gemini-cli", "Gemini CLI"),
    ("qwen", "Qwen Code"),
    ("kiro", "Kiro"),
    ("qoder", "Qoder"),
    ("copilot", "GitHub Copilot"),
    ("copilot-cli", "GitHub Copilot CLI"),
    ("hermes", "Hermes Agent"),
    ("dsh", "DeepSeek Harness"),
    ("codebuddy", "CodeBuddy Code"),
];

fn on_path(name: &str) -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|p| {
        let file = p.join(name);
        let Ok(meta) = file.metadata() else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    })
}
fn has_var(key: &str) -> bool {
    env::var(key).is_ok_and(|v| !v.trim().is_empty())
}
pub(super) fn detected(name: &str, home: &Path, repo: &Path) -> bool {
    match name {
        "codex" => has_var("CODEX_HOME") || home.join(".codex").is_dir() || on_path("codex"),
        "claude" => {
            has_var("CLAUDECODE")
                || home.join(".claude").exists()
                || home.join(".claude.json").exists()
                || (has_var("CLAUDE_CONFIG_DIR")
                    && configured_home("CLAUDE_CONFIG_DIR", home, ".claude").exists())
                || on_path("claude")
        }
        "cursor" => home.join(".cursor").exists(),
        "windsurf" => home.join(".codeium/windsurf").exists(),
        "zed" => paths(home, false)
            .iter()
            .find(|s| s.owner == "zed")
            .and_then(|s| s.root.join(s.path).parent().map(Path::to_owned))
            .is_some_and(|p| p.exists()),
        "continue" => home.join(".continue").exists(),
        "opencode" => {
            on_path("opencode")
                || home.join(".opencode").exists()
                || configured_home("XDG_CONFIG_HOME", home, ".config")
                    .join("opencode")
                    .exists()
                || configured_home("XDG_DATA_HOME", home, ".local/share")
                    .join("opencode")
                    .exists()
        }
        "mimo" => {
            on_path("mimo")
                || env::var("MIMOCODE_HOME")
                    .ok()
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join("config"))
                    .unwrap_or_else(|| {
                        configured_home("XDG_CONFIG_HOME", home, ".config").join("mimocode")
                    })
                    .exists()
        }
        "gemini-cli" => on_path("gemini") || home.join(".gemini").exists(),
        "antigravity" => home.join(".gemini/antigravity").exists(),
        "qwen" => home.join(".qwen").exists(),
        "kiro" => home.join(".kiro").exists() || repo.join(".kiro").is_dir(),
        "qoder" => {
            on_path("qoder")
                || on_path("qodercli")
                || home.join(".qoder").exists()
                || home.join("Library/Application Support/Qoder").exists()
                || [
                    home.join("Applications/Qoder.app"),
                    PathBuf::from("/Applications/Qoder.app"),
                ]
                .iter()
                .any(|p| cfg!(target_os = "macos") && p.exists())
                || configured_home("XDG_CONFIG_HOME", home, ".config")
                    .join("Qoder")
                    .exists()
        }
        "copilot" => copilot(home),
        "copilot-cli" => home.join(".copilot").exists(),
        "hermes" => configured_home("HERMES_HOME", home, ".hermes").exists(),
        "dsh" => configured_home("DSH_HOME", home, ".dsh").exists(),
        "codebuddy" => on_path("codebuddy") || home.join(".codebuddy").exists(),
        _ => false,
    }
}
fn copilot(home: &Path) -> bool {
    let mut dirs = vec![
        home.join(".vscode/extensions"),
        home.join(".vscode-insiders/extensions"),
    ];
    if cfg!(target_os = "macos") {
        for d in [
            Path::new("/Applications").to_owned(),
            home.join("Applications"),
        ] {
            for app in [
                "Visual Studio Code.app",
                "Visual Studio Code - Insiders.app",
            ] {
                dirs.push(d.join(app).join("Contents/Resources/app/extensions"))
            }
        }
    }
    for name in ["code", "code-insiders"] {
        if let Some(path) = env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join(name))
            .find(|p| p.is_file())
            .and_then(|p| p.canonicalize().ok())
        {
            for parent in path.ancestors().skip(1).take(6) {
                for suffix in [
                    "extensions",
                    "resources/app/extensions",
                    "Resources/app/extensions",
                ] {
                    dirs.push(parent.join(suffix))
                }
            }
        }
    }
    dirs.iter()
        .filter_map(|p| std::fs::read_dir(p).ok())
        .flatten()
        .flatten()
        .any(|it| {
            let name = it.file_name().to_string_lossy().to_lowercase();
            name.starts_with("github.copilot-")
                || (name.contains("copilot")
                    && std::fs::read_to_string(it.path().join("package.json"))
                        .ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .is_some_and(|v| {
                            v["publisher"]
                                .as_str()
                                .is_some_and(|s| s.eq_ignore_ascii_case("github"))
                                && v["name"].as_str().is_some_and(|s| {
                                    matches!(s.to_lowercase().as_str(), "copilot" | "copilot-chat")
                                })
                        }))
        })
}

pub(super) fn installation<'a>(
    available: &'a [Binding<'static>],
    owner: &str,
) -> Option<&'a Binding<'static>> {
    let choices: Vec<_> = available.iter().filter(|s| s.owner == owner).collect();
    if owner == "mimo" {
        return choices
            .iter()
            .find(|s| s.path.ends_with(".jsonc") && s.root.join(s.path).exists())
            .copied()
            .or_else(|| choices.first().copied());
    }
    if owner == "opencode" {
        return choices
            .iter()
            .find(|s| s.root.join(s.path).exists())
            .copied()
            .or_else(|| choices.first().copied());
    }
    choices.first().copied()
}
