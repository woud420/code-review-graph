mod lex;
use super::{super::syntax::qualified, function_kind};
use crate::model::{
    Call, ParsedFile, Symbol,
    syntax::{Definition, Edge, Site},
};
use lex::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

struct Module<'s> {
    name: &'s str,
    start: usize,
    body: usize,
    end: usize,
}
struct Binding {
    id: String,
    start: usize,
    end: usize,
}
fn symbol(file: &mut ParsedFile, id: &str, kind: &str, start: usize, end: usize, extra: Value) {
    file.symbols.push(Symbol {
        name: id.into(),
        kind: kind.into(),
        start_line: start,
        end_line: end,
    });
    file.syntax.declarations.insert(
        id.into(),
        Definition {
            extra,
            ..Default::default()
        },
    );
}
fn import(file: &mut ParsedFile, target: &str, line: usize, extra: Value) {
    if !file.syntax.edges.iter().any(|e| e.target == target) {
        file.syntax.edges.push(Edge {
            kind: "IMPORTS_FROM".into(),
            owner: "<module>".into(),
            target: target.into(),
            line,
            extra,
        });
    }
}
fn call(file: &mut ParsedFile, b: &Binding, raw: &str, line: usize, extra: Value) {
    let target =
        (!raw.contains('.') && file.syntax.declarations.contains_key(raw)).then(|| raw.to_string());
    file.calls.push(Call {
        caller: b.id.clone(),
        callee: raw.into(),
        line,
    });
    file.syntax.sites.entry(line).or_default().push(Site {
        caller: b.id.clone(),
        raw: raw.into(),
        target,
        extra,
    });
}
pub(super) fn parse(path: &Path, source: &str) -> ParsedFile {
    let mut file = ParsedFile::default();
    let input = Source::new(source);
    let s = &input.cleaned;
    let interface = path.extension().is_some_and(|e| e == "resi");
    if interface {
        file.syntax.file = json!({"rescript_interface":true});
    }
    let aliases: BTreeSet<_> = ALIAS.find_iter(s).map(|m| m.start()).collect();
    let mut modules = Vec::new();
    for cap in MODULE.captures_iter(s) {
        let header = cap.get(0).unwrap();
        if aliases.contains(&header.start()) {
            continue;
        }
        let Some(at) = s[header.end()..].find('{').map(|off| off + header.end()) else {
            continue;
        };
        if NEXT.is_match(&s[header.end()..at]) {
            continue;
        }
        let end = (at + 1..s.len())
            .find(|&i| s.as_bytes()[i] == b'}' && input.depth(i) == input.depth(at) + 1)
            .unwrap_or(s.len() - 1);
        modules.push(Module {
            name: cap.get(1).unwrap().as_str(),
            start: header.start(),
            body: at + 1,
            end,
        });
    }
    let parent = |off| {
        modules
            .iter()
            .rev()
            .find(|m| m.start <= off && off <= m.end)
    };
    for m in &modules {
        let outer = modules
            .iter()
            .rev()
            .find(|p| p.start < m.start && p.end > m.end)
            .map_or("", |p| p.name);
        symbol(
            &mut file,
            &qualified(outer, m.name),
            "Class",
            input.line(m.start),
            input.line(m.end),
            json!({"rescript_kind":"module"}),
        );
    }
    let mut lets = Vec::new();
    for cap in LET.captures_iter(s) {
        let name = cap.get(1).unwrap();
        let p = parent(name.start());
        if keyword(name.as_str())
            || input.depth(name.start()) != p.map_or(0, |m| input.depth(m.body))
        {
            continue;
        }
        lets.push(Binding {
            id: qualified(p.map_or("", |m| m.name), name.as_str()),
            start: name.start(),
            end: p.map_or(s.len(), |m| m.end),
        });
    }
    let mut owners: BTreeMap<&str, Vec<bool>> = BTreeMap::new();
    for i in 0..lets.len() {
        if let Some(next) = lets.get(i + 1) {
            lets[i].end = lets[i].end.min(next.start);
        }
        let b = &lets[i];
        let name = b.id.rsplit('.').next().unwrap();
        let kind = function_kind(name);
        symbol(
            &mut file,
            &b.id,
            kind,
            input.line(b.start),
            input.line(b.end.saturating_sub(1)),
            json!({}),
        );
        if let Some(p) = parent(b.start)
            && kind == "Function"
        {
            owners.entry(p.name).or_default().push(false);
        }
    }
    for (pattern, kind) in [(&*EXTERNAL, "Function"), (&*TYPE, "Type")] {
        for cap in pattern.captures_iter(s) {
            let name = cap.get(1).unwrap();
            let p = parent(name.start());
            if keyword(name.as_str())
                || input.depth(name.start()) != p.map_or(0, |m| input.depth(m.body))
            {
                continue;
            }
            let external = kind == "Function";
            let line = input.line(name.start());
            let id = qualified(p.map_or("", |m| m.name), name.as_str());
            symbol(
                &mut file,
                &id,
                kind,
                line,
                line,
                if external {
                    json!({"rescript_external":true})
                } else {
                    json!({})
                },
            );
            if external {
                if let Some(p) = p {
                    owners.entry(p.name).or_default().push(true);
                }
                let start = source[..name.start()]
                    .char_indices()
                    .rev()
                    .nth(199)
                    .map_or(0, |(i, _)| i);
                for attr in ATTRIBUTE.captures_iter(&source[start..name.start()]) {
                    import(
                        &mut file,
                        &attr[1],
                        line,
                        json!({"rescript_import_kind":"external_module"}),
                    );
                }
            }
        }
    }
    for m in &modules {
        if owners
            .get(m.name)
            .is_some_and(|values| values.iter().all(|v| *v))
        {
            let id = file
                .symbols
                .iter()
                .find(|sym| sym.kind == "Class" && sym.name.rsplit('.').next() == Some(m.name))
                .unwrap()
                .name
                .clone();
            file.syntax.declarations.get_mut(&id).unwrap().extra =
                json!({"rescript_kind":"js_binding"});
        }
    }
    for cap in OPEN.captures_iter(s) {
        import(
            &mut file,
            &cap[2],
            input.line(cap.get(0).unwrap().start()),
            json!({"rescript_import_kind":&cap[1]}),
        );
    }
    for cap in ALIAS.captures_iter(s) {
        import(
            &mut file,
            &cap[2],
            input.line(cap.get(0).unwrap().start()),
            json!({"rescript_import_kind":"module_alias","alias_name":&cap[1]}),
        );
    }
    if interface {
        return file;
    }
    let binding = |pos| lets.iter().rev().find(|b| b.start <= pos && pos < b.end);
    for cap in JSX.captures_iter(s) {
        let off = cap.get(0).unwrap().start();
        let tag = &cap[1];
        if s[..off]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace() && !"{(,>}".contains(c))
        {
            continue;
        }
        let line = input.line(off);
        import(
            &mut file,
            tag.split('.').next().unwrap(),
            line,
            json!({"rescript_import_kind":"jsx"}),
        );
        if let Some(b) = binding(off) {
            call(&mut file, b, tag, line, json!({"rescript_call_kind":"jsx"}));
        }
    }
    for cap in CALL.captures_iter(s) {
        let m = cap.get(1).unwrap();
        let raw = m.as_str();
        if keyword(raw.split('.').next().unwrap())
            || s[..m.start()]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
        {
            continue;
        }
        if let Some(b) = binding(m.start())
            && b.start != m.start()
        {
            call(&mut file, b, raw, input.line(m.start()), json!({}));
        }
    }
    file
}
