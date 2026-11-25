use crate::model::{Call, ParsedFile, Symbol};
use std::io::Read;
use std::path::{Component, Path};
use tree_sitter::Node;

pub(crate) fn supports(path: &Path, head: &[u8]) -> bool {
    if let Some(ext) = path.extension() {
        return matches!(ext.to_str(), Some("sh" | "bash" | "zsh" | "ksh"));
    }
    let Some(line) = head
        .get(..head.len().min(256))
        .and_then(|bytes| bytes.split(|b| matches!(b, b'\n' | 0)).next())
        .and_then(|s| s.strip_prefix(b"#!"))
        .and_then(|s| std::str::from_utf8(s).ok())
    else {
        return false;
    };
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let interpreter = if first == "env" || first.ends_with("/env") {
        words.find(|word| !word.starts_with('-')).unwrap_or("")
    } else {
        first
    };
    matches!(
        interpreter.rsplit('/').next(),
        Some("bash" | "sh" | "zsh" | "ksh" | "dash" | "ash")
    )
}

pub(crate) fn discover(path: &Path) -> bool {
    if path.extension().is_some() {
        return supports(path, &[]);
    }
    let mut head = [0; 256];
    std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .is_ok_and(|n| supports(path, &head[..n]))
}

pub(crate) fn sourced_file(from: &str, module: &str) -> Option<String> {
    let mut relative = Path::new(from).parent()?.to_owned();
    for component in Path::new(module).components() {
        match component {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            Component::ParentDir if relative.pop() => {}
            _ => return None,
        }
    }
    Some(relative.to_string_lossy().into_owned())
}

pub(crate) fn extract(root: Node<'_>, source: &str) -> ParsedFile {
    let mut facts = ParsedFile::default();
    let mut stack = vec![(root, "<module>")];
    while let Some((node, mut caller)) = stack.pop() {
        let line = node.start_position().row + 1;
        if node.kind() == "function_definition"
            && let Some(name) = node.child_by_field_name("name")
        {
            let name = &source[name.byte_range()];
            let test = name.starts_with("test_")
                || name.starts_with("Test")
                || name.ends_with("_test")
                || name.ends_with("_spec");
            facts.symbols.push(Symbol {
                name: name.into(),
                kind: if test { "Test" } else { "Function" }.into(),
                start_line: line,
                end_line: node.end_position().row + 1,
            });
            caller = name;
        } else if node.kind() == "command"
            && let Some(command) = node.child_by_field_name("name")
        {
            let name = source[command.byte_range()].trim();
            if matches!(name, "." | "source") {
                let mut cursor = node.walk();
                let target = node
                    .children_by_field_name("argument", &mut cursor)
                    .find(|n| matches!(n.kind(), "word" | "raw_string" | "string"));
                if let Some(arg) = target {
                    let word = source[arg.byte_range()].trim();
                    let unquote = word
                        .strip_prefix('\'')
                        .and_then(|s| s.strip_suffix('\''))
                        .or_else(|| word.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                        .unwrap_or(word);
                    if !unquote.is_empty() {
                        facts.bash_imports.push((line, unquote.to_owned()));
                        continue;
                    }
                }
            }
            if !name.is_empty() {
                facts.calls.push(Call {
                    caller: caller.to_owned(),
                    callee: name.to_owned(),
                    line,
                });
            }
        }
        let mut c = node.walk();
        let children = node.named_children(&mut c).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev().map(|n| (n, caller)));
    }
    facts
}

#[cfg(test)]
mod tests {
    use crate::canonical::{index, postprocess::Level};
    use anyhow::Result;
    use rusqlite::Connection;
    use std::fs;

    #[test]
    fn sourced_function_renames_clear_old_call_targets_from_an_extensionless_script() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let db_path = root.join(".code-review-graph/graph.db");
        fs::write(root.join("library.bash"), "work_now() { echo done; }\n")?;
        fs::write(
            root.join("deploy"),
            "#!/usr/bin/env -S bash -eu\n. './library.bash'\nstart() { work_now; }\nstart\n",
        )?;
        index(&root, &db_path, true, Level::Full, false)?;
        let db = Connection::open(&db_path)?;
        let target = || -> Result<(String, String)> {
            Ok(db.query_row("SELECT target_qualified,confidence_tier FROM edges WHERE source_qualified LIKE '%/deploy::start' AND kind='CALLS'",[],|r|Ok((r.get(0)?,r.get(1)?)))?)
        };
        assert_eq!(
            target()?,
            (
                format!("{}::work_now", root.join("library.bash").display()),
                "INFERRED".into()
            )
        );
        fs::write(root.join("library.bash"), "renamed() { echo done; }\n")?;
        index(&root, &db_path, false, Level::Full, false)?;
        assert_eq!(target()?, ("work_now".into(), "UNRESOLVED".into()));
        Ok(())
    }
}
