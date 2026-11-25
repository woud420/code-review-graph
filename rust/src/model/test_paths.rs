use regex::RegexSet;
use std::{path::Path, sync::LazyLock};

pub fn test_file(file: &str, repo: Option<&str>) -> bool {
    let path = Path::new(file);
    if path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| TEST_FILENAME.is_match(s))
    {
        return true;
    }
    let relative = if path.is_absolute() {
        repo.and_then(|r| path.strip_prefix(r).ok())
    } else {
        Some(path)
    };
    let Some(relative) = relative else {
        return false;
    };
    if TEST_DIR.is_match(&relative.to_string_lossy()) {
        return true;
    }
    if let Some(dir) = relative
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "test"))
    {
        let parent = dir.parent().unwrap_or(Path::new(""));
        let shipped = path.extension().is_some_and(|e| e == "py")
            && !parent.as_os_str().is_empty()
            && repo.is_some_and(|root| {
                Path::new(root).join(dir).join("__init__.py").is_file()
                    && Path::new(root).join(parent).join("__init__.py").is_file()
            });
        return !shipped;
    }
    false
}

static TEST_FILENAME: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r#"^test_.*\.py$"#,
        r#"_test\.py$"#,
        r#"^conftest\.py$"#,
        r#"\.(?:test|spec)\.[cm]?[jt]sx?$"#,
        r#"\.stories\.[jt]sx?$"#,
        r#"_test\.go$"#,
        r#"_(?:test|spec)\.rb$"#,
        r#"[a-z0-9]Tests?\.(?:java|kt|kts|scala|groovy)$"#,
        r#"[a-z0-9]Tests?\.cs$"#,
        r#"[a-z0-9]Test\.php$"#,
        r#"_test\.dart$"#,
        r#"^test[_-].*\.[rR]$"#,
        r#"(?i)\.tests\.ps1$"#,
        r#"[_.]test\.resi?$"#,
        r#"^runtests\.jl$"#,
    ])
    .expect("valid built-in patterns")
});
static TEST_DIR: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r#"(?:^|[\\/])tests[\\/]"#,
        r#"(?:^|[\\/])__tests__[\\/]"#,
        r#"(?:^|[\\/])e2e[_-]?tests?[\\/]"#,
        r#"(?:^|[\\/])testthat[\\/]"#,
        r#"(?:^|[\\/])specs?[\\/].*\.(?:rb|rake)$"#,
    ])
    .expect("valid built-in patterns")
});
