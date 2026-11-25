use std::path::Path;

pub(crate) fn absolute_module(
    from: &str,
    module: &str,
    exists: impl Fn(&str) -> bool,
) -> Option<String> {
    if module.is_empty()
        || module
            .split('.')
            .any(|v| v.is_empty() || !v.chars().all(|c| c == '_' || c.is_alphanumeric()))
    {
        return None;
    }
    let tail = module.replace('.', "/");
    let standard = include_str!("../canonical/imports/stdlib-modules.txt")
        .split_whitespace()
        .any(|s| module.split('.').next() == Some(s));
    for directory in Path::new(from).parent()?.ancestors() {
        if standard && exists(directory.join("__init__.py").to_str()?) {
            continue;
        }
        let base = directory.join(&tail);
        for file in [base.join("__init__.py"), base.with_extension("py")] {
            let candidate = file.to_str()?;
            if exists(candidate) {
                return Some(candidate.into());
            }
        }
    }
    None
}
