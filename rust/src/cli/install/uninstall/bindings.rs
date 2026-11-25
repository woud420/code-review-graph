use super::super::platforms::{Form::*, paths};
use super::{formats, report::Report};
use std::{collections::BTreeSet, path::Path};
pub(super) fn sweep(report: &mut Report, base: &Path, repo: bool, platform: &str) {
    let home_or_repo = base;
    let specs = paths(base, repo);
    let mut seen = BTreeSet::new();
    for s in specs {
        if platform != "all" && s.owner != platform {
            continue;
        }
        let path = s.root.join(s.path);
        if !seen.insert((path.clone(), s.key)) {
            continue;
        }
        let boundary = if path.starts_with(home_or_repo) {
            home_or_repo
        } else {
            &s.root
        };
        let Some(raw) = report.read(&path, boundary) else {
            continue;
        };
        let (result, detail) = match s.form {
            Object | Array => (
                formats::json(&raw, s.key, matches!(s.form, Array)),
                format!("remove 'code-review-graph' from '{}'", s.key),
            ),
            Toml => (
                formats::toml(&raw, s.key),
                format!("remove [{}.code-review-graph]", s.key),
            ),
            Yaml => (
                formats::yaml(&raw, false),
                format!("remove {}.code-review-graph", s.key),
            ),
            Patch => (
                formats::yaml(&raw, true),
                "remove the mcp-code-review-graph insert".to_owned(),
            ),
        };
        match result {
            Ok(Some(out)) => report.write(&path, boundary, &raw, &out, &detail),
            Ok(None) => {}
            Err(err) => report.skip(&path, &err.to_string()),
        }
    }
}
