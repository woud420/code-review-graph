use super::*;
use std::ops::Range;
use tree_sitter::{Node, Parser, Tree};

pub(super) type Lines = BTreeMap<usize, &'static str>;
pub(super) struct Source {
    pub path: String,
    pub lang: String,
    pub data: String,
    pub tree: Option<Tree>,
    pub imports: BTreeMap<usize, BTreeSet<String>>,
}
impl Source {
    pub fn read(path: &str, lang: &str, name: &str, required: bool) -> Option<Self> {
        let data = std::fs::read_to_string(path).ok()?;
        let data = data.trim_start_matches('\u{feff}').to_owned();
        if !required && !data.contains(name) && !data.contains('*') {
            return None;
        }
        let grammar = match lang {
            "python" => Some(tree_sitter_python::LANGUAGE),
            "javascript" => Some(tree_sitter_javascript::LANGUAGE),
            "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX),
            "go" => Some(tree_sitter_go::LANGUAGE),
            "rust" => Some(tree_sitter_rust::LANGUAGE),
            "java" => Some(tree_sitter_java::LANGUAGE),
            "csharp" => Some(tree_sitter_c_sharp::LANGUAGE),
            "bash" => Some(tree_sitter_bash::LANGUAGE),
            "c" | "cpp" => Some(if path.ends_with(".c") {
                tree_sitter_c::LANGUAGE
            } else {
                tree_sitter_cpp::LANGUAGE
            }),
            "solidity" => Some(tree_sitter_solidity::LANGUAGE),
            "hcl" => Some(tree_sitter_hcl::LANGUAGE),
            _ => None,
        };
        let tree = grammar.and_then(|g| {
            let mut parser = Parser::new();
            parser.set_language(&g.into()).ok()?;
            parser.parse(&data, None)
        });
        Some(Self {
            path: path.into(),
            lang: lang.into(),
            data,
            tree,
            imports: BTreeMap::new(),
        })
    }
    pub fn value(&self, n: Node<'_>) -> &str {
        &self.data[n.byte_range()]
    }
    pub fn field(&self, n: Node<'_>, key: &str) -> &str {
        n.child_by_field_name(key)
            .map(|c| self.value(c))
            .unwrap_or("")
    }
    pub fn location(&self, n: Node<'_>) -> (usize, usize) {
        let p = n.start_position();
        (
            p.row + 1,
            self.data[n.start_byte() - p.column..n.start_byte()]
                .chars()
                .count()
                + 1,
        )
    }
}
pub(super) struct Edit {
    pub site: &'static str,
    pub new: Option<String>,
}
#[derive(Default)]
pub(super) struct Scan {
    pub edits: BTreeMap<(usize, usize), Edit>,
    pub authoritative: bool,
    pub exports: bool,
    pub kept: BTreeSet<usize>,
}
impl Scan {
    pub fn add(&mut self, src: &Source, n: Node<'_>, site: &'static str, new: Option<String>) {
        self.edits
            .entry(src.location(n))
            .or_insert(Edit { site, new });
    }
}
pub(super) fn walk<'a>(root: Node<'a>, mut visit: impl FnMut(Node<'a>) -> bool) {
    let mut cursor = root.walk();
    loop {
        if visit(cursor.node()) && cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}
pub(super) fn parents(n: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(n.parent(), |p| p.parent())
}
pub(super) fn is_field(p: Node<'_>, field: &str, n: Node<'_>) -> bool {
    p.child_by_field_name(field) == Some(n)
}
pub(super) fn within(outer: &Range<usize>, span: &Range<usize>) -> bool {
    outer.start <= span.start && outer.end >= span.end
}
pub(super) fn fallback(
    src: &Source,
    t: &Target,
    new: &str,
    lines: &Lines,
    scoped: bool,
    scan: &mut Scan,
) {
    let home = src.path == t.file;
    let mut need_definition = home && !scan.edits.values().any(|e| e.site == "definition");
    let mut covered = BTreeSet::new();
    if let Some(tree) = &src.tree {
        walk(tree.root_node(), |n| {
            if n.child_count() != 0 {
                return true;
            }
            if !(n.kind().contains("identifier")
                || matches!(n.kind(), "name" | "constant" | "word"))
                || src.value(n) != t.name
            {
                return false;
            }
            let (line, _) = src.location(n);
            if need_definition && t.window(line) {
                scan.add(src, n, "definition", None);
                need_definition = false;
                covered.insert(t.first);
                return false;
            }
            let key = if lines.contains_key(&line) {
                line
            } else {
                let Some(call) = parents(n)
                    .take_while(|a| {
                        !a.kind().contains("argument") && !a.kind().ends_with("statement")
                    })
                    .find(|p| p.kind().contains("call") || p.kind().contains("invocation"))
                else {
                    return false;
                };
                call.start_position().row + 1
            };
            let Some(site) = lines.get(&key) else {
                return false;
            };
            covered.insert(key);
            if *site == "definition" || scan.kept.contains(&n.start_byte()) {
                return false;
            }
            if t.lang == "python"
                && t.member
                && !n
                    .parent()
                    .is_some_and(|p| p.kind() == "attribute" && is_field(p, "attribute", n))
            {
                return false;
            }
            if scoped && js(&t.lang) && n.kind() == "property_identifier" {
                return false;
            }
            let repl =
                (n.kind() == "shorthand_property_identifier").then(|| format!("{}: {new}", t.name));
            scan.add(src, n, site, repl);
            false
        });
        if src.lang == "python" {
            return;
        }
    }
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$');
    let mut offset = 0;
    for (index, line) in src.data.split('\n').enumerate() {
        let row = index + 1;
        let site = if need_definition && t.window(row) {
            Some(&"definition")
        } else {
            lines.get(&row)
        };
        if let Some(site) =
            site.filter(|s| !covered.contains(&row) && (**s != "definition" || need_definition))
        {
            for (col, token) in line.match_indices(&t.name) {
                if line[..col].chars().next_back().is_some_and(word)
                    || line[col + token.len()..].chars().next().is_some_and(word)
                {
                    continue;
                }
                if let Some(tree) = &src.tree
                    && let Some(n) = tree
                        .root_node()
                        .descendant_for_byte_range(offset + col, offset + col + token.len())
                    && std::iter::once(n)
                        .chain(parents(n))
                        .take_while(|a| {
                            !a.kind().contains("interpolation")
                                && !a.kind().contains("substitution")
                        })
                        .any(|a| a.kind().contains("string") || a.kind().contains("comment"))
                {
                    continue;
                }
                scan.edits
                    .entry((row, line[..col].chars().count() + 1))
                    .or_insert(Edit { site, new: None });
                if *site == "definition" {
                    need_definition = false;
                }
                break;
            }
        }
        offset += line.len() + 1;
    }
}
