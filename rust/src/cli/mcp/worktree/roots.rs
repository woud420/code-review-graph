use anyhow::{Result, anyhow, bail, ensure};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

async fn git(root: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .kill_on_drop(true)
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_SYSTEM", null)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let failed = || anyhow!("Git command failed: {}", args.join(" "));
    let mut child = command.spawn().map_err(|_| failed())?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut out = child
        .stdout
        .take()
        .expect("piped stdout")
        .take(4 * 1024 * 1024 + 1);
    let mut err = child
        .stderr
        .take()
        .expect("piped stderr")
        .take(64 * 1024 + 1);
    let (status, _, _) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(
            child.wait(),
            out.read_to_end(&mut stdout),
            err.read_to_end(&mut stderr)
        )
    })
    .await
    .map_err(|_| failed())?
    .map_err(|_| failed())?;
    ensure!(
        stdout.len() <= 4 * 1024 * 1024 && stderr.len() <= 64 * 1024,
        "{}",
        failed()
    );
    if !status.success() {
        let detail = String::from_utf8_lossy(if stderr.is_empty() { &stdout } else { &stderr });
        if detail.trim().is_empty() {
            return Err(failed());
        }
        bail!("{} ({})", failed(), detail.trim());
    }
    Ok(String::from_utf8_lossy(&stdout).trim().into())
}

pub(super) fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolve(parent).join(name),
        _ => path.to_owned(),
    }
}

async fn identity(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let expanded = if path.starts_with("~") {
        std::env::home_dir()
            .unwrap_or_default()
            .join(path.strip_prefix("~")?)
    } else {
        path.to_owned()
    };
    ensure!(
        expanded.is_absolute(),
        "repo_root cannot be resolved: repo_root must be an absolute path"
    );
    ensure!(
        !expanded.as_os_str().to_string_lossy().contains('\0'),
        "repo_root cannot be resolved: lstat: embedded null character in path"
    );
    let candidate = resolve(&expanded);
    ensure!(
        candidate.is_dir(),
        "repo_root is not a directory: {}",
        candidate.display()
    );
    let root =
        PathBuf::from(git(&candidate, &["rev-parse", "--show-toplevel"]).await?).canonicalize()?;
    let common = resolve(&root.join(git(&root, &["rev-parse", "--git-common-dir"]).await?));
    Ok((root, common))
}

pub(super) struct Worktrees {
    pub(super) primary: PathBuf,
    common: PathBuf,
}
impl Worktrees {
    pub(super) async fn new(path: &Path) -> Result<Self> {
        let (primary, common) = identity(path).await?;
        Ok(Self { primary, common })
    }
    pub(super) async fn authorize(&self, path: &str) -> Result<PathBuf> {
        ensure!(
            !path.is_empty(),
            "repo_root must be a non-empty absolute path"
        );
        let (root, common) = identity(Path::new(path)).await?;
        if root == self.primary {
            return Ok(root);
        }
        ensure!(
            common == self.common,
            "repo_root must be the primary repository or a linked worktree"
        );
        let listing = git(&self.primary, &["worktree", "list", "--porcelain"]).await?;
        ensure!(
            listing
                .lines()
                .filter_map(|l| l.strip_prefix("worktree "))
                .any(|p| Path::new(p.trim())
                    .canonicalize()
                    .is_ok_and(|real| real == root)),
            "repo_root is not an active linked worktree of the primary repository"
        );
        Ok(root)
    }
}
