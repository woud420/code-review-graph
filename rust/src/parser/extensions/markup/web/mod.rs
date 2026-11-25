mod markup;
use super::super::{
    syntax::{child, children, text},
    typescript,
};
use crate::model::{
    Call, ParsedFile,
    syntax::{Edge, Site},
};
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::json;
use std::{collections::BTreeSet, path::Path, sync::LazyLock};
use tree_sitter::{Node, Parser};

pub(crate) struct Page {
    code: Vec<u8>,
    typescript: bool,
    facts: ParsedFile,
    handlers: Vec<(String, String, usize)>,
    module_ranges: Vec<(usize, usize)>,
}
pub(crate) fn language(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "html" | "htm" => Some("html"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}
impl Page {
    pub(crate) fn read(kind: &str, s: &str) -> Result<Self> {
        let mut result = Self {
            code: s
                .bytes()
                .map(|c| if c == b'\n' { b'\n' } else { b' ' })
                .collect(),
            typescript: false,
            facts: ParsedFile::default(),
            handlers: vec![],
            module_ranges: vec![],
        };
        if kind == "html" && s.len() > 2_097_152 {
            result.facts.syntax.file = json!({"html_truncated":true});
            return Ok(result);
        }
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_html::LANGUAGE.into())?;
        let tree = parser.parse(s, None).context("parsing HTML markup")?;
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "script_element" => {
                    if kind == "html" || n.parent() == Some(tree.root_node()) {
                        result.script(kind, n, s);
                    }
                    continue;
                }
                "style_element" => continue,
                "element" if kind == "html" => {
                    markup::section(&mut result.facts, &mut result.handlers, n, s)
                }
                _ => {}
            }
            stack.extend(children(n).into_iter().rev());
        }
        Ok(result)
    }
    fn script(&mut self, kind: &str, n: Node<'_>, s: &str) {
        let Some(tag) = child(n, "start_tag") else {
            return;
        };
        let attrs = markup::attrs(tag, s);
        if kind == "html"
            && let Some(src) = attrs.get("src")
        {
            let raw = src.trim().split(['?', '#']).next().unwrap_or("");
            if !raw.is_empty()
                && !raw.starts_with("//")
                && !raw.split_once(':').is_some_and(|(v, _)| !v.contains('/'))
                && !markup::template(raw)
            {
                self.facts.syntax.edges.push(Edge {
                    kind: "IMPORTS_FROM".into(),
                    owner: "<module>".into(),
                    target: raw.into(),
                    line: n.start_position().row + 1,
                    extra: json!({}),
                });
            }
            return;
        }
        let ty = attrs
            .get("type")
            .and_then(|v| v.split(';').next())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let ts = attrs
            .get("lang")
            .is_some_and(|v| matches!(v.to_lowercase().as_str(), "ts" | "typescript"))
            || matches!(ty.as_str(), "application/typescript" | "text/typescript");
        if kind == "html"
            && !ts
            && !matches!(
                ty.as_str(),
                "" | "application/ecmascript"
                    | "application/javascript"
                    | "application/x-javascript"
                    | "module"
                    | "text/babel"
                    | "text/ecmascript"
                    | "text/javascript"
                    | "text/jsx"
            )
        {
            return;
        }
        if let Some(raw) = child(n, "raw_text") {
            self.code[raw.byte_range()].copy_from_slice(text(raw, s).as_bytes());
            self.typescript |= ts;
            if ty == "module" {
                self.module_ranges
                    .push((raw.start_position().row + 1, raw.end_position().row + 1));
            }
        }
    }
    pub(crate) fn script_path(&self) -> &Path {
        if self.typescript {
            Path::new("inline.ts")
        } else {
            Path::new("inline.js")
        }
    }
    pub(crate) fn source(&self) -> &str {
        std::str::from_utf8(&self.code).expect("script spans preserve UTF-8 boundaries")
    }
    pub(super) fn extract(mut self) -> Result<ParsedFile> {
        let mut file = typescript::parse_typescript(self.script_path(), self.source())?;
        static FUNCTION: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"([A-Za-z_$][\w$]*)\s*\(").unwrap());
        for (event, handler, line) in self.handlers {
            let mut seen = BTreeSet::new();
            for m in FUNCTION.captures_iter(&handler) {
                let call = m.get(1).unwrap();
                if handler[..call.start()].trim_end().ends_with('.') || !seen.insert(call.as_str())
                {
                    continue;
                }
                if file.symbols.iter().any(|v| {
                    v.name == call.as_str()
                        && matches!(v.kind.as_str(), "Function" | "Test")
                        && !self
                            .module_ranges
                            .iter()
                            .any(|&(a, b)| v.start_line >= a && v.start_line <= b)
                }) {
                    file.calls.push(Call {
                        caller: "<module>".into(),
                        callee: call.as_str().into(),
                        line,
                    });
                    file.syntax.sites.entry(line).or_default().push(Site {
                        raw: call.as_str().into(),
                        caller: "<module>".into(),
                        target: Some(call.as_str().into()),
                        extra: json!({"html_event":event}),
                    });
                }
            }
        }
        file.symbols.append(&mut self.facts.symbols);
        file.syntax.edges.append(&mut self.facts.syntax.edges);
        file.syntax
            .display_names
            .append(&mut self.facts.syntax.display_names);
        file.syntax
            .declarations
            .append(&mut self.facts.syntax.declarations);
        file.syntax.file = self.facts.syntax.file;
        Ok(file)
    }
}
