mod artifacts;
mod bindings;
mod formats;
mod hooks;
mod manifest;
mod report;

use anyhow::{Context, Result};
use clap::Args;
use report::Report;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::LazyLock,
};

#[derive(Args)]
pub(in crate::cli) struct Options {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    all_repos: bool,
    #[arg(long)]
    keep_data: bool,
    #[arg(long)]
    keep_user_configs: bool,
    #[arg(long,default_value="all",value_parser=["all","codex","claude","claude-code","cursor","windsurf","zed","continue","opencode","mimo","antigravity","gemini-cli","qwen","kiro","qoder","copilot","copilot-cli","hermes","dsh","codebuddy"])]
    platform: String,
    #[arg(long)]
    dry_run: bool,
    #[arg(short = 'y', long)]
    yes: bool,
}
pub(super) fn catalog(key: &str) -> impl Iterator<Item = &'static str> {
    static DATA: LazyLock<Value> = LazyLock::new(|| {
        serde_json::from_str(include_str!("../../../../assets/uninstall/catalog.json")).unwrap()
    });
    DATA[key]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
}
fn data(r: &mut Report, root: &Path, relative: &str, dir: bool, keep: bool) {
    let p = root.join(relative);
    if keep {
        if p.symlink_metadata().is_ok() {
            r.skip(&p, "kept by --keep-data")
        }
    } else {
        r.delete(
            &p,
            root,
            if dir {
                "remove owned directory"
            } else {
                "remove owned file"
            },
            dir,
        )
    }
}
fn repository(r: &mut Report, root: &Path, platform: &str, keep_data: bool) {
    let files = manifest::repo_files(root);
    let only = manifest::untracked(root, &files);
    r.with_install_manifest(only.clone(), |r| {
        bindings::sweep(r, root, true, platform);
        if platform == "all" {
            data(r, root, ".code-review-graph", true, keep_data);
            for file in [
                ".code-review-graph.db",
                ".code-review-graph.db-wal",
                ".code-review-graph.db-shm",
            ] {
                data(r, root, file, false, keep_data)
            }
            for (file, kind) in [
                (".claude", "claude"),
                (".qoder", "project"),
                (".codebuddy", "project"),
                (".gemini", "gemini"),
            ] {
                hooks::clean(r, &root.join(file).join("settings.json"), root, kind, root)
            }
            for name in catalog("gemini_scripts") {
                r.delete(
                    &root.join(".gemini/hooks").join(name),
                    root,
                    "remove owned file",
                    false,
                )
            }
            for dir in [".claude", ".gemini", ".codebuddy"] {
                for slug in catalog("skills") {
                    r.skill(
                        &root.join(dir).join("skills").join(slug).join("SKILL.md"),
                        root,
                    )
                }
            }
            let mut names: Vec<_> = catalog("qoder").map(str::to_owned).collect();
            let src = root.join("skills");
            if !src.is_symlink()
                && let Ok(iter) = fs::read_dir(src)
            {
                for row in iter.flatten() {
                    if !row.path().is_symlink() && row.path().join("SKILL.md").is_file() {
                        let s = row.file_name().to_string_lossy().into_owned();
                        if !names.contains(&s) {
                            names.push(s)
                        }
                    }
                }
            }
            for name in names {
                r.skill(
                    &root.join(".qoder/skills").join(name).join("SKILL.md"),
                    root,
                )
            }
            artifacts::instructions(r, root);
            artifacts::git_hooks(r, root);
            artifacts::ignore(r, root);
        }
    });
    if !r.dry() {
        manifest::forget(r, root, files, &only)
    }
}
fn user(r: &mut Report, home: &Path, root: &Path, platform: &str, keep_data: bool) {
    bindings::sweep(r, home, false, platform);
    if platform != "all" {
        return;
    }
    data(r, home, ".code-review-graph", true, keep_data);
    let codex = super::platforms::configured_home("CODEX_HOME", home, ".codex");
    hooks::clean(r, &codex.join("hooks.json"), &codex, "codex", root);
    manifest::codex_skill(r, &codex);
    hooks::clean(r, &home.join(".cursor/hooks.json"), home, "cursor", root);
    for script in catalog("cursor_scripts") {
        r.delete(
            &home.join(".cursor/hooks").join(script),
            home,
            "remove owned file",
            false,
        )
    }
    let config = super::platforms::configured_home("XDG_CONFIG_HOME", home, ".config");
    let plugin = "opencode/plugins/crg-plugin.ts";
    r.delete(&config.join(plugin), &config, "remove owned file", false);
    if config != home.join(".config") {
        r.delete(
            &home.join(".config").join(plugin),
            home,
            "remove owned file",
            false,
        )
    }
    let hermes = super::platforms::configured_home("HERMES_HOME", home, ".hermes");
    for slug in catalog("skills") {
        r.skill(
            &hermes
                .join("skills/code-review-graph")
                .join(slug)
                .join("SKILL.md"),
            &hermes,
        )
    }
}
fn expand(path: &Path, home: &Path) -> PathBuf {
    path.strip_prefix("~")
        .map(|rel| home.join(rel))
        .unwrap_or_else(|_| path.to_owned())
}
fn sweep(opts: &Options, dry: bool) -> Result<Report> {
    let home = env::home_dir().context("missing home directory")?;
    let mut r = Report::new(dry);
    let mut requested = vec![opts.repo.clone().unwrap_or(env::current_dir()?)];
    if opts.all_repos {
        let path = home.join(".code-review-graph/registry.json");
        if let Some(raw) = r.read(&path, &home) {
            match serde_json::from_str::<Value>(&raw) {
                Ok(v) if v["repos"].is_array() => {
                    for entry in v["repos"].as_array().unwrap() {
                        if let Some(p) = entry["path"].as_str().filter(|p| !p.is_empty()) {
                            requested.push(p.into())
                        }
                        if let Some(d) = entry["data_dir"].as_str().filter(|d| !d.is_empty()) {
                            r.skip(
                                &expand(Path::new(d), &home),
                                "external data directory retained for safety",
                            )
                        }
                    }
                }
                Ok(_) => r.skip(&path, "registry has no valid repos array"),
                Err(e) => r.skip(&path, &format!("registry parse failed: {e}")),
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut roots = vec![];
    for path in requested {
        let p = std::path::absolute(expand(&path, &home))?;
        let p = p.canonicalize().unwrap_or(p);
        let home_boundary = home.canonicalize().unwrap_or(home.clone());
        if p.parent().is_none() || p == home_boundary {
            r.skip(&p, "refusing unsafe repository boundary");
            continue;
        }
        if !p.is_dir() {
            r.skip(&p, "repository directory is missing");
            continue;
        }
        let Some(root) = p
            .ancestors()
            .find(|v| v.join(".git").exists())
            .or_else(|| p.ancestors().find(|v| v.join(".svn").is_dir()))
        else {
            r.skip(&p, "not inside a Git or SVN repository");
            continue;
        };
        if root.parent().is_none() || root == home_boundary {
            r.skip(root, "refusing unsafe repository boundary");
            continue;
        }
        if seen.insert(root.to_owned()) {
            roots.push(root.to_owned())
        }
    }
    let platform = if opts.platform == "claude-code" {
        "claude"
    } else {
        &opts.platform
    };
    for root in &roots {
        repository(&mut r, root, platform, opts.keep_data)
    }
    if !opts.keep_user_configs {
        let sentinel = home.join(".crg-uninstall-repo-sentinel");
        user(
            &mut r,
            &home,
            roots.first().unwrap_or(&sentinel),
            platform,
            opts.keep_data,
        )
    }
    Ok(r)
}
pub(in crate::cli) fn run(opts: Options) -> Result<()> {
    let preview = sweep(&opts, true)?;
    let scope = opts.platform != "all";
    if scope {
        println!(
            "code-review-graph unbind ({}) — planned actions:",
            opts.platform
        )
    } else {
        println!("code-review-graph uninstall — planned actions:")
    }
    preview.print();
    if preview.counts() == (0, 0) {
        if preview.failed() {
            std::process::exit(1)
        }
        if scope {
            println!(
                "  (nothing to do — {} has no code-review-graph MCP registration)",
                opts.platform
            )
        } else {
            println!("  (nothing to do — no code-review-graph artifacts found)")
        }
        return Ok(());
    }
    if opts.dry_run {
        println!("\n[dry-run] No changes made.");
        if preview.failed() {
            std::process::exit(1)
        }
        return Ok(());
    }
    if !opts.yes {
        let yes = if io::stdin().is_terminal() {
            print!(
                "\nProceed with {}? [y/N] ",
                if scope { "unbind" } else { "uninstall" }
            );
            io::stdout().flush()?;
            let mut ans = String::new();
            io::stdin().read_line(&mut ans)?;
            matches!(ans.trim().to_ascii_lowercase().as_str(), "yes" | "y")
        } else {
            println!("No terminal: taking the default (no). Use --yes to go ahead.");
            false
        };
        if !yes {
            println!("Aborted.");
            return Ok(());
        }
    }
    let result = sweep(&opts, false)?;
    println!("\nApplied actions:");
    result.print();
    let (removed, edited) = result.counts();
    println!("Done. Removed {removed} path(s); edited {edited} shared file(s).");
    if result.failed() {
        std::process::exit(1)
    }
    Ok(())
}
