use crate::model::{ParsedFile, Symbol, sql::SqlDeclaration};
use anyhow::{Context, Result};
use regex::Regex;
use std::{collections::HashSet, path::Path, sync::LazyLock};
use tree_sitter::{Node, Parser};

pub(crate) mod dbt;
mod oracle;

macro_rules! pattern {
    ($pattern:expr) => {{
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
        &*RE
    }};
}
pub(super) use pattern;

pub fn supports(extension: &str) -> bool {
    matches!(
        extension,
        "sql" | "pls" | "plb" | "pks" | "pkb" | "pck" | "prc" | "fnc" | "trg"
    )
}

struct Extractor<'a> {
    src: &'a str,
    lines: Vec<usize>,
    file: ParsedFile,
}

impl<'a> Extractor<'a> {
    fn line(&self, offset: usize) -> usize {
        self.lines.partition_point(|start| *start <= offset)
    }

    fn declare(
        &mut self,
        name: &str,
        identity: &str,
        parent: Option<&str>,
        kind: &str,
        span: std::ops::Range<usize>,
    ) {
        let symbol_kind = if matches!(kind, "procedure" | "function" | "trigger") {
            "Function"
        } else {
            "Class"
        };
        self.file.symbols.push(Symbol {
            name: identity.into(),
            kind: symbol_kind.into(),
            start_line: self.line(span.start),
            end_line: self.line(span.end),
        });
        self.file.sql.declarations.insert(
            identity.into(),
            SqlDeclaration {
                name: name.into(),
                parent: parent.map(String::from),
                extra: serde_json::json!({"sql_kind":kind}),
            },
        );
    }

    fn ddl(&mut self, node: Node<'_>) {
        let kind = match node.kind() {
            "create_table" => "table",
            "create_view" => "view",
            "create_function" => "function",
            _ => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.ddl(child);
                }
                return;
            }
        };
        let mut cursor = node.walk();
        let Some(name_node) =
            node.named_children(&mut cursor)
                .find_map(|child| match child.kind() {
                    "identifier" | "object_reference" | "dotted_name" => Some(child),
                    "relation" => child
                        .named_children(&mut child.walk())
                        .find(|n| matches!(n.kind(), "identifier" | "object_reference")),
                    _ => None,
                })
        else {
            return;
        };
        let name = self.src[name_node.byte_range()]
            .trim_matches(['`', '"'])
            .rsplit('.')
            .next()
            .unwrap_or("");
        if !name.is_empty() {
            self.declare(name, name, None, kind, node.byte_range());
        }
    }

    fn tables(&mut self, text: &str) {
        let pattern = pattern!(
            r"(?i)(?:FROM|JOIN|INTO|CREATE\s+(?:OR\s+REPLACE\s+)?(?:TABLE|VIEW)(?:\s+IF\s+NOT\s+EXISTS)?|INSERT\s+OVERWRITE)\s+((?:`[^`]+`|\w+)(?:\.(?:`[^`]+`|\w+))*)"
        );
        let mut seen = HashSet::new();
        for hit in pattern.captures_iter(text) {
            let name = hit[1].trim_matches('`').rsplit('.').next().unwrap_or("");
            if !name.is_empty() && !is_keyword(name) && seen.insert(name.to_string()) {
                self.file
                    .sql
                    .tables
                    .push((self.line(hit.get(0).unwrap().start()), name.into()));
            }
        }
    }

    fn dbt_model(&mut self, path: &Path, membership: Option<bool>) -> bool {
        let pattern = pattern!(
            r#"\{\{-?\s*(ref|source)\s*\(\s*['"]([^'"]+)['"](?:\s*,\s*['"]([^'"]+)['"])?\s*\)"#
        );
        if !membership.unwrap_or_else(|| pattern.is_match(self.src)) {
            return false;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        self.declare(name, name, None, "dbt_model", 0..self.src.len());
        let mut seen = HashSet::new();
        for hit in pattern.captures_iter(self.src) {
            let target = if let Some(second) = hit.get(3) {
                format!("{}.{}", &hit[2], second.as_str())
            } else if &hit[1] == "ref" {
                hit[2].to_string()
            } else {
                continue;
            };
            if target != name && seen.insert(target.clone()) {
                self.file
                    .sql
                    .tables
                    .push((self.line(hit.get(0).unwrap().start()), target));
            }
        }
        true
    }
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "SELECT"
            | "WHERE"
            | "GROUP"
            | "ORDER"
            | "HAVING"
            | "LIMIT"
            | "OFFSET"
            | "UNION"
            | "INTERSECT"
            | "EXCEPT"
            | "AS"
            | "ON"
            | "USING"
            | "SET"
            | "VALUES"
            | "DEFAULT"
            | "NULL"
            | "TRUE"
            | "FALSE"
            | "INNER"
            | "OUTER"
            | "LEFT"
            | "RIGHT"
            | "FULL"
            | "CROSS"
            | "NATURAL"
            | "LATERAL"
            | "RECURSIVE"
            | "ONLY"
            | "WITH"
            | "IF"
            | "NOT"
            | "EXISTS"
    )
}

pub fn parse(path: &Path, src: &str, membership: Option<bool>) -> Result<ParsedFile> {
    let mut out = Extractor {
        src,
        lines: std::iter::once(0)
            .chain(src.match_indices('\n').map(|(i, _)| i + 1))
            .collect(),
        file: ParsedFile::default(),
    };
    out.file.sql.dbt_membership = membership;
    if out.dbt_model(path, membership) {
        return Ok(out.file);
    }
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_sequel::LANGUAGE.into())?;
    let tree = parser.parse(src, None).context("parsing SQL")?;
    out.ddl(tree.root_node());
    let clean = oracle::clean_if_oracle(path, src);
    let text = clean.as_deref().unwrap_or(src);
    oracle::routines(&mut out, text, clean.is_some());
    out.tables(text);
    Ok(out.file)
}
