use super::{Finding, UPGRADE, VERSION, mismatch};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub(super) const NOT_RUN: &str = "doctor does not run a program inside the repository or named by a relative path, because the repository chose it";

fn candidates(name: &str) -> Vec<PathBuf> {
    let mut names = vec![name.to_owned()];
    if cfg!(windows) && Path::new(name).extension().is_none() {
        names = env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| format!("{name}{}", s.to_lowercase()))
            .collect();
    }
    let mut seen = HashSet::new();
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .filter(|d| !d.as_os_str().is_empty())
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .filter(|p| {
            let Ok(meta) = fs::metadata(p) else {
                return false;
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 == 0 {
                    return false;
                }
            }
            meta.is_file() && p.canonicalize().is_ok_and(|real| seen.insert(real))
        })
        .collect()
}

pub(super) fn launcher(token: &str) -> &str {
    let name = token.rsplit(['/', '\\']).next().unwrap_or(token);
    if name.to_ascii_lowercase().ends_with(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}
pub(super) fn python(name: &str) -> bool {
    Regex::new(r"^pythonw?[0-9]*(?:\.[0-9]+)*$")
        .unwrap()
        .is_match(name)
}
pub(super) fn upgrade(path: &Path) -> &'static str {
    let name = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_owned())
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    if name.contains("/uv/tools/") {
        "uv tool upgrade code-review-graph"
    } else if name.contains("/pipx/") {
        "pipx upgrade code-review-graph"
    } else {
        UPGRADE
    }
}

fn output(exe: &Path, args: &[&str], cwd: &Path, root: &Path) -> Option<String> {
    let outside = |p: &Path| {
        p.is_absolute()
            && !p.starts_with(root)
            && p.canonicalize().is_ok_and(|v| !v.starts_with(root))
    };
    let path = env::join_paths(
        env::split_paths(&env::var_os("PATH").unwrap_or_default()).filter(|p| outside(p)),
    )
    .ok()?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .env("PATH", path)
        .env("UV_OFFLINE", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    // A descendant may keep a pipe open after the probed program exits.
    // Bound both output and the wait for its readers.
    let (send, recv) = mpsc::channel();
    fn reader(
        stream: impl Read + Send + 'static,
        index: usize,
        send: mpsc::Sender<(usize, Vec<u8>)>,
    ) {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if stream.take((1 << 20) + 1).read_to_end(&mut bytes).is_ok() && bytes.len() <= 1 << 20
            {
                let _ = send.send((index, bytes));
            }
        });
    }
    reader(child.stdout.take()?, 0, send.clone());
    reader(child.stderr.take()?, 1, send);
    let limit = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().ok()? {
            if !status.success() {
                return None;
            }
            let mut out = [Vec::new(), Vec::new()];
            for _ in 0..2 {
                let (index, bytes) = recv
                    .recv_timeout(limit.checked_duration_since(Instant::now())?)
                    .ok()?;
                out[index] = bytes;
            }
            return Some(String::from_utf8_lossy(&out.concat()).into_owned());
        }
        if Instant::now() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub(super) struct Probes<'a> {
    root: &'a Path,
    own: &'a Path,
    seen: HashMap<(PathBuf, bool), Option<String>>,
    uv: Option<Option<String>>,
}
impl<'a> Probes<'a> {
    pub(super) fn new(root: &'a Path, own: &'a Path) -> Self {
        Self {
            root,
            own,
            seen: HashMap::new(),
            uv: None,
        }
    }
    pub(super) fn on_path(&self, name: &str) -> Option<PathBuf> {
        candidates(name).into_iter().next()
    }
    pub(super) fn allowed(&self, name: &str) -> Option<PathBuf> {
        let p = Path::new(name);
        let path = if p.is_absolute() {
            p.to_owned()
        } else if name.contains(['/', '\\']) {
            return None;
        } else {
            self.on_path(name)?
        };
        let real = path.canonicalize().ok()?;
        let root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.to_owned());
        // A repository-local venv can symlink its interpreter to a system binary.
        // Launching through that path still loads the repository's site-packages.
        let located_in_repo =
            path.parent()?.canonicalize().ok()?.starts_with(&root) || real.starts_with(&root);
        if !real.is_file()
            || (located_in_repo && self.own.canonicalize().ok().as_ref() != Some(&real))
        {
            None
        } else {
            Some(path)
        }
    }
    fn neutral_dir(&self) -> Option<PathBuf> {
        [env::temp_dir(), self.own.parent()?.to_owned()]
            .into_iter()
            .filter_map(|p| p.canonicalize().ok())
            .find(|p| !p.starts_with(self.root))
    }
    pub(super) fn version(&mut self, path: &Path, is_python: bool) -> Option<String> {
        let allowed = self.allowed(path.to_str()?)?;
        let neutral = self.neutral_dir()?;
        let value = self
            .seen
            .entry((allowed.clone(), is_python))
            .or_insert_with(|| {
                let args = if is_python {
                    ["-I", "-m", "code_review_graph", "--version"].as_slice()
                } else {
                    ["--version"].as_slice()
                };
                let raw = output(&allowed, args, &neutral, self.root)?;
                [
                    r"code-review-graph\s+v?(\d+\.\d+\.\d+[0-9A-Za-z.+-]*)",
                    r"(\d+\.\d+\.\d+[0-9A-Za-z.+-]*)",
                ]
                .iter()
                .find_map(|p| {
                    Regex::new(p)
                        .unwrap()
                        .captures(&raw)
                        .map(|c| c[1].to_owned())
                })
            });
        value.clone()
    }
    pub(super) fn git(&self, args: &[&str]) -> Option<String> {
        let git = self.allowed("git")?;
        let mut flags = vec!["-c", "core.fsmonitor=false"];
        flags.extend_from_slice(args);
        output(&git, &flags, self.root, self.root)
    }
    pub(super) fn uv_tool(&mut self) -> Option<String> {
        if let Some(version) = &self.uv {
            return version.clone();
        }
        let version = self
            .allowed("uv")
            .and_then(|p| output(&p, &["tool", "list"], &self.neutral_dir()?, self.root))
            .and_then(|s| {
                Regex::new(r"(?m)^code-review-graph v(\S+)")
                    .unwrap()
                    .captures(&s)
                    .map(|c| c[1].to_owned())
            });
        self.uv = Some(version.clone());
        version
    }
    pub(super) fn copies(&mut self, checks: &mut Vec<Finding>) -> Vec<Value> {
        let mut copies = Vec::new();
        for (i, path) in candidates("code-review-graph").iter().enumerate() {
            let version = self.version(path, false);
            let role = if i == 0 {
                "Your shell runs this one, and so does a hook that names no copy"
            } else {
                "Hidden behind the one above"
            };
            let message = format!(
                "On PATH: {} {}. {role}.",
                path.display(),
                version.as_deref().unwrap_or("no answer to --version")
            );
            let check = match (i, version.as_deref()) {
                (_, Some(VERSION)) => Finding::new("ok", message),
                (0, Some(v)) => Finding::new("fail", message).fix(mismatch(
                    v,
                    &format!("the copy at {}", path.display()),
                    upgrade(path),
                )),
                (0, None) => Finding::new("fail", message)
                    .fix(format!("Reinstall the copy at {}.", path.display())),
                _ => Finding::new("warn", message)
                    .fix(format!("Remove it, or upgrade it too: {}.", upgrade(path))),
            };
            checks.push(check);
            copies.push(json!({"path":path,"version":version}));
        }
        if copies.is_empty() {
            checks.push(Finding::new("info","On PATH: no code-review-graph. Your shell cannot find it, and nor can a hook that names no copy."));
        }
        copies
    }
}
