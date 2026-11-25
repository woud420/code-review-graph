use super::sources;
use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

struct Symbol {
    name: String,
    start: usize,
    end: usize,
}

#[derive(Default)]
pub(super) struct Targets {
    primary: PathBuf,
    sibling: PathBuf,
    deletable: PathBuf,
    functions: Vec<Symbol>,
    imports: Vec<(PathBuf, usize)>,
}

fn extension(path: &Path) -> &str {
    path.extension().and_then(|x| x.to_str()).unwrap_or("")
}
fn read(path: &Path) -> Result<String> {
    Ok(String::from_utf8_lossy(&fs::read(path)?)
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

pub(super) struct Edit {
    pub target: String,
    pub note: String,
}
impl Edit {
    fn changes(path: &Path, note: impl Into<String>) -> Self {
        Self {
            target: path.display().to_string(),
            note: note.into(),
        }
    }
    fn skip(note: impl Into<String>) -> Self {
        Self {
            target: String::new(),
            note: note.into(),
        }
    }
}

impl Targets {
    pub(super) fn load(db: &Connection, tree: &Path) -> Result<Self> {
        let relative = |s: String| -> Result<PathBuf> {
            Ok(Path::new(&s)
                .strip_prefix(tree)
                .with_context(|| format!("indexed file outside benchmark tree: {s}"))?
                .to_owned())
        };
        let mut symbols = BTreeMap::<PathBuf, Vec<Symbol>>::new();
        let mut stmt=db.prepare("SELECT name,file_path,line_start,line_end FROM nodes WHERE kind='Function' AND is_test=0 AND line_start IS NOT NULL AND line_end IS NOT NULL ORDER BY file_path,line_start")?;
        for r in stmt.query_map([], |r| {
            Ok((
                r.get(1)?,
                Symbol {
                    name: r.get(0)?,
                    start: r.get::<_, u32>(2)? as usize,
                    end: r.get::<_, u32>(3)? as usize,
                },
            ))
        })? {
            let (path, symbol) = r?;
            symbols.entry(relative(path)?).or_default().push(symbol);
        }
        let mut ranked = symbols
            .keys()
            .filter(|p| sources::comment(extension(p)).is_some())
            .cloned()
            .collect::<Vec<_>>();
        ranked.sort_by_key(|p| (std::cmp::Reverse(symbols[p].len()), p.clone()));
        let primary = ranked.first().cloned().unwrap_or_default();
        let sibling = ranked
            .into_iter()
            .skip(1)
            .find(|p| extension(p) == extension(&primary))
            .unwrap_or_default();
        let mut imports = Vec::new();
        for r in db.prepare("SELECT file_path,line FROM edges WHERE kind='IMPORTS_FROM' AND line>0 ORDER BY file_path,line")?.query_map([],|r|Ok((r.get(0)?,r.get::<_,u32>(1)?)))? {
            let(p,line)=r?; imports.push((relative(p)?,line as usize));
        }
        imports.sort_by_key(|(path, line)| {
            (
                if path == &primary {
                    0
                } else if extension(path) == extension(&primary) {
                    1
                } else {
                    2
                },
                path.clone(),
                *line,
            )
        });
        let mut deletable = PathBuf::new();
        let mut stmt=db.prepare("SELECT n.file_path,COUNT(*) AS hits FROM edges e JOIN nodes n ON n.qualified_name=e.target_qualified WHERE e.file_path!=n.file_path GROUP BY n.file_path ORDER BY hits DESC,n.file_path")?;
        for path in stmt.query_map([], |r| r.get(0))? {
            let p = relative(path?)?;
            if sources::comment(extension(&p)).is_some() {
                deletable = p;
                break;
            }
        }
        let functions = symbols.remove(&primary).unwrap_or_default();
        Ok(Self {
            primary,
            sibling,
            deletable,
            functions,
            imports,
        })
    }

    fn trailing(&self, tree: &Path) -> Result<Edit> {
        let Some(token) = sources::comment(extension(&self.primary)) else {
            return Ok(Edit::skip("no source file with a known comment syntax"));
        };
        let path = tree.join(&self.primary);
        let mut text = read(&path)?;
        if !text.ends_with('\n') {
            text.push('\n')
        }
        text.push_str(&format!(
            "{token} incremental-fidelity benchmark: neutral edit\n"
        ));
        fs::write(path, text)?;
        Ok(Edit::changes(&self.primary, "appended one comment line"))
    }

    fn rename(&self, tree: &Path) -> Result<Edit> {
        if !self.primary.as_os_str().is_empty() {
            let path = tree.join(&self.primary);
            let text = read(&path)?;
            let mut lines = text
                .split_inclusive('\n')
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let identifier = Regex::new(r"^(?:\p{XID_Start}|_)\p{XID_Continue}*$")?;
            for symbol in self
                .functions
                .iter()
                .filter(|s| identifier.is_match(&s.name))
            {
                let pattern = Regex::new(&format!(r"\b{}\b", regex::escape(&symbol.name)))?;
                let new = format!("{}_crg_renamed", symbol.name);
                for line in lines
                    .iter_mut()
                    .skip(symbol.start.saturating_sub(1))
                    .take(3)
                {
                    if pattern.is_match(line) {
                        *line = pattern.replace(line, regex::NoExpand(&new)).into_owned();
                        fs::write(path, lines.concat())?;
                        return Ok(Edit::changes(
                            &self.primary,
                            format!("renamed {} to {new}", symbol.name),
                        ));
                    }
                }
            }
        }
        Ok(Edit::skip("no function definition line found to rename"))
    }

    fn delete(&self, tree: &Path) -> Result<Edit> {
        if self.deletable.as_os_str().is_empty() {
            return Ok(Edit::skip("no depended-on file to delete"));
        }
        let path = tree.join(&self.deletable);
        if !path.is_file() {
            return Ok(Edit::skip(format!(
                "{} is not a file",
                self.deletable.display()
            )));
        }
        fs::remove_file(path)?;
        Ok(Edit::changes(&self.deletable, "deleted a depended-on file"))
    }

    fn add(&self, tree: &Path) -> Result<Edit> {
        let suffix = extension(&self.primary);
        let Some(source) = sources::probe(suffix) else {
            return Ok(Edit::skip(format!(
                "no probe source for {}",
                if suffix.is_empty() {
                    "unknown suffix".into()
                } else {
                    format!(".{suffix}")
                }
            )));
        };
        let path = self
            .primary
            .with_file_name(format!("crg_fidelity_probe.{suffix}"));
        fs::write(tree.join(&path), source)?;
        Ok(Edit::changes(&path, "added a new source file"))
    }

    fn move_function(&self, tree: &Path) -> Result<Edit> {
        if self.primary.as_os_str().is_empty() || self.sibling.as_os_str().is_empty() {
            return Ok(Edit::skip("need two same-language files to move between"));
        }
        let path = tree.join(&self.primary);
        let text = read(&path)?;
        let mut lines = text.split_inclusive('\n').collect::<Vec<_>>();
        let Some(symbol) = self.functions.iter().rev().find(|s| {
            s.start >= 1
                && s.end > s.start
                && s.end <= lines.len()
                && !lines[s.start - 1].starts_with(char::is_whitespace)
        }) else {
            return Ok(Edit::skip("no top-level multi-line function to move"));
        };
        let block = lines
            .drain(symbol.start - 1..symbol.end)
            .collect::<String>();
        fs::write(path, lines.concat())?;
        let file = tree.join(&self.sibling);
        let mut tail = read(&file)?;
        if !tail.is_empty() && !tail.ends_with('\n') {
            tail.push('\n')
        }
        fs::write(file, format!("{tail}\n\n{block}"))?;
        Ok(Edit {
            target: format!("{} -> {}", self.primary.display(), self.sibling.display()),
            note: format!("moved {}", symbol.name),
        })
    }

    fn change_import(&self, tree: &Path) -> Result<Edit> {
        for (rel, line) in &self.imports {
            let path = tree.join(rel);
            if !path.is_file() {
                continue;
            }
            let text = read(&path)?;
            let mut lines = text.split_inclusive('\n').collect::<Vec<_>>();
            if *line == 0 || *line > lines.len() {
                continue;
            }
            let content = lines[*line - 1].trim();
            if content.is_empty() || content.ends_with(['(', '\\', '{', ',']) {
                continue;
            }
            lines.remove(*line - 1);
            fs::write(path, lines.concat())?;
            return Ok(Edit::changes(rel, format!("removed import at line {line}")));
        }
        Ok(Edit::skip("no single-line import statement found"))
    }

    pub(super) fn apply(&self, tree: &Path, kind: &str) -> Result<Edit> {
        match kind {
            "trailing_comment" | "revert" => self.trailing(tree),
            "rename_function" => self.rename(tree),
            "delete_file" => self.delete(tree),
            "add_file" => self.add(tree),
            "move_function" => self.move_function(tree),
            "change_import" => self.change_import(tree),
            _ => anyhow::bail!("unknown edit {kind}"),
        }
    }
}
