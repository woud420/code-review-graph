mod client_assets;
mod config;
mod ignore_block;
mod jsonc;
mod plan;
mod platforms;
mod registration;
pub(super) mod uninstall;
use super::changes;
use anyhow::{Context, Result, ensure};
use clap::Args;
use plan::Plan;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    env,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

macro_rules! asset {
    ($p:literal) => {
        include_str!(concat!("../../../assets/install/", $p))
    };
}

#[derive(Args)]
pub(super) struct Options {
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long,default_value="all",value_parser=["all","codex","claude","claude-code","cursor","windsurf","zed","continue","opencode","mimo","antigravity","gemini-cli","qwen","kiro","qoder","copilot","copilot-cli","hermes","dsh","codebuddy"])]
    platform: String,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    no_hooks: bool,
    #[arg(long)]
    no_skills: bool,
    #[arg(long)]
    no_instructions: bool,
    #[arg(long, overrides_with = "no_steering")]
    steering: bool,
    #[arg(long, overrides_with = "steering")]
    no_steering: bool,
    #[arg(long)]
    gitignore: bool,
    #[arg(short = 'y', long)]
    yes: bool,
    #[arg(long, hide = true)]
    skills: bool,
    #[arg(long, hide = true)]
    hooks: bool,
    #[arg(long, hide = true)]
    all: bool,
}

fn codex_home() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
        return super::absolute(Path::new(&dir));
    }
    Ok(env::home_dir().context("missing home")?.join(".codex"))
}

fn confirmed(prompt: &str, default: bool) -> Result<bool> {
    if !io::stdin().is_terminal() {
        return Ok(default);
    }
    print!("{prompt} ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(match answer.trim().to_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}

fn managed_file(plan: &mut Plan, base: &Path, path: &str, shown: String, body: &str) -> Result<()> {
    plan.edit(base, path, shown, false, |old| {
        ensure!(
            old.is_empty() || old == body,
            "{} already contains a different file",
            base.join(path).display()
        );
        Ok(body.into())
    })
}

fn instruction(plan: &mut Plan, root: &Path, path: &str, section: &str) -> Result<()> {
    let marker = "<!-- code-review-graph MCP tools -->";
    plan.edit(root, path, path.into(), false, |before| {
        if before.contains(marker) {
            let mut stale = uninstall::catalog("instructions")
                .filter(|known| *known != section && before.contains(known));
            let Some(anchor) = stale.next() else {
                return Ok(before.into());
            };
            let (left, right) = before.split_once(anchor).unwrap();
            let (mut left, mut right) = (left.to_owned(), right.to_owned());
            for old in stale {
                left = left.replace(old, "");
                right = right.replace(old, "");
            }
            if left.contains(section) || right.contains(section) {
                return Ok(left + &right);
            }
            return Ok(left + section + &right);
        }
        let prefix = if before.is_empty() {
            ""
        } else if before.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        Ok(format!("{before}{prefix}{section}"))
    })
}

fn git_hooks(plan: &mut Plan, root: &Path, binary: &Path) -> Result<()> {
    let Ok((true, path)) = changes::git(root, &["rev-parse", "--git-path", "hooks"]) else {
        return Ok(());
    };
    let hooks = root.join(String::from_utf8(path)?.trim());
    let hooks = std::path::absolute(hooks)?;
    let quoted = format!("'{}'", binary.to_string_lossy().replace('\'', "'\\''"));
    for (name, body) in [
        ("pre-commit", asset!("git-hooks/pre-commit")),
        ("post-checkout", asset!("git-hooks/post-checkout")),
        ("post-merge", asset!("git-hooks/post-merge")),
        ("post-rewrite", asset!("git-hooks/post-rewrite")),
    ] {
        let display = hooks.join(name);
        let shown = display
            .strip_prefix(root)
            .unwrap_or(&display)
            .display()
            .to_string();
        plan.edit(&hooks, name, shown, true, |before| {
            let desired = body.replace("__CRG_SH__", &quoted);
            if before.is_empty() || before == desired {
                return Ok(desired);
            }
            let block = desired
                .lines()
                .nth(1)
                .context("hook template has no marker")?;
            let end = desired
                .lines()
                .last()
                .context("hook template has no end marker")?;
            if let Some(i) = before.find(block) {
                let j = before[i..]
                    .find(end)
                    .context("incomplete existing CRG hook block")?
                    + i
                    + end.len();
                let start = desired.find(block).unwrap();
                return Ok(format!(
                    "{}{}{}",
                    &before[..i],
                    desired[start..].trim_end(),
                    &before[j..]
                ));
            }
            let shebang = before.lines().next().unwrap_or("");
            if !(shebang.starts_with("#!")
                && (shebang.ends_with("/sh")
                    || shebang.ends_with("/bash")
                    || shebang.ends_with(" sh")
                    || shebang.ends_with(" bash")))
            {
                return Ok(before.into());
            }
            Ok(format!(
                "{}\n{}\n",
                before.trim_end(),
                desired.lines().skip(1).collect::<Vec<_>>().join("\n")
            ))
        })?;
    }
    Ok(())
}

pub(super) fn run(opts: Options) -> Result<()> {
    let root = super::root(opts.repo.as_deref(), false)?;
    ensure!(root.is_dir(), "repository directory does not exist");
    let binary = env::current_exe()?;
    let codex = codex_home()?;
    let home = env::home_dir().context("missing home")?;
    let target = if opts.platform == "claude-code" {
        "claude"
    } else {
        &opts.platform
    };
    let clients: Vec<_> = platforms::CLIENTS
        .into_iter()
        .filter(|(name, _)| {
            name == &target || (target == "all" && platforms::detected(name, &home, &root))
        })
        .collect();
    let claude = clients.iter().any(|(name, _)| *name == "claude");
    let use_codex = clients.iter().any(|(name, _)| *name == "codex");
    let mut plan = Plan::default();
    let mut configured = Vec::new();
    println!("Installing MCP server config...");
    let mut paths = platforms::paths(&root, true);
    paths.extend(platforms::paths(&home, false));
    for (client, label) in &clients {
        if *client == "codebuddy" && claude {
            continue;
        }
        if let Some(binding) = platforms::installation(&paths, client)
            && registration::bind(
                &mut plan,
                binding,
                &root,
                &home,
                &binary,
                label,
                opts.dry_run,
            )?
        {
            configured.push(*label);
            if *client == "claude" && clients.iter().any(|(p, _)| *p == "codebuddy") {
                configured.push("CodeBuddy Code");
            }
        }
    }
    if configured.is_empty() {
        println!("No platforms detected.");
    } else if opts.dry_run {
        println!(
            "\n[dry-run] Would configure {} platform(s): {}",
            configured.len(),
            configured.join(", ")
        );
    }
    plan.edit(&root, ".gitignore", ".gitignore".into(), false, |before| {
        if before
            .lines()
            .map(str::trim)
            .any(|l| l == ".code-review-graph" || l.starts_with(".code-review-graph/"))
        {
            return Ok(before.into());
        }
        let prefix = if before.is_empty() || before.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        Ok(format!(
            "{before}{prefix}# Added by code-review-graph\n.code-review-graph/\n"
        ))
    })?;
    if !opts.no_skills {
        client_assets::skills(&mut plan, &root, &home, &clients)?;
    }
    if !opts.no_skills && use_codex {
        let skill = codex.join("skills/code-review-graph");
        let mut hashes = serde_json::Map::new();
        for (n, body, upstream_hash) in [
            (
                "SKILL.md",
                asset!("codex/skills/code-review-graph/SKILL.md"),
                "a3734b1b4990784bce803b3a37bd37fca5f2e8b3312ced34ff42de00b89abaae",
            ),
            (
                "agents/openai.yaml",
                asset!("codex/skills/code-review-graph/agents/openai.yaml"),
                "dd1ae3b88cb76e1c45dc77336aab6d4d24015c93b8e15730cf619adc320afafd",
            ),
        ] {
            hashes.insert(n.into(), hex::encode(Sha256::digest(body)).into());
            plan.edit(
                &skill,
                n,
                format!("~/.codex/skills/code-review-graph/{n}"),
                false,
                |old| {
                    ensure!(
                        old.is_empty()
                            || old == body
                            || hex::encode(Sha256::digest(old)) == upstream_hash,
                        "existing Codex skill file has local changes: {}",
                        skill.join(n).display()
                    );
                    Ok(body.into())
                },
            )?;
        }
        let legacy = "scripts/crg_readonly.py";
        plan.remove_known(
            &skill,
            legacy,
            format!("~/.codex/skills/code-review-graph/{legacy}"),
            |s| {
                hex::encode(Sha256::digest(s))
                    == "510553593e702d982836182b72a2df52def714ece9168a77b495818d834c6ec3"
            },
        )?;
        let manifest = ".code-review-graph-managed.json";
        plan.edit(
            &skill,
            manifest,
            format!("~/.codex/skills/code-review-graph/{manifest}"),
            false,
            |_| {
                Ok(format!(
                    "{}\n",
                    serde_json::to_string_pretty(&json!({"version":1,"files":hashes}))?
                ))
            },
        )?;
    }
    if !opts.no_hooks {
        if claude {
            client_assets::hook_settings(
                &mut plan,
                &root,
                ".claude/settings.json",
                asset!("claude/settings.json"),
                &binary,
                false,
            )?;
        }
        if use_codex {
            client_assets::hook_settings(
                &mut plan,
                &codex,
                "hooks.json",
                asset!("codex/hooks.json"),
                &binary,
                false,
            )?;
        }
        client_assets::hooks(&mut plan, &root, &home, &binary, &clients)?;
        if matches!(target, "all" | "claude" | "qoder" | "codex") {
            git_hooks(&mut plan, &root, &binary)?;
        }
    }
    if opts.steering || opts.no_steering {
        let prefix = if opts.dry_run { "[dry-run] " } else { "" };
        if !matches!(opts.platform.as_str(), "all" | "claude" | "claude-code") {
            println!("{prefix}Skipped the steering hook: it is only for Claude Code.");
        } else if opts.steering && opts.no_hooks {
            println!("{prefix}Skipped the steering hook: --no-hooks was given.");
        } else {
            let path = ".claude/settings.json";
            let mut changed = false;
            plan.edit(&root, path, path.into(), false, |old| {
                let settings = if opts.steering {
                    config::hooks(old, asset!("claude/steering.json"), &binary)?
                } else {
                    config::without_steering(old, asset!("claude/steering.json"))?
                };
                changed = settings != old;
                Ok(settings)
            })?;
            if opts.no_steering {
                println!(
                    "{prefix}{}",
                    if !changed {
                        "No steering hook to remove."
                    } else if opts.dry_run {
                        "Would remove the steering hook."
                    } else {
                        "Removed the steering hook."
                    }
                );
            } else if changed {
                let file = root.join(path);
                if opts.dry_run {
                    println!(
                        "[dry-run] Would add the steering hook to {}.",
                        file.display()
                    );
                } else {
                    println!(
                        "Installed the steering hook in {}. Remove it with --no-steering.",
                        file.display()
                    );
                }
            }
        }
    }
    if !opts.no_instructions
        && (opts.dry_run
            || opts.yes
            || confirmed("Inject graph instructions into the repository?", true)?)
    {
        client_assets::instructions(&mut plan, &root, &clients)?;
    }
    let new: BTreeSet<_> = plan
        .0
        .iter()
        .filter(|c| c.after.is_some())
        .filter_map(|c| c.path.strip_prefix(&root).ok())
        .filter(|p| !p.starts_with(".git") && !p.starts_with(".code-review-graph"))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if !new.is_empty() {
        if !root.join(".code-review-graph").exists() {
            let ignore = ".code-review-graph/.gitignore";
            plan.edit(&root,ignore,ignore.into(),false,|_| Ok("# Auto-generated by code-review-graph — do not commit database files.\n# The graph.db contains absolute paths and code structure metadata.\n*\n".into()))?;
        }
        let path = ".code-review-graph/install-manifest.json";
        plan.edit(&root, path, path.into(), false, |s| {
            let mut files = new;
            if !s.is_empty() {
                let manifest: Value = serde_json::from_str(s)?;
                ensure!(manifest["version"] == 1, "unsupported install manifest");
                files.extend(
                    manifest["files"]
                        .as_array()
                        .context("invalid install manifest")?
                        .iter()
                        .filter_map(|p| p.as_str())
                        .map(str::to_owned),
                );
            }
            Ok(format!(
                "{}\n",
                serde_json::to_string_pretty(&json!({"version":1,"files":files}))?
            ))
        })?;
    }
    if opts.gitignore {
        ignore_block::apply(&mut plan, &root, opts.dry_run)?;
    }
    if opts.dry_run {
        plan.print();
    } else {
        plan.apply()?;
        println!(
            "\nConfigured {} platform(s): {}",
            configured.len(),
            configured.join(", ")
        );
        for change in &plan.0 {
            println!("  {}", change.shown);
        }
    }
    Ok(())
}

pub(super) fn readonly(args: &[std::ffi::OsString]) -> Result<i32> {
    let cmd = args.first().and_then(|s| s.to_str()).unwrap_or_default();
    ensure!(
        matches!(
            cmd,
            "status"
                | "search"
                | "query"
                | "impact"
                | "detect-changes"
                | "architecture"
                | "flows"
                | "flow"
                | "communities"
                | "community"
                | "large-functions"
                | "refactor"
        ),
        "unsupported read-only command"
    );
    let mut command = std::process::Command::new(env::current_exe()?);
    command.env("CRG_READ_ONLY", "1").arg(cmd);
    if cmd == "status" && !args.iter().any(|s| s == "--json") {
        command.arg("--json");
    }
    let status = command.args(&args[1..]).status()?;
    Ok(status.code().unwrap_or(1))
}
