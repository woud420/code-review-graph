use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path};
use unicode_normalization::UnicodeNormalization;

fn slug(name: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for ch in name.nfkd().filter(char::is_ascii) {
        if ch.is_ascii_alphanumeric() {
            if separator && !result.is_empty() {
                result.push('-');
            }
            result.push(ch.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    result.truncate(80);
    if result.is_empty() {
        "unnamed".to_owned()
    } else {
        result
    }
}

pub fn read(db: &Path, name: &str, limit: i64) -> Result<Value> {
    ensure!(
        limit >= 1,
        "max_chars must be an integer greater than or equal to 1"
    );
    let wiki = db.with_file_name("wiki");
    let slug = slug(name);
    let first = wiki.join(format!("{slug}.md"));
    let canonical = wiki.canonicalize().ok();
    let contained = |p: &Path| {
        p.canonicalize()
            .ok()
            .filter(|s| s.is_file() && canonical.as_ref().is_some_and(|root| s.starts_with(root)))
    };
    let page = contained(&first)
        .or_else(|| contained(&wiki.join(name)))
        .or_else(|| {
            fs::read_dir(&wiki)
                .ok()?
                .filter_map(Result::ok)
                .map(|f| f.path())
                .filter(|p| {
                    p.extension().is_some_and(|e| e == "md")
                        && p.file_stem()
                            .and_then(|v| v.to_str())
                            .is_some_and(|s| s.contains(&slug))
                })
                .find_map(|p| contained(&p))
        });
    let Some(page) = page else {
        return Ok(
            json!({"status":"not_found","summary":format!("No wiki page found for '{name}'.")}),
        );
    };
    let bytes = fs::read(page)?;
    let content = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let total = content.chars().count();
    let cap = limit.min(80000) as usize;
    let truncated = total > cap;
    let mut summary = format!("Wiki page for '{name}' ({total} chars)");
    if truncated {
        summary += &format!(", showing first {cap}");
    }
    Ok(
        json!({"status":"ok","summary":summary,"content":content.chars().take(cap).collect::<String>(),"total_chars":total,"truncated":truncated}),
    )
}
