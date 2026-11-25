use super::children;
use regex::Regex;
use std::{borrow::Cow, sync::LazyLock};
use tree_sitter::Node;

static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?ms)//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|(?m:^[\t ]*\#[^\n]*(?:\\\n[^\n]*)*)|\b(?:QT_BEGIN_NAMESPACE|QT_END_NAMESPACE|Q_OBJECT|Q_SIGNALS|Q_SLOTS|Q_EMIT)\b"#).unwrap()
});

pub(super) fn qt(source: &str) -> Cow<'_, str> {
    if !source.contains("Q_") && !source.contains("QT_") {
        return Cow::Borrowed(source);
    }
    let mut bytes = None;
    for token in TOKENS.find_iter(source) {
        if !token.as_str().starts_with('Q') {
            continue;
        }
        let out = bytes.get_or_insert_with(|| source.as_bytes().to_vec());
        out[token.range()].fill(b' ');
        if token.as_str() == "Q_SIGNALS" {
            out[token.start()..token.start() + 6].copy_from_slice(b"public");
        }
    }
    bytes.map_or(Cow::Borrowed(source), |b| {
        Cow::Owned(String::from_utf8(b).expect("only ASCII tokens masked"))
    })
}

pub(super) fn dead(parent: Node<'_>, child: Node<'_>, source: &str) -> bool {
    matches!(parent.kind(), "preproc_if" | "preproc_elif")
        && parent.child_by_field_name("condition").is_some_and(|n| {
            source[n.byte_range()]
                .trim()
                .trim_matches(['(', ')'])
                .trim()
                == "0"
        })
        && parent.child_by_field_name("alternative") != Some(child)
}

pub(super) fn cpp_tree(root: Node<'_>, source: &str) -> bool {
    let mut work = vec![root];
    while let Some(n) = work.pop() {
        if n.is_error()
            || n.is_missing()
            || n.prev_named_sibling()
                .is_some_and(|p| p.is_error() && p.end_byte() == n.start_byte())
        {
            continue;
        }
        if matches!(
            n.kind(),
            "access_specifier"
                | "alias_declaration"
                | "base_class_clause"
                | "class_specifier"
                | "concept_definition"
                | "lambda_expression"
                | "namespace_definition"
                | "noexcept"
                | "template_declaration"
                | "trailing_return_type"
                | "using_declaration"
        ) || n.kind() == "enum_specifier"
            && n.children(&mut n.walk())
                .any(|c| matches!(c.kind(), "class" | "struct"))
            || n.kind() == "type_qualifier"
                && matches!(&source[n.byte_range()], "constinit" | "consteval")
        {
            return true;
        }
        work.extend(children(n).into_iter().filter(|&c| !dead(n, c, source)));
    }
    false
}

pub(super) fn potential(source: &str) -> bool {
    [
        "class",
        "namespace",
        "template",
        "concept",
        "using",
        "public",
        "private",
        "protected",
        "consteval",
        "constinit",
        "noexcept",
        "enum",
        ":",
        "]",
        "->",
    ]
    .iter()
    .any(|word| source.contains(word))
}
