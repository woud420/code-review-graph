mod cells;
use super::super::{
    python,
    syntax::{children, field, text},
};
use crate::model::{
    ParsedFile,
    syntax::{Definition, Edge, Notebook, Site},
};
use anyhow::{Context, Result};
use cells::{Cell, databricks, jupyter};
use regex::Regex;
use serde_json::json;
use std::{path::Path, sync::LazyLock};
use tree_sitter::{Node, Parser};

pub(crate) fn export(path: &Path, s: &str) -> bool {
    path.extension().is_some_and(|e| e == "py")
        && s.lines()
            .next()
            .is_some_and(|l| l.trim_end() == "# Databricks notebook source")
}
fn imports(n: Node<'_>, s: &str, edges: &mut Vec<Edge>) {
    let line = n.start_position().row + 1;
    let mut add = |target: &str| {
        edges.push(Edge {
            kind: "IMPORTS_FROM".into(),
            owner: "<module>".into(),
            target: target.into(),
            line,
            extra: json!({}),
        })
    };
    if n.kind() == "import_from_statement"
        && let Some(name) = field(n, "module_name", s)
    {
        add(name);
        return;
    }
    if n.kind() == "import_statement" {
        for name in n.children_by_field_name("name", &mut n.walk()) {
            add(field(name, "name", s).unwrap_or(text(name, s)));
        }
        return;
    }
    for c in children(n) {
        imports(c, s, edges);
    }
}
pub(super) fn parse(path: &Path, s: &str) -> Result<ParsedFile> {
    let mut file = ParsedFile::default();
    let parsed = if export(path, s) {
        file.syntax.file = json!({"notebook_format":"databricks_py"});
        Some(("python".into(), databricks(s)))
    } else {
        jupyter(s)
    };
    let Some((kernel, cells)) = parsed else {
        file.syntax.notebook = Some(Notebook::default());
        return Ok(file);
    };
    let mut meta = Notebook {
        kernel,
        lines: 1,
        ..Default::default()
    };
    let mut offsets = Vec::new();
    let mut groups: Vec<(&str, Vec<&Cell>)> = Vec::new();
    for cell in &cells {
        if let Some((_, rows)) = groups.iter_mut().find(|(lang, _)| *lang == cell.language) {
            rows.push(cell);
        } else {
            groups.push((&cell.language, vec![cell]));
        }
    }
    static TABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:FROM|JOIN|INTO|CREATE\s+(?:OR\s+REPLACE\s+)?(?:TABLE|VIEW)(?:\s+IF\s+NOT\s+EXISTS)?|INSERT\s+OVERWRITE)\s+((?:`[^`]+`|\w+)(?:\.(?:`[^`]+`|\w+))*)").unwrap()
    });
    for (lang, cells) in groups {
        if lang == "sql" {
            for c in cells {
                for cap in TABLE.captures_iter(&c.source) {
                    file.syntax.edges.push(Edge {
                        kind: "IMPORTS_FROM".into(),
                        owner: "<module>".into(),
                        target: cap[1].replace('`', ""),
                        line: 1,
                        extra: json!({}),
                    });
                }
            }
            continue;
        }
        let mut line = 1;
        let mut chunks = Vec::new();
        for c in cells {
            let length = c.source.matches('\n').count() + usize::from(!c.source.ends_with('\n'));
            offsets.push((c.index, line, line + length));
            line += length + 1;
            chunks.push(c.source.as_str());
        }
        let code = chunks.join("\n");
        meta.lines = meta.lines.max(line);
        let mut parser = Parser::new();
        parser.set_language(
            &(if lang == "python" {
                tree_sitter_python::LANGUAGE
            } else {
                tree_sitter_r::LANGUAGE
            })
            .into(),
        )?;
        let tree = parser.parse(&code, None).context("parsing notebook code")?;
        let mut group = if lang == "python" {
            imports(tree.root_node(), &code, &mut file.syntax.edges);
            python::extract(tree.root_node(), &code)
        } else {
            let mut r = ParsedFile::default();
            super::r::walk(tree.root_node(), &code, &mut r, "", "<module>");
            r
        };
        for sym in &group.symbols {
            meta.languages.insert(sym.name.clone(), lang.into());
            if let Some(f) = group.python.definitions.get(&sym.name) {
                group.syntax.declarations.insert(
                    sym.name.clone(),
                    Definition {
                        params: f.params.clone(),
                        returns: f.returns.clone(),
                        decorators: f.decorators.clone(),
                        modifiers: f.modifiers.clone(),
                        ..Default::default()
                    },
                );
            }
        }
        for call in &mut group.calls {
            let mut extra = json!({});
            let target = if lang == "python"
                && let Some((receiver, method)) = call.callee.rsplit_once('.')
            {
                extra["receiver"] = json!(receiver);
                if receiver == "self" {
                    call.caller
                        .rsplit_once('.')
                        .map(|(owner, _)| format!("{owner}.{method}"))
                        .filter(|v| group.syntax.declarations.contains_key(v))
                } else {
                    None
                }
            } else if group.syntax.declarations.contains_key(&call.callee) {
                Some(call.callee.clone())
            } else {
                None
            };
            if lang == "python"
                && target.is_none()
                && let Some((_, method)) = call.callee.rsplit_once('.')
            {
                call.callee = method.to_owned();
            }
            file.syntax.sites.entry(call.line).or_default().push(Site {
                caller: call.caller.clone(),
                raw: call.callee.clone(),
                target,
                extra,
            });
        }
        file.syntax.edges.append(&mut group.syntax.edges);
        file.syntax
            .declarations
            .append(&mut group.syntax.declarations);
        file.symbols.append(&mut group.symbols);
        file.calls.append(&mut group.calls);
    }
    for sym in &file.symbols {
        if let Some((idx, _, _)) = offsets
            .iter()
            .find(|(_, start, end)| *start <= sym.start_line && sym.start_line < *end)
        {
            let details = file
                .syntax
                .declarations
                .entry(sym.name.clone())
                .or_default();
            details.extra["cell_index"] = json!(idx);
        }
    }
    file.syntax.notebook = Some(meta);
    Ok(file)
}
