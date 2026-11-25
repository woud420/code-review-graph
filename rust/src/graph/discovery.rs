use anyhow::{Context, Result};
use ignore::{
    WalkBuilder,
    gitignore::{Gitignore, GitignoreBuilder},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub struct Inventory {
    pub files: Vec<String>,
    pub directories: BTreeSet<PathBuf>,
    pub followed_files: BTreeSet<String>,
    pub external_folders: BTreeMap<PathBuf, PathBuf>,
    pub stamps: BTreeMap<PathBuf, (SystemTime, u64)>,
}

fn supported(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let json = ext == "json"
        && !matches!(
            name,
            "package-lock.json" | "npm-shrinkwrap.json" | "project.assets.json"
        )
        && !name.ends_with(".lock.json")
        && !name.ends_with(".min.json");
    super::JS_EXTENSIONS.contains(&ext)
        || matches!(
            ext,
            "py" | "rs"
                | "go"
                | "java"
                | "cs"
                | "sol"
                | "lua"
                | "luau"
                | "php"
                | "yaml"
                | "yml"
                | "tf"
                | "hcl"
                | "rb"
        )
        || crate::parser::extensions::language(ext).is_some()
        || json
        || crate::parser::cfamily::supports(ext)
        || crate::parser::bash::discover(path)
        || crate::parser::sql::supports(ext)
        || (ext == "properties" && crate::parser::yaml::spring::supports(path))
}

const VENDOR: &[&str] = &[
    "Cargo.toml",
    "Gemfile",
    "Rakefile",
    "go.mod",
    "go.work",
    "composer.json",
];

fn has_marker(parent: &Path, name: &str) -> bool {
    let markers: &[&str] = match name {
        "target" => &["pom.xml", "Cargo.toml", "build.sbt"],
        "build" => &[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ],
        ".next" => &[
            "next.config.js",
            "next.config.mjs",
            "next.config.cjs",
            "next.config.ts",
        ],
        ".nuxt" => &["nuxt.config.js", "nuxt.config.mjs", "nuxt.config.ts"],
        "vendor" => VENDOR,
        _ => return false,
    };
    markers.iter().any(|n| parent.join(n).is_file())
        || (name == "vendor"
            && fs::read_dir(parent).is_ok_and(|mut entries| {
                entries
                    .any(|e| e.is_ok_and(|f| f.path().extension().is_some_and(|e| e == "gemspec")))
            }))
}

struct Rules {
    explicit: Gitignore,
    keep: BTreeSet<PathBuf>,
    nested_depth: usize,
}

impl Rules {
    fn excluded(&self, root: &Path, relative: &Path, is_dir: bool) -> bool {
        if self
            .explicit
            .matched(root.join(relative), is_dir)
            .is_ignore()
        {
            return true;
        }
        if !is_dir || self.keep.contains(relative) {
            return false;
        }
        let depth = relative.components().count();
        depth >= 2
            && depth <= self.nested_depth.saturating_add(1)
            && relative
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| {
                    has_marker(&root.join(relative.parent().unwrap_or(Path::new(""))), n)
                })
    }
}

fn ignores(root: &Path) -> Result<Rules> {
    let mut rules = GitignoreBuilder::new(root);
    for name in [
        ".git/",
        ".svn/",
        ".code-review-graph/",
        ".code-review-graph-rs/",
        "node_modules/",
        "__pycache__/",
        ".venv/",
        "venv/",
        ".bundle/",
        ".gradle/",
        ".dart_tool/",
        ".pub-cache/",
        "cdk.out/",
        ".cache/",
        "/target/",
        "/dist/",
        "/build/",
        "/bin/",
        "/obj/",
        "/.next/",
        "/.nuxt/",
        "/storage/",
        "/bootstrap/cache/",
        "/public/build/",
        "/coverage/",
        "/.tmp/",
        "/tmp/",
        "*.min.js",
        "*.lock.json",
        "*.min.json",
        "pnpm-lock.yaml",
    ] {
        rules.add_line(None, name)?;
    }
    if has_marker(root, "vendor") {
        rules.add_line(None, "vendor/")?;
    }
    let mut keep = BTreeSet::new();
    if let Ok(text) = fs::read_to_string(root.join(".code-review-graphignore")) {
        for line in text.lines().map(str::trim) {
            if let Some(path) = line.strip_prefix('!') {
                keep.insert(PathBuf::from(path.trim().trim_matches('/')));
            } else {
                rules.add_line(None, line)?;
            }
        }
    }
    let nested_depth = if std::env::var("CRG_NESTED_OUTPUT_SCAN").as_deref() == Ok("0") {
        0
    } else {
        std::env::var("CRG_MODULE_SCAN_DEPTH")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(3)
    };
    Ok(Rules {
        explicit: rules.build()?,
        keep,
        nested_depth,
    })
}

pub fn follow_links(explicit: Option<bool>) -> bool {
    explicit.unwrap_or_else(|| {
        std::env::var("CRG_FOLLOW_SYMLINKS").is_ok_and(|s| {
            matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            )
        })
    })
}

impl Inventory {
    pub(super) fn source_files(root: &Path) -> Result<Vec<String>> {
        Ok(Self::for_index(root, None)?.files)
    }

    pub(crate) fn for_index(root: &Path, follow: Option<bool>) -> Result<Self> {
        Self::scan(root, follow_links(follow), false)
    }

    pub fn discover(root: &Path, follow: bool) -> Result<Self> {
        Self::scan(root, follow, true)
    }

    fn scan(root: &Path, follow: bool, record_stamps: bool) -> Result<Self> {
        let rules = std::sync::Arc::new(ignores(root)?);
        let mut files = BTreeSet::new();
        let mut followed_files = BTreeSet::new();
        let mut external_folders = BTreeMap::new();
        let mut directories = BTreeSet::new();
        let mut stamps = BTreeMap::new();
        let mut links = VecDeque::from([(root.to_owned(), PathBuf::new())]);
        let limit = std::env::var("CRG_MAX_FOLLOWED_FILES")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(10000)
            .max(1);
        let mut followed = 0;
        while let Some((physical, logical)) = links.pop_front() {
            if directories.contains(&physical) {
                continue;
            }
            if !logical.as_os_str().is_empty() && !physical.starts_with(root) {
                external_folders.insert(physical.clone(), logical.clone());
            }
            let (base, prefix, ignored) = (physical.clone(), logical.clone(), rules.clone());
            let root_name = root.to_owned();
            let walker = WalkBuilder::new(&physical)
                .hidden(false)
                .require_git(false)
                .sort_by_file_name(|a, b| a.cmp(b))
                .filter_entry(move |entry| {
                    let Ok(suffix) = entry.path().strip_prefix(&base) else {
                        return false;
                    };
                    !ignored.excluded(
                        &root_name,
                        &prefix.join(suffix),
                        entry.file_type().is_some_and(|e| e.is_dir()),
                    )
                })
                .build();
            for entry in walker {
                if !logical.as_os_str().is_empty() && followed >= limit {
                    break;
                }
                let entry = entry?;
                let p = entry.path();
                let path = logical.join(p.strip_prefix(&physical)?);
                let kind = entry.file_type();
                if kind.is_some_and(|t| t.is_dir()) {
                    directories.insert(p.to_owned());
                } else if kind.is_some_and(|t| t.is_symlink()) && follow {
                    if let Ok(real) = p.canonicalize()
                        && real.is_dir()
                        && !p.starts_with(&real)
                        && !root.starts_with(&real)
                    {
                        let logical_target = real.strip_prefix(root).unwrap_or(&path);
                        if !rules.excluded(root, logical_target, true) {
                            links.push_back((real.clone(), logical_target.to_owned()));
                        }
                    }
                } else if kind.is_some_and(|t| t.is_file()) {
                    if !logical.as_os_str().is_empty() {
                        followed += 1;
                    }
                    let is_source = supported(p);
                    let control = p.extension().is_some_and(|ext| ext == "csproj")
                        || p.file_name()
                            .is_some_and(|s| s == ".code-review-graphignore" || s == ".gitignore");
                    if is_source {
                        let name = path
                            .to_str()
                            .context("source path is not UTF-8")?
                            .replace(std::path::MAIN_SEPARATOR, "/");
                        if !logical.as_os_str().is_empty() {
                            followed_files.insert(name.clone());
                        }
                        files.insert(name);
                    }
                    if record_stamps
                        && (is_source || control)
                        && let Ok(m) = p.metadata()
                    {
                        stamps.insert(path, (m.modified()?, m.len()));
                    }
                }
            }
        }
        Ok(Self {
            files: files.into_iter().collect(),
            followed_files,
            external_folders,
            directories,
            stamps,
        })
    }
}
