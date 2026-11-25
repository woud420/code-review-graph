mod config;
mod graph;
mod hooks;
mod probe;

use super::wire;
use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use std::path::Path;

const VERSION: &str = "2.4.0";
const UPGRADE: &str = "pip install -U code-review-graph, pipx upgrade code-review-graph or uv tool upgrade code-review-graph, whichever installed it";

#[derive(Serialize)]
struct Finding {
    status: &'static str,
    message: String,
    fix: String,
}
impl Finding {
    fn new(status: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            fix: String::new(),
        }
    }
    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = fix.into();
        self
    }
}

fn mismatch(version: &str, location: &str, upgrade: &str) -> String {
    let numbers: Vec<u64> = version
        .split('.')
        .take(3)
        .map(|s| {
            s.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
        })
        .collect::<Result<_, _>>()
        .unwrap_or_default();
    if numbers.len() == 3 && numbers.as_slice() >= [2, 4, 0].as_slice() {
        format!(
            "This copy ({VERSION}) is the older one. Upgrade it ({UPGRADE}), then restart your AI tool."
        )
    } else {
        format!("Upgrade {location}: {upgrade}. Then restart your AI tool.")
    }
}

pub(super) fn run(root: &Path, json_output: bool) -> Result<i32> {
    let exe = std::env::current_exe()?;
    let mut checks = vec![Finding::new(
        "ok",
        format!("This copy: {VERSION} at {}.", exe.display()),
    )];
    let mut probes = probe::Probes::new(root, &exe);
    let copies = probes.copies(&mut checks);
    let entries = config::entries(root, &mut probes, &mut checks);
    let graph = graph::check(root, &probes, &mut checks);
    let hooks = hooks::check(root, &mut probes, !copies.is_empty(), &mut checks);
    let problems = checks.iter().filter(|c| c.status == "fail").count();
    if json_output {
        let report = json!({"healthy":problems==0,"repo_root":root,"this_copy":{"version":VERSION,"executable":exe},"path_copies":copies,"mcp_entries":entries,"graph":graph,"hooks":hooks,"checks":checks});
        println!("{}", wire::json_text(&report, true));
    } else {
        println!("code-review-graph doctor: {}\n", root.display());
        for check in checks {
            println!("  {:<5} {}", check.status, check.message);
            if !check.fix.is_empty() {
                println!("        Fix: {}", check.fix);
            }
        }
        match problems {
            0 => println!("\nAll good."),
            1 => println!("\n1 problem found."),
            n => println!("\n{n} problems found."),
        }
    }
    Ok(i32::from(problems > 0))
}
