mod header;
mod resolve;
mod signature;

use crate::model::{
    Call, ParsedFile, Symbol,
    cfamily::{Definition, Site},
};
use anyhow::{Context, Result};
use std::{borrow::Cow, collections::HashSet, path::Path};
use tree_sitter::{Node, Parser};

pub(crate) fn supports(ext: &str) -> bool {
    matches!(ext, "h" | "c" | "xs" | "cpp" | "cc" | "cxx" | "hh" | "hpp")
}

fn text<'s>(n: Node<'_>, s: &'s str) -> &'s str {
    &s[n.byte_range()]
}
fn children(n: Node<'_>) -> Vec<Node<'_>> {
    n.named_children(&mut n.walk()).collect()
}
fn field<'s>(n: Node<'_>, field: &str, s: &'s str) -> Option<&'s str> {
    n.child_by_field_name(field).map(|v| text(v, s))
}
fn qualified(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_owned()
    } else {
        format!("{scope}.{name}")
    }
}
fn doc(n: Node<'_>, s: &str) -> Option<String> {
    let mut previous = n.prev_named_sibling();
    let mut row = n.start_position().row;
    let mut lines = Vec::new();
    while let Some(p) = previous {
        if p.kind() != "comment" || p.end_position().row + 1 < row {
            break;
        }
        let comment = text(p, s).trim();
        if let Some(body) = comment
            .strip_prefix("///")
            .or_else(|| comment.strip_prefix("//!"))
        {
            lines.push(body.trim().to_owned());
        } else if lines.is_empty() && (comment.starts_with("/**") || comment.starts_with("/*!")) {
            let body = comment[3..].strip_suffix("*/")?.trim();
            return Some(
                body.lines()
                    .map(|l| l.trim().trim_start_matches('*').trim())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        } else {
            break;
        }
        row = p.start_position().row;
        previous = p.prev_named_sibling();
    }
    lines.reverse();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn tree(s: &str, cpp: bool) -> Result<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(
        &(if cpp {
            tree_sitter_cpp::LANGUAGE
        } else {
            tree_sitter_c::LANGUAGE
        })
        .into(),
    )?;
    parser.parse(s, None).context("parsing C-family source")
}

pub fn parse(path: &Path, source: &str) -> Result<ParsedFile> {
    let ext = path.extension().and_then(|v| v.to_str()).unwrap_or("");
    let is_c = matches!(ext, "c" | "xs");
    let masked = if is_c {
        Cow::Borrowed(source)
    } else {
        header::qt(source)
    };
    let mut cpp = !is_c && (ext != "h" || header::potential(&masked));
    let mut syntax = tree(&masked, cpp)?;
    if ext == "h" && cpp && !header::cpp_tree(syntax.root_node(), &masked) {
        cpp = false;
        syntax = tree(source, false)?;
    }
    let mut out = ParsedFile::default();
    out.cfamily.language = if cpp { "cpp".into() } else { "c".into() };
    let mut input = Input {
        file: &mut out,
        seen: HashSet::new(),
        test_file: crate::model::test_file(&path.to_string_lossy(), None),
        source: if cpp { &masked } else { source },
        cpp,
    };
    input.walk(syntax.root_node(), "", None, "<module>", false);
    resolve::calls(&mut out, cpp);
    Ok(out)
}

struct Input<'a> {
    seen: HashSet<String>,
    test_file: bool,
    file: &'a mut ParsedFile,
    source: &'a str,
    cpp: bool,
}

impl Input<'_> {
    fn add(&mut self, n: Node<'_>, kind: &str, mut d: Definition) -> String {
        if !self.seen.insert(d.identity.clone()) {
            use sha2::{Digest, Sha256};
            let payload = serde_json::to_string(&(
                kind,
                &d.name,
                &d.parent,
                if self.cpp { "cpp" } else { "c" },
                &d.params,
                None::<String>,
                None::<String>,
                self.test_file || kind == "Test",
            ))
            .unwrap();
            let mut ascii = String::new();
            for ch in payload.chars() {
                if ch.is_ascii() {
                    ascii.push(ch);
                } else {
                    for u in ch.encode_utf16(&mut [0; 2]) {
                        use std::fmt::Write;
                        write!(ascii, "\\u{u:04x}").unwrap();
                    }
                }
            }
            let base = format!(
                "{}:S{}",
                d.identity,
                hex::encode(&Sha256::digest(ascii)[..6])
            );
            d.identity = base.clone();
            let mut ordinal = 2;
            while !self.seen.insert(d.identity.clone()) {
                d.identity = format!("{base}:{ordinal}");
                ordinal += 1;
            }
        }
        d.start = n.start_position().row + 1;
        d.doc = doc(n, self.source);
        self.file.symbols.push(Symbol {
            name: d.identity.clone(),
            kind: kind.into(),
            start_line: d.start,
            end_line: n.end_position().row + 1,
        });
        let identity = d.identity.clone();
        self.file.cfamily.declarations.push(d);
        identity
    }
    fn walk(&mut self, n: Node<'_>, scope: &str, class: Option<&str>, caller: &str, dead: bool) {
        let s = self.source;
        let mut scope = scope.to_owned();
        let mut class = class.map(str::to_owned);
        let mut caller = caller.to_owned();
        match n.kind() {
            "namespace_definition" if self.cpp => {
                if let Some(name) = field(n, "name", s) {
                    scope = qualified(&scope, &name.replace("::", "."));
                }
            }
            "struct_specifier" | "class_specifier" | "type_definition" => {
                let name = field(
                    n,
                    if n.kind() == "type_definition" {
                        "declarator"
                    } else {
                        "name"
                    },
                    s,
                );
                if let Some(name) = name {
                    let parent = class
                        .as_deref()
                        .and_then(|identity| {
                            self.file
                                .cfamily
                                .declarations
                                .iter()
                                .rev()
                                .find(|d| d.identity == identity)
                        })
                        .map(|d| d.name.clone());
                    let identity = qualified(parent.as_deref().unwrap_or(""), name);
                    let bases = children(n)
                        .into_iter()
                        .filter(|c| c.kind() == "base_class_clause")
                        .flat_map(children)
                        .filter(|c| !matches!(c.kind(), "access_specifier" | "comment"))
                        .map(|c| text(c, s).to_owned())
                        .collect();
                    let saved = self.add(
                        n,
                        "Class",
                        Definition {
                            identity: identity.clone(),
                            name: name.into(),
                            parent: parent.clone(),
                            container: if self.cpp { None } else { class.clone() },
                            bases,
                            ..Default::default()
                        },
                    );
                    scope = qualified(&scope, name);
                    class = Some(saved);
                }
            }
            "function_definition" | "declaration" | "field_declaration" => {
                let eligible = n.kind() == "function_definition"
                    || (self.cpp && !dead && caller == "<module>");
                if eligible && let Some(sig) = signature::extract(n, s, self.cpp) {
                    let parent = match sig.scope {
                        Some(explicit)
                            if !scope.is_empty()
                                && explicit != scope
                                && !explicit.starts_with(&format!("{scope}.")) =>
                        {
                            Some(qualified(&scope, &explicit))
                        }
                        Some(explicit) => Some(explicit),
                        None if !scope.is_empty() => Some(scope.clone()),
                        None => None,
                    };
                    caller = qualified(parent.as_deref().unwrap_or(""), &sig.identity);
                    let kind = if sig.name.starts_with("Test")
                        || sig.name.starts_with("test_")
                        || sig.name.ends_with("_test")
                        || sig.name.ends_with("_spec")
                    {
                        "Test"
                    } else {
                        "Function"
                    };
                    caller = self.add(
                        n,
                        kind,
                        Definition {
                            identity: caller.clone(),
                            name: sig.name,
                            parent,
                            container: class.clone(),
                            params: sig.params,
                            ..Default::default()
                        },
                    );
                }
            }
            "preproc_include" => {
                if let Some(path) = field(n, "path", s) {
                    self.file.cfamily.imports.push((
                        n.start_position().row + 1,
                        path.trim_matches(['"', '<', '>']).to_owned(),
                    ));
                }
            }
            "call_expression" if !dead => {
                if let Some(fun) = n.child_by_field_name("function") {
                    let (name, receiver) = match fun.kind() {
                        "qualified_identifier" | "identifier" => (Some(text(fun, s)), None),
                        "field_expression" => (field(fun, "field", s), field(fun, "argument", s)),
                        "template_function" => (field(fun, "name", s), None),
                        _ => (None, None),
                    };
                    if let Some(name) = name {
                        let raw = receiver.map_or_else(|| name.into(), |r| format!("{r}.{name}"));
                        let line = n.start_position().row + 1;
                        self.file.cfamily.sites.entry(line).or_default().push(Site {
                            caller: caller.clone(),
                            raw: raw.clone(),
                            receiver: receiver.map(str::to_owned),
                            ..Default::default()
                        });
                        self.file.calls.push(Call {
                            caller: caller.clone(),
                            callee: raw,
                            line,
                        });
                    }
                }
            }
            _ => {}
        }
        for c in children(n) {
            self.walk(
                c,
                &scope,
                class.as_deref(),
                &caller,
                dead || header::dead(n, c, s),
            );
        }
    }
}
