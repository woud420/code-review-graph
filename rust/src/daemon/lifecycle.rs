macro_rules! daemon_log { ($($arg:tt)*) => { super::log(format_args!($($arg)*)) }; }
use super::config::{Config, Repo, atomic_write, load, lock, log_file};
use crate::canonical::{self, postprocess::Level};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub(super) fn health_file(home: &Path, repo: &Path) -> PathBuf {
    let hash = hex::encode(Sha256::digest(repo.to_string_lossy().as_bytes()));
    home.join("watch-health")
        .join(format!("{}.json", &hash[..16]))
}

fn read_pid(home: &Path) -> Option<u32> {
    fs::read_to_string(home.join("daemon.pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(super) fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let result = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "stat="])
            .output();
        result.is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().starts_with(b"Z"))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

pub(super) fn running(home: &Path) -> Result<Option<u32>> {
    let Some(pid) = read_pid(home) else {
        return Ok(None);
    };
    let lock_file = lock(home, "daemon.lock")?;
    match lock_file.try_lock() {
        Err(TryLockError::WouldBlock) => Ok(read_pid(home)),
        Err(TryLockError::Error(e)) => Err(e.into()),
        Ok(()) => {
            if alive(pid) {
                daemon_log!(
                    "INFO: PID file names live process {pid}, which is not this daemon (it holds no daemon lock); treating the file as stale"
                );
            }
            let _ = fs::remove_file(home.join("daemon.pid"));
            Ok(None)
        }
    }
}

fn self_command() -> Result<Command> {
    let exe = std::env::current_exe()?;
    let nested = exe.file_stem().is_some_and(|s| s == "code-review-graph");
    let mut cmd = Command::new(exe);
    if nested {
        cmd.arg("daemon");
    }
    Ok(cmd)
}

pub(super) fn start(home: &Path, foreground: bool) -> Result<()> {
    ensure!(running(home)?.is_none(), "Daemon is already running.");
    let config = load(home)?;
    if foreground {
        return serve(home, config);
    }
    fs::create_dir_all(&config.logs)?;
    let log = File::options()
        .create(true)
        .append(true)
        .open(log_file(&config, None)?)?;
    let mut cmd = self_command()?;
    cmd.args(["start", "--foreground"])
        .env("CRG_HOME", std::path::absolute(home)?)
        .stdin(Stdio::null())
        .stderr(log.try_clone()?)
        .stdout(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if read_pid(home) == Some(child.id()) {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            if running(home)?.is_some() {
                bail!("Daemon is already running.");
            }
            bail!("Daemon startup failed ({status})");
        }
        thread::sleep(Duration::from_millis(30));
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!("Timed out waiting for daemon startup")
}

pub(super) fn stop(home: &Path) -> Result<bool> {
    let Some(pid) = running(home)? else {
        println!("Daemon is not running.");
        return Ok(false);
    };
    let token = fs::read_to_string(home.join("daemon.instance"))
        .context("daemon has no native control channel")?;
    ensure!(
        token
            .split_once('.')
            .is_some_and(|(p, _)| p.parse() == Ok(pid)),
        "daemon control channel does not match its PID"
    );
    println!("Stopping daemon (PID {pid})...");
    atomic_write(&home.join("daemon.stop"), token.as_bytes())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if read_pid(home) != Some(pid) || running(home)?.is_none() {
            println!("Daemon stopped.");
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("Daemon PID {pid} did not acknowledge shutdown")
}

struct Worker {
    path: PathBuf,
    process: Child,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.process.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.process.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(30));
        }
        // Child is our unreaped handle, never a PID imported from a state file.
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn spawn_watcher(home: &Path, c: &Config, repo: &Repo) -> Result<Worker> {
    crate::registry::register_repo(&repo.path, Some(repo.alias.clone()))?;
    fs::create_dir_all(&c.logs)?;
    let log = File::options()
        .append(true)
        .create(true)
        .open(log_file(c, Some(&repo.alias))?)?;
    let mut cmd = self_command()?;
    let process = cmd
        .arg("--crg-watch-worker")
        .arg(&repo.path)
        .arg(c.poll.max(1).to_string())
        .env("CRG_HOME", std::path::absolute(home)?)
        .current_dir(&repo.path)
        .stdin(Stdio::piped())
        .stderr(log.try_clone()?)
        .stdout(log)
        .spawn()?;
    Ok(Worker {
        path: repo.path.clone(),
        process,
    })
}

fn reconcile(home: &Path, c: &Config, children: &mut BTreeMap<String, Worker>) -> Result<()> {
    children.retain(|alias, worker| {
        let keep = c
            .repos
            .iter()
            .any(|r| &r.alias == alias && r.path == worker.path)
            && matches!(worker.process.try_wait(), Ok(None));
        if !keep {
            let _ = fs::remove_file(health_file(home, &worker.path));
        }
        keep
    });
    for repo in &c.repos {
        if !children.contains_key(&repo.alias) {
            match spawn_watcher(home, c, repo) {
                Ok(worker) => {
                    children.insert(repo.alias.clone(), worker);
                }
                Err(e) => {
                    daemon_log!("WARNING: Could not start watcher '{}': {e:#}", repo.alias);
                }
            }
        }
    }
    let state: BTreeMap<_, _> = children
        .iter()
        .map(|(alias, w)| (alias, json!({"pid":w.process.id(),"path":w.path})))
        .collect();
    atomic_write(&home.join("daemon-state.json"), serde_json::to_vec(&state)?)?;
    Ok(())
}

struct Session<'a> {
    home: &'a Path,
    _lock: File,
}
impl Drop for Session<'_> {
    fn drop(&mut self) {
        for name in [
            "daemon-state.json",
            "daemon.stop",
            "daemon.instance",
            "daemon.pid",
        ] {
            let _ = fs::remove_file(self.home.join(name));
        }
    }
}

fn serve(home: &Path, mut config: Config) -> Result<()> {
    let guard = lock(home, "daemon.lock")?;
    match guard.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => bail!("Daemon is already running."),
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    let _session = Session { home, _lock: guard };
    let pid = std::process::id();
    let token = format!(
        "{pid}.{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    atomic_write(&home.join("daemon.instance"), &token)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, Arc::clone(&shutdown))?;
    }
    atomic_write(&home.join("daemon.pid"), pid.to_string())?;
    daemon_log!("INFO: Starting daemon '{}'", config.name);
    let mut workers = BTreeMap::new();
    reconcile(home, &config, &mut workers)?;
    println!("Daemon started — watching {} repo(s)", config.repos.len());
    std::io::stdout().flush()?;
    let mut next = Instant::now() + Duration::from_secs(config.poll.max(1) as u64);
    let mut fingerprint = None;
    while !shutdown.load(Ordering::Relaxed) {
        if fs::read_to_string(home.join("daemon.stop")).ok().as_deref() == Some(token.as_str()) {
            break;
        }
        if Instant::now() >= next {
            let bytes = fs::read(home.join("watch.toml")).ok();
            if bytes != fingerprint {
                match load(home) {
                    Ok(c) => {
                        config = c;
                        fingerprint = bytes;
                    }
                    Err(e) => daemon_log!("WARNING: Could not reload config: {e:#}"),
                }
            }
            reconcile(home, &config, &mut workers)?;
            next = Instant::now() + Duration::from_secs(config.poll.max(1) as u64);
        }
        thread::sleep(Duration::from_millis(75));
    }
    let paths: Vec<_> = workers.values().map(|w| w.path.clone()).collect();
    drop(workers);
    for path in paths {
        let _ = fs::remove_file(health_file(home, &path));
    }
    daemon_log!("INFO: Daemon stopped");
    Ok(())
}

pub(super) fn worker(home: &Path, repo: &Path, seconds: u64) -> Result<()> {
    let (closed, stop) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0];
        let _ = std::io::stdin().read(&mut buf);
        let _ = closed.send(());
    });
    let health = health_file(home, repo);
    fs::create_dir_all(health.parent().context("missing health directory")?)?;
    let db = std::env::var_os("CRG_DATA_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join(".code-review-graph"))
        .join("graph.db");
    let mut last_event = Value::Null;
    loop {
        let result = canonical::index(repo, &db, false, Level::Full, false);
        let ok = match result {
            Ok(report) => {
                if report.files_parsed + report.files_deleted > 0 {
                    last_event = json!(now());
                    println!(
                        "Indexed {} changed and {} deleted files",
                        report.files_parsed, report.files_deleted
                    );
                }
                true
            }
            Err(e) => {
                daemon_log!("WARNING: Could not index {}: {e:#}", repo.display());
                false
            }
        };
        atomic_write(
            &health,
            serde_json::to_vec(&json!({"pid":std::process::id(),"repo_root":repo,
            "updated_at":now(),"last_event_at":last_event,"observer_alive":true,"degraded":!ok,
            "phase":"watching","failed_paths":[]}))?,
        )?;
        if !matches!(
            stop.recv_timeout(Duration::from_secs(seconds.max(1))),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            break;
        }
    }
    let _ = fs::remove_file(health);
    Ok(())
}
