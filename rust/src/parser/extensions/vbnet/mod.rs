mod syntax;
use super::super::syntax::qualified;
use crate::model::{
    Call, ParsedFile, Symbol,
    syntax::{Definition, Edge, Site},
};
use serde_json::json;
use std::{collections::HashMap, path::Path};
use syntax::*;

#[derive(Default)]
struct State {
    file: ParsedFile,
    namespaces: Vec<usize>,
    types: Vec<(String, usize)>,
    member: Option<usize>,
    member_by_name: HashMap<String, usize>,
}

fn same_file(ids: &[String], owner: &str, raw: &str) -> Option<String> {
    let bare = raw
        .split_once('.')
        .filter(|(p, _)| {
            ["global", "me", "myclass"]
                .iter()
                .any(|s| s.eq_ignore_ascii_case(p))
        })
        .map_or(raw, |(_, s)| s);
    let mut scope = owner.rsplit_once('.').map(|(scope, _)| scope);
    loop {
        let test = qualified(scope.unwrap_or(""), bare);
        let mut found = ids.iter().filter(|id| id.eq_ignore_ascii_case(&test));
        if let Some(first) = found.next()
            && found.next().is_none()
        {
            return Some(first.clone());
        }
        scope = match scope {
            Some(p) => p.rsplit_once('.').map(|(p, _)| p),
            None => break,
        };
    }
    let mut found = ids.iter().filter(|id| {
        id.rsplit('.')
            .next()
            .is_some_and(|p| p.eq_ignore_ascii_case(bare))
    });
    let one = found.next()?.clone();
    found.next().is_none().then_some(one)
}

impl State {
    fn name(&self, index: Option<usize>) -> &str {
        index.map_or("", |i| self.file.symbols[i].name.as_str())
    }
    fn parent(&self) -> &str {
        self.name(
            self.types
                .last()
                .map(|t| t.1)
                .or_else(|| self.namespaces.last().copied()),
        )
    }
    fn close_member(&mut self, end: usize) {
        if let Some(i) = self.member.take() {
            let sym = &mut self.file.symbols[i];
            sym.end_line = sym.end_line.max(end);
        }
    }
    fn close_type(&mut self, kind: &str, end: usize) {
        self.close_member(end);
        if let Some(i) = self
            .types
            .iter()
            .rposition(|t| t.0 == kind)
            .or_else(|| self.types.len().checked_sub(1))
        {
            let (_, n) = self.types.remove(i);
            self.file.symbols[n].end_line = end;
        }
    }
    fn add(&mut self, name: &str, kind: &str, rows: (usize, usize), decl: Definition) -> usize {
        let parent = self.parent().to_owned();
        let id = qualified(&parent, name);
        if name.contains('.') {
            self.file
                .syntax
                .display_names
                .insert(id.clone(), name.to_owned());
            if !parent.is_empty() {
                self.file.syntax.containers.insert(id.clone(), parent);
            }
        }
        let i = self.file.symbols.len();
        self.file.syntax.declarations.insert(id.clone(), decl);
        self.file.symbols.push(Symbol {
            name: id,
            kind: kind.into(),
            start_line: rows.0,
            end_line: rows.1,
        });
        i
    }
    fn relationship(&mut self, rest: &str, id: &str, line: usize) {
        if let Some(m) = REL.captures(rest) {
            for target in targets(&m[2]) {
                self.file.syntax.edges.push(Edge {
                    kind: m[1].to_ascii_uppercase(),
                    owner: id.into(),
                    target,
                    line,
                    extra: json!({"vbnet_unresolved":true}),
                });
            }
        }
    }
    fn calls(&mut self, statement: &str, line: usize) {
        let owner = self.name(self.member).to_owned();
        let mut occurrences = Vec::new();
        let mut spans = Vec::new();
        for m in NEW.captures_iter(statement) {
            spans.push(m.get(0).unwrap().range());
            occurrences.push((name(&m[1]), true));
        }
        for m in CALL.captures_iter(statement) {
            let start = m.get(0).unwrap().start();
            if spans.iter().any(|r| r.contains(&start))
                || statement[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
            {
                continue;
            }
            let raw = name(&m[1]);
            if !call_keyword(raw.split('.').next().unwrap_or("")) {
                occurrences.push((raw, false));
            }
        }
        for (raw, constructor) in occurrences {
            self.file.calls.push(Call {
                caller: owner.clone(),
                callee: raw.clone(),
                line,
            });
            let mut extra = json!({"vbnet_unresolved":true});
            if constructor {
                extra["constructor"] = true.into();
            }
            self.file.syntax.sites.entry(line).or_default().push(Site {
                caller: owner.clone(),
                raw,
                target: None,
                extra,
            });
        }
    }
    fn statement(&mut self, s: &str, rows: (usize, usize), test: bool) {
        let (first, last) = rows;
        let mut words = s.split_whitespace();
        let head = words.next().unwrap_or("").to_ascii_lowercase();
        if head == "imports" {
            for target in targets(&s[head.len()..]) {
                self.file.syntax.edges.push(Edge {
                    kind: "IMPORTS_FROM".into(),
                    owner: "<module>".into(),
                    target,
                    line: first,
                    extra: json!({}),
                });
            }
            return;
        }
        if head == "end" {
            let kind = words.next().unwrap_or("").to_ascii_lowercase();
            match kind.as_str() {
                "namespace" => {
                    while !self.types.is_empty() {
                        self.close_type("", last);
                    }
                    if let Some(i) = self.namespaces.pop() {
                        self.file.symbols[i].end_line = last;
                    }
                }
                "class" | "interface" | "structure" | "module" | "enum" => {
                    self.close_type(&kind, last)
                }
                "function" | "sub" | "property" | "operator" => self.close_member(last),
                _ => {}
            }
            return;
        }
        if let Some(c) = NAMESPACE.captures(s) {
            let i = self.add(
                &name(&c[1]),
                "Class",
                rows,
                Definition {
                    extra: json!({"vbnet_kind":"namespace"}),
                    ..Default::default()
                },
            );
            self.namespaces.push(i);
            return;
        }
        if !self.types.is_empty() && matches!(head.as_str(), "inherits" | "implements") {
            let owner = self.parent().to_owned();
            self.relationship(s, &owner, first);
            return;
        }
        if let Some(c) = DECL.captures(s).or_else(|| OPERATOR.captures(s)) {
            self.close_member(first);
            let kind = c[2].to_ascii_lowercase();
            let mut name = name(&c[3]);
            if kind == "operator" {
                name = format!("operator_{name}");
            }
            let (id, rest) = (qualified(self.parent(), &name), &c[4]);
            let (generics, params, returns) = signature(rest);
            let mut extra = json!({"vbnet_kind":kind});
            if !generics.is_empty() {
                extra["vbnet_type_parameters"] = json!(generics);
            }
            if matches!(
                kind.as_str(),
                "class" | "interface" | "structure" | "module" | "enum"
            ) {
                let i = self.add(
                    &name,
                    "Class",
                    rows,
                    Definition {
                        extra,
                        ..Default::default()
                    },
                );
                self.types.push((kind, i));
                self.relationship(rest, &id, first);
                return;
            }
            let mods = c[1].split_whitespace().collect::<Vec<_>>().join(" ");
            let key = id.to_lowercase();
            let index = if let Some(&i) = self.member_by_name.get(&key) {
                let old = &self.file.symbols[i].name;
                let data = self.file.syntax.declarations.get_mut(old).unwrap();
                if data.extra.get("vbnet_overloads").is_none() {
                    data.extra["vbnet_overloads"] = json!([data.params.as_deref().unwrap_or("")]);
                }
                data.extra["vbnet_overloads"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(params.as_deref().unwrap_or("")));
                self.file.symbols[i].end_line = last;
                i
            } else {
                let kind = if test
                    && matches!(
                        name.as_str(),
                        "describe"
                            | "it"
                            | "test"
                            | "beforeEach"
                            | "afterEach"
                            | "beforeAll"
                            | "afterAll"
                            | "suite"
                    ) {
                    "Test"
                } else {
                    super::function_kind(&name)
                };
                let i = self.add(
                    &name,
                    kind,
                    rows,
                    Definition {
                        params,
                        returns,
                        extra,
                        modifiers: (!mods.is_empty()).then_some(mods.clone()),
                        ..Default::default()
                    },
                );
                self.member_by_name.insert(key, i);
                i
            };
            self.relationship(rest, &id, first);
            if !self.types.last().is_some_and(|t| t.0 == "interface")
                && !mods.split_whitespace().any(|s| {
                    s.eq_ignore_ascii_case("mustoverride") || s.eq_ignore_ascii_case("declare")
                })
            {
                self.member = Some(index);
            }
            return;
        }
        if self.member.is_some() {
            self.calls(s, first);
        }
    }
    fn finish(mut self, last: usize) -> ParsedFile {
        self.close_member(last);
        while !self.types.is_empty() {
            self.close_type("", last);
        }
        for i in self.namespaces {
            self.file.symbols[i].end_line = last;
        }
        let ids: Vec<_> = self.file.symbols.iter().map(|s| s.name.clone()).collect();
        for e in &mut self.file.syntax.edges {
            if matches!(e.kind.as_str(), "INHERITS" | "IMPLEMENTS")
                && let Some(target) = same_file(&ids, &e.owner, &e.target)
            {
                e.target = target;
                e.extra = json!({});
            }
        }
        for site in self.file.syntax.sites.values_mut().flatten() {
            site.target = same_file(&ids, &site.caller, &site.raw);
            if site.target.is_some() {
                site.extra
                    .as_object_mut()
                    .and_then(|v| v.remove("vbnet_unresolved"));
            }
        }
        self.file
    }
}

pub(super) fn parse(path: &Path, source: &str) -> ParsedFile {
    let mut state = State::default();
    let test = crate::model::test_file(&path.to_string_lossy(), None);
    for (first, last, s) in statements(source) {
        state.statement(&s, (first, last), test);
    }
    state.finish(source.matches('\n').count() + 1)
}
