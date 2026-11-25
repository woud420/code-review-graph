use super::super::jsonc;
use anyhow::{Context, Result, bail, ensure};
use tree_sitter::{Node, Parser};

const NAME: &str = "code-review-graph";
pub fn json(raw: &str, key: &str, array: bool) -> Result<Option<String>> {
    let doc = jsonc::parse(raw)?;
    ensure!(doc.value.is_object(), "expected an object");
    let Some(node) = doc.member(key) else {
        return Ok(None);
    };
    let mut expected = doc.value.clone();
    let mut cuts = Vec::new();
    if array {
        let items = expected[key].as_array_mut().context("expected an array")?;
        for (i, it) in items.iter().enumerate() {
            if it["name"] == NAME {
                node.cut(i, &mut cuts)
            }
        }
        items.retain(|it| it["name"] != NAME);
    } else {
        let map = expected[key]
            .as_object_mut()
            .context("expected an object")?;
        if map.remove(NAME).is_none() {
            return Ok(None);
        }
        if let Some(i) = node.children.iter().position(|it| it.key == NAME) {
            node.cut(i, &mut cuts)
        }
    }
    if cuts.is_empty() {
        return Ok(None);
    }
    Ok(Some(jsonc::splice(raw, cuts, &expected)?))
}
pub fn toml(raw: &str, key: &str) -> Result<Option<String>> {
    let mut document: toml::Table = toml::from_str(raw)?;
    let lines: Vec<_> = raw.split_inclusive('\n').collect();
    let Some(i) = lines.iter().position(|s| {
        let t = s.trim();
        t.strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .is_some_and(|s| {
                s.split('.')
                    .map(|p| p.trim().trim_matches(['\'', '"']))
                    .eq([key, NAME])
            })
    }) else {
        return Ok(None);
    };
    let mut end = i + 1;
    for (j, s) in lines.iter().enumerate().skip(i + 1) {
        let t = s.trim();
        if t.starts_with('[') {
            break;
        }
        if !t.is_empty() && !t.starts_with('#') {
            end = j + 1
        }
    }
    while end < lines.len() && lines[end].trim().is_empty() {
        end += 1
    }
    let text = lines[..i].concat() + &lines[end..].concat();
    let result: toml::Table = toml::from_str(&text)?;
    if let Some(table) = document.get_mut(key).and_then(|v| v.as_table_mut()) {
        table.remove(NAME);
        if table.is_empty() && !result.contains_key(key) {
            document.remove(key);
        }
    }
    ensure!(document == result, "TOML edit changed unrelated settings");
    Ok(Some(text))
}
fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.named_children(&mut node.walk())
        .filter(|c| c.kind() != "comment")
        .collect()
}
fn unwrapped(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "stream" | "document" | "block_node" | "flow_node" | "block_sequence_item"
    ) {
        let c = children(node);
        if c.len() != 1 {
            break;
        }
        node = c[0];
    }
    node
}
fn field<'t>(pair: Node<'t>, raw: &str, key: &str) -> Option<Node<'t>> {
    (pair
        .child_by_field_name("key")?
        .utf8_text(raw.as_bytes())
        .ok()?
        .trim_matches(['\'', '"'])
        == key)
        .then(|| pair.child_by_field_name("value"))
        .flatten()
}
fn get<'t>(n: Node<'t>, s: &str, key: &str) -> Option<Node<'t>> {
    children(unwrapped(n))
        .into_iter()
        .find_map(|p| field(p, s, key))
}
fn val(n: Node<'_>, s: &str, key: &str) -> bool {
    n.utf8_text(s.as_bytes())
        .is_ok_and(|v| v.trim_matches(['\'', '"']) == key)
}
pub fn yaml(raw: &str, dsh: bool) -> Result<Option<String>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_yaml::LANGUAGE.into())?;
    let tree = parser.parse(raw, None).context("YAML parser unavailable")?;
    let root = unwrapped(tree.root_node());
    ensure!(
        !tree.root_node().has_error(),
        "YAML parse failed; left unchanged"
    );
    let lines: Vec<_> = raw.split_inclusive('\n').collect();
    let mut empty_layer = false;
    let target = if dsh {
        if !matches!(root.kind(), "block_sequence" | "flow_sequence") {
            return Ok(None);
        }
        let ops = children(root);
        let mut targets = vec![];
        for op in &ops {
            let Some(insert) = get(*op, raw, "insert") else {
                continue;
            };
            let rows = children(unwrapped(insert));
            for row in &rows {
                let registered = get(*row, raw, "id")
                    .is_some_and(|n| val(n, raw, "mcp-code-review-graph"))
                    || (get(*row, raw, "name")
                        .is_some_and(|n| val(n, raw, "@deepseek-ai/dsh-mcp-client"))
                        && get(*row, raw, "config")
                            .and_then(|n| get(n, raw, "serverName"))
                            .is_some_and(|n| val(n, raw, NAME)));
                if registered {
                    ensure!(
                        rows.len() == 1 && children(unwrapped(*op)).len() == 1,
                        "the code-review-graph row shares an insert with other rows; left unchanged, remove it by hand"
                    );
                    targets.push(*op)
                }
            }
        }
        if targets.is_empty() {
            return Ok(None);
        }
        ensure!(
            targets.len() == 1,
            "the code-review-graph row shares an insert with other rows; left unchanged, remove it by hand"
        );
        ensure!(
            root.kind() == "block_sequence",
            "cannot locate insert lines"
        );
        empty_layer = ops.len() == 1;
        targets[0]
    } else {
        let Some(container) = get(root, raw, "mcp_servers") else {
            return Ok(None);
        };
        let mapping = unwrapped(container);
        let Some(pair) = children(mapping)
            .into_iter()
            .find(|n| field(*n, raw, NAME).is_some())
        else {
            return Ok(None);
        };
        ensure!(
            mapping.kind() == "block_mapping",
            "mcp_servers is not a block mapping; left unchanged"
        );
        ensure!(
            lines[pair.start_position().row]
                .trim_start()
                .starts_with("code-review-graph:"),
            "could not locate code-review-graph block; left unchanged"
        );
        pair
    };
    let start = target.start_position().row;
    let mut end = target.end_position().row + usize::from(target.end_position().column > 0);
    if !dsh {
        let indent = target.start_position().column;
        end = start + 1;
        while end < lines.len() {
            let line = lines[end];
            if !line.trim().is_empty() && line.len() - line.trim_start().len() <= indent {
                break;
            }
            end += 1;
        }
    }
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1
    }
    if lines[start][..target.start_position().column]
        .trim()
        .is_empty()
    {
        let mut out = lines[..start].concat() + &lines[end..].concat();
        if empty_layer {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n')
            }
            out.push_str("[]\n")
        }
        ensure!(
            parser
                .parse(&out, None)
                .is_some_and(|t| !t.root_node().has_error()),
            "safe YAML edit failed; left unchanged"
        );
        Ok(Some(out))
    } else {
        bail!("cannot locate complete YAML block; left unchanged")
    }
}
