mod config;
mod lifecycle;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "crg-daemon", disable_help_subcommand = true)]
struct Args {
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    Start {
        #[arg(long)]
        foreground: bool,
    },
    Stop,
    Restart {
        #[arg(long)]
        foreground: bool,
    },
    Status,
    Logs {
        #[arg(long)]
        repo: Option<String>,
        #[arg(short = 'f', long)]
        follow: bool,
        #[arg(short = 'n', long, default_value_t = 50, allow_hyphen_values = true)]
        lines: isize,
    },
    Add {
        path: PathBuf,
        #[arg(long)]
        alias: Option<String>,
    },
    Remove {
        path_or_alias: String,
    },
}

fn nested() -> bool {
    static NESTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NESTED.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::file_stem)
            == Some(OsStr::new("code-review-graph"))
    })
}

fn log(message: std::fmt::Arguments<'_>) {
    if nested() {
        let message = message.to_string();
        if let Some(warning) = message.strip_prefix("WARNING: ") {
            eprintln!("{warning}");
        }
    } else {
        eprintln!("{message}");
    }
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    let args: Vec<_> = args.into_iter().collect();
    match dispatch(&args) {
        Ok(code) => code,
        Err(e) => {
            println!("Error: {e:#}");
            1
        }
    }
}

fn dispatch(args: &[OsString]) -> Result<i32> {
    if args.first().is_some_and(|s| s == "--crg-watch-worker") {
        let [_, repo, poll] = args else {
            anyhow::bail!("invalid watch worker arguments")
        };
        lifecycle::worker(
            &config::home()?,
            Path::new(repo),
            poll.to_str().context("invalid interval")?.parse()?,
        )?;
        return Ok(0);
    }
    let nested = nested();
    let docs: BTreeMap<String, String> = serde_json::from_str(if nested {
        include_str!("nested-help.json")
    } else {
        include_str!("standalone-help.json")
    })?;
    let cmd = args.first().and_then(|s| s.to_str()).unwrap_or("");
    if args.is_empty()
        || matches!(cmd, "-h" | "--help")
        || (docs.contains_key(cmd) && args.iter().any(|s| s == "-h" || s == "--help"))
    {
        print!("{}", docs.get(cmd).unwrap_or(&docs[""]));
        return Ok(0);
    }
    let action = match Args::try_parse_from(
        std::iter::once(OsString::from("crg-daemon")).chain(args.iter().cloned()),
    ) {
        Ok(args) => args.action,
        Err(e) => {
            e.print()?;
            return Ok(e.exit_code());
        }
    };
    let home = config::home()?;
    let success = match action {
        Action::Start { foreground } => {
            lifecycle::start(&home, foreground)?;
            true
        }
        Action::Stop => lifecycle::stop(&home)?,
        Action::Restart { foreground } => {
            if lifecycle::running(&home)?.is_some() {
                lifecycle::stop(&home)?;
            } else {
                println!("Daemon is not running, starting fresh.");
            }
            lifecycle::start(&home, foreground)?;
            true
        }
        Action::Status => {
            status(&home)?;
            true
        }
        Action::Logs {
            repo,
            follow,
            lines,
        } => logs(&home, repo.as_deref(), follow, lines)?,
        Action::Add { path, alias } => {
            config::add(&home, &path, alias)?;
            changed(&home)?;
            true
        }
        Action::Remove { path_or_alias } => {
            let removed = config::remove(&home, &path_or_alias)?;
            if removed {
                changed(&home)?;
            }
            removed
        }
    };
    Ok(i32::from(!success))
}

fn changed(home: &Path) -> Result<()> {
    if lifecycle::running(home)?.is_some() {
        println!("Daemon will pick up the change automatically.");
    }
    Ok(())
}

fn logs(home: &Path, alias: Option<&str>, follow: bool, lines: isize) -> Result<bool> {
    let c = config::load(home)?;
    let path = config::log_file(&c, alias)?;
    if !path.exists() {
        println!("Log file not found: {}", path.display());
        return Ok(false);
    }
    if follow {
        Command::new("tail").arg("-f").arg(&path).status()?;
        return Ok(true);
    }
    let data = fs::read(&path)?;
    let data = String::from_utf8_lossy(&data);
    let rows: Vec<_> = data.lines().collect();
    let start = if lines == 0 {
        0
    } else if lines < 0 {
        lines.unsigned_abs().min(rows.len())
    } else {
        rows.len().saturating_sub(lines as usize)
    };
    for row in &rows[start..] {
        println!("{row}");
    }
    Ok(true)
}

fn json_file(path: PathBuf) -> Value {
    fs::read(path)
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or(Value::Null)
}

fn age(seconds: f64) -> String {
    let s = seconds.max(0.0);
    let (n, u) = if s < 60.0 {
        (s, "s")
    } else if s < 3600.0 {
        (s / 60.0, "m")
    } else if s < 86400.0 {
        (s / 3600.0, "h")
    } else {
        (s / 86400.0, "d")
    };
    format!("{n:.0}{u}")
}

fn status(home: &Path) -> Result<()> {
    let c = config::load(home)?;
    let pid = lifecycle::running(home)?;
    if let Some(pid) = pid {
        println!("Daemon:  running (PID {pid})");
    } else {
        println!("Daemon:  not running");
    }
    println!(
        "Name:    {}\nLog dir: {}\nPoll:    {}s\n",
        c.name,
        c.logs.display(),
        c.poll
    );
    if c.repos.is_empty() {
        println!("No repositories configured.\nUse: crg-daemon add <path> [--alias NAME]");
        return Ok(());
    }
    let width = c
        .repos
        .iter()
        .map(|r| r.alias.chars().count())
        .max()
        .unwrap_or(0)
        .max(5);
    if pid.is_none() {
        println!("  {:width$}  {:8}  {:8}  Path", "Alias", "Status", "PID");
        println!(
            "  {}  --------  --------  {}",
            "-".repeat(width),
            "-".repeat(40)
        );
        for r in c.repos {
            println!(
                "  {:width$}  {:8}  {:8}  {}",
                r.alias,
                "-",
                "-",
                r.path.display()
            );
        }
        return Ok(());
    }
    let state = json_file(home.join("daemon-state.json"));
    println!(
        "  {:width$}  {:8}  {:8}  {:8}  {:6}  Path",
        "Alias", "Status", "Watcher", "PID", "Event"
    );
    println!(
        "  {}  --------  --------  --------  ------  {}",
        "-".repeat(width),
        "-".repeat(40)
    );
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    let stale_after = std::env::var("CRG_WATCH_HEALTH_STALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90.0);
    let mut stalled = false;
    let mut partial = false;
    for r in c.repos {
        let pid = state[&r.alias]["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok());
        let alive = pid.is_some_and(lifecycle::alive);
        let health = json_file(lifecycle::health_file(home, &r.path));
        let watcher = if !alive {
            "dead"
        } else if health.is_null() {
            "unknown"
        } else if health["observer_alive"] == false
            || health["updated_at"]
                .as_f64()
                .is_some_and(|v| now - v > stale_after)
        {
            stalled = true;
            "stalled"
        } else if health["degraded"] == true {
            partial = true;
            "partial"
        } else {
            "ok"
        };
        let event = health["last_event_at"]
            .as_f64()
            .map_or_else(|| "-".into(), |s| age(now - s));
        let pid = pid.map_or_else(|| "-".into(), |p| p.to_string());
        println!(
            "  {:width$}  {:8}  {watcher:8}  {pid:8}  {event:6}  {}",
            r.alias,
            if alive { "alive" } else { "dead" },
            r.path.display()
        );
    }
    if stalled {
        println!(
            "\n  A stalled watcher means the process is up but its filesystem observer is not;\n  the graph is no longer updating. Check the log, then: crg-daemon restart"
        );
    }
    if partial {
        println!(
            "\n  A partial watcher is running with reduced coverage. The health file's\n  failed_paths field lists directories the OS could not register;\n  when it is empty, watch slots ran out and one recursive watch\n  replaced the filtered plan; raise CRG_MAX_WATCH_SCHEDULES then."
        );
    }
    Ok(())
}
