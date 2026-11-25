use crate::model::{
    Call, ParsedFile, Symbol,
    syntax::{Edge, Site},
};
use regex::Regex;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};

static COLUMNS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{2,}|\t+").unwrap());
static PIPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s\|(?:\s|$)").unwrap());
static VARIABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[$@&]\{[^}]+\}\s*=?$").unwrap());
static ASSIGN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[$@&]\{[^}]+\}\s*=\s+(.+)$").unwrap());
static BDD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:given|when|then|and|but)\s+").unwrap());

pub(crate) fn resource(file: &Path, raw: &str) -> String {
    let parent = file.parent().unwrap_or(Path::new(""));
    let expanded = raw.replace("${CURDIR}", &parent.to_string_lossy());
    if expanded.contains("${") {
        return expanded;
    }
    let mut result = PathBuf::new();
    for part in parent.join(expanded).components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result.to_string_lossy().into_owned()
}
fn control(s: &str) -> bool {
    matches!(
        s,
        "IF" | "FOR"
            | "WHILE"
            | "TRY"
            | "GROUP"
            | "ELSE IF"
            | "ELSE"
            | "EXCEPT"
            | "FINALLY"
            | "BREAK"
            | "CONTINUE"
            | "RETURN"
            | "VAR"
            | "END"
    )
}
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}
fn row_calls(mut cells: &[&str]) -> Vec<String> {
    while cells.first().is_some_and(|c| VARIABLE.is_match(c)) {
        cells = &cells[1..];
    }
    let Some(raw) = cells.first() else {
        return vec![];
    };
    let capture = ASSIGN.captures(raw);
    let word = capture
        .as_ref()
        .and_then(|c| c.get(1))
        .map_or(*raw, |v| v.as_str());
    match word.to_ascii_lowercase().as_str() {
        "[arguments]" | "[documentation]" | "[return]" | "[returns]" | "[timeout]" | "[tags]" => {
            return vec![];
        }
        "[setup]" | "[teardown]" | "[template]" => {
            return cells
                .get(1)
                .filter(|c| !c.eq_ignore_ascii_case("none"))
                .map(|s| s.to_string())
                .into_iter()
                .collect();
        }
        _ => {}
    }
    if word == "IF" && cells.len() > 2 {
        let mut branches = vec![cells[2]];
        for (i, c) in cells.iter().enumerate() {
            let idx = match *c {
                "ELSE IF" => i + 2,
                "ELSE" => i + 1,
                _ => continue,
            };
            if let Some(t) = cells.get(idx) {
                branches.push(t);
            }
        }
        return branches
            .into_iter()
            .filter(|s| !control(s))
            .map(str::to_owned)
            .collect();
    }
    (!control(word))
        .then(|| word.to_owned())
        .into_iter()
        .collect()
}
pub(super) fn parse(source: &str) -> ParsedFile {
    let mut f = ParsedFile::default();
    let mut section = String::new();
    let mut active: Option<usize> = None;
    let mut pending = Vec::new();
    let lines = source.trim_start_matches('\u{feff}').lines();
    let mut last = 0;
    for (i, line) in lines.enumerate() {
        last = i + 1;
        let trim = line.trim();
        if trim.is_empty() || trim.starts_with('#') {
            continue;
        }
        let pipe;
        let (row, body) = if let Some(rest) = trim.strip_prefix('|') {
            pipe = format!("{rest} ");
            let cols: Vec<_> = PIPE.split(&pipe).collect();
            let body = cols[0].trim().is_empty();
            (cols, body)
        } else {
            (
                COLUMNS.split(line).collect(),
                line.starts_with(char::is_whitespace),
            )
        };
        let cells: Vec<_> = row
            .into_iter()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let Some(first) = cells.first().copied() else {
            continue;
        };
        if first.starts_with('*')
            && first.ends_with('*')
            && first.trim_matches('*').trim().chars().next().is_some()
        {
            if let Some(a) = active.take() {
                f.symbols[a].end_line = last - 1;
            }
            let h = first.trim_matches('*').trim().to_lowercase();
            section = match h.as_str() {
                "setting" => "settings",
                "test case" => "test cases",
                "task" => "tasks",
                "keyword" => "keywords",
                _ => &h,
            }
            .into();
            continue;
        }
        if first == "..." {
            continue;
        }
        if section == "settings" {
            let Some(value) = cells.get(1) else {
                continue;
            };
            let key = first.to_lowercase();
            if matches!(key.as_str(), "library" | "resource" | "variables") {
                f.syntax.edges.push(Edge {
                    kind: "IMPORTS_FROM".into(),
                    owner: "<module>".into(),
                    target: value.to_string(),
                    line: last,
                    extra: json!({"robot_setting":key}),
                });
            } else if matches!(
                key.as_str(),
                "suite setup"
                    | "suite teardown"
                    | "test setup"
                    | "test teardown"
                    | "test template"
                    | "task setup"
                    | "task teardown"
                    | "task template"
            ) && !value.eq_ignore_ascii_case("none")
            {
                pending.push((None, value.to_string(), last));
            }
            continue;
        }
        if !matches!(section.as_str(), "keywords" | "test cases" | "tasks") {
            continue;
        }
        if !body {
            if let Some(a) = active.take() {
                f.symbols[a].end_line = last - 1;
            }
            active = Some(f.symbols.len());
            f.symbols.push(Symbol {
                name: first.into(),
                kind: if section == "keywords" {
                    "Function"
                } else {
                    "Test"
                }
                .into(),
                start_line: last,
                end_line: last,
            });
        } else if active.is_some() {
            pending.extend(row_calls(&cells).into_iter().map(|t| (active, t, last)));
        }
    }
    if let Some(a) = active {
        f.symbols[a].end_line = last;
    }
    let tests: HashSet<_> = f
        .symbols
        .iter()
        .filter(|s| s.kind == "Test")
        .map(|s| s.name.clone())
        .collect();
    let funcs: HashSet<_> = f
        .symbols
        .iter()
        .filter(|s| s.kind == "Function")
        .map(|s| s.name.clone())
        .collect();
    let mut local: HashMap<String, Vec<String>> = HashMap::new();
    for s in &mut f.symbols {
        let display = s.name.clone();
        if tests.contains(&display) && funcs.contains(&display) {
            s.name = format!("{}::{display}", s.kind);
        }
        f.syntax
            .display_names
            .insert(s.name.clone(), display.clone());
        f.syntax
            .declarations
            .insert(s.name.clone(), Default::default());
        if s.kind == "Function" {
            local
                .entry(normalize(&display))
                .or_default()
                .push(s.name.clone());
        }
    }
    for (owner, mut raw, line) in pending {
        if !local.contains_key(&normalize(&raw)) {
            raw = BDD.replace(&raw, "").into_owned();
        }
        let target = local
            .get(&normalize(&raw))
            .filter(|s| s.len() == 1)
            .map(|s| s[0].clone());
        let caller = owner.map_or("<module>", |i| &f.symbols[i].name).to_owned();
        f.calls.push(Call {
            caller: caller.clone(),
            callee: raw.clone(),
            line,
        });
        f.syntax.sites.entry(line).or_default().push(Site {
            caller,
            raw,
            target,
            extra: json!({}),
        });
    }
    f
}
