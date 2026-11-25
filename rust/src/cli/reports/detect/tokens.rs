use anyhow::{Result, ensure};
use serde_json::Value;
use std::path::Path;

pub(super) fn verify(root: &Path, files: &[String], response: &Value) -> Result<(usize, usize)> {
    let encoder = tiktoken_rs::cl100k_base_singleton();
    let markers = encoder.special_tokens();
    let count = |s: &str| -> Result<usize> {
        ensure!(
            !markers.iter().any(|m| s.contains(m)),
            "Encountered text corresponding to a disallowed special token"
        );
        Ok(encoder.count_ordinary(s))
    };
    let mut original = 0;
    for path in files.iter().map(|p| root.join(p)).filter(|p| p.is_file()) {
        if let Ok(bytes) = std::fs::read(path) {
            original += count(
                &String::from_utf8_lossy(&bytes)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n"),
            )?;
        }
    }
    let mut ordered = response.clone();
    ordered.sort_all_objects();
    let compact = crate::cli::wire::ascii(&ordered.to_string(), false);
    Ok((original, count(&compact)?))
}
