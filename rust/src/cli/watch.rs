use super::{
    index::location::{FollowSymlinks, Location},
    store,
};
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use code_review_graph_rs::{
    canonical::{self, embeddings::Refresh, postprocess::Level},
    daemon,
    graph::discovery::{self, Inventory},
};
use notify::{EventKind, RecursiveMode, Watcher};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Args)]
pub(super) struct Options {
    #[command(flatten)]
    location: Location,
    #[arg(long, value_parser=["local", "openai", "google", "minimax", "voyage"])]
    embedding_provider: Option<String>,
    #[arg(long)]
    embedding_model: Option<String>,
    #[command(flatten)]
    links: FollowSymlinks,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

struct Health {
    path: Option<PathBuf>,
    started: f64,
    events_seen: usize,
    last_event: Option<f64>,
}

impl Health {
    fn new(repo: &Path) -> Self {
        Self {
            path: daemon::watch_health_path(repo).ok(),
            started: now(),
            events_seen: 0,
            last_event: None,
        }
    }
    fn report(&self, repo: &Path, phase: &str, watched: usize) {
        let _ = (|| -> Result<()> {
            let path = self.path.as_ref().context("no health location")?;
            let parent = path.parent().context("missing health directory")?;
            fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(
                &mut file,
                &json!({
                    "repo":repo,"pid":std::process::id(),"started_at":self.started,"updated_at":now(),
                    "observer_alive":true,"last_event_at":self.last_event,"events_seen":self.events_seen,
                    "watched_paths":watched,"dead_threads":[],"degraded":false,"failed_paths":[],"phase":phase
                }),
            )?;
            file.flush()?;
            file.persist(path)?;
            Ok(())
        })();
    }
    fn record_event(&mut self) {
        self.events_seen += 1;
        self.last_event = Some(now());
    }
    fn clear(&self) {
        if let Some(path) = &self.path
            && fs::read(path)
                .ok()
                .and_then(|s| serde_json::from_slice::<serde_json::Value>(&s).ok())
                .is_some_and(|v| v["pid"].as_u64() == Some(std::process::id().into()))
        {
            let _ = fs::remove_file(path);
        }
    }
}

struct Observations {
    native: notify::RecommendedWatcher,
    directories: BTreeSet<PathBuf>,
    messages: mpsc::Receiver<notify::Result<notify::Event>>,
}
impl Observations {
    fn start() -> Result<Self> {
        let (send, messages) = mpsc::channel();
        let native = notify::recommended_watcher(move |event| {
            let _ = send.send(event);
        })?;
        Ok(Self {
            native,
            directories: BTreeSet::new(),
            messages,
        })
    }
    fn sync(&mut self, desired: &BTreeSet<PathBuf>) -> Result<()> {
        if desired == &self.directories {
            return Ok(());
        }
        let mut paths = self.native.paths_mut();
        for added in desired.difference(&self.directories) {
            paths
                .add(added, RecursiveMode::NonRecursive)
                .with_context(|| format!("watching {}", added.display()))?;
        }
        for dropped in self.directories.difference(desired) {
            let _ = paths.remove(dropped);
        }
        paths.commit()?;
        self.directories.clone_from(desired);
        Ok(())
    }
    fn watched(&self) -> usize {
        self.directories.len()
    }
    fn changed(&self) -> Result<bool> {
        match self.messages.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => Ok(!matches!(event.kind, EventKind::Access(_))),
            Ok(Err(error)) => Err(error.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("filesystem observer stopped"),
        }
    }
}

struct Session {
    root: PathBuf,
    database: PathBuf,
    follow: bool,
    embedding: Option<(String, String)>,
}

impl Session {
    fn previous_files(&self) -> Result<BTreeSet<String>> {
        let old = self.root.join(".code-review-graph.db");
        let db = if !self.database.exists()
            && self.database == self.root.join(".code-review-graph/graph.db")
            && old.exists()
        {
            &old
        } else {
            &self.database
        };
        if !db.is_file() || db.metadata()?.len() == 0 {
            return Ok(BTreeSet::new());
        }
        let conn = store::open(db)?;
        let mut query = conn.prepare("SELECT file_path FROM nodes WHERE kind='File'")?;
        let mut files = BTreeSet::new();
        for row in query.query_map([], |r| r.get::<_, String>(0))? {
            let path = PathBuf::from(row?);
            let local = path
                .strip_prefix(&self.root)
                .with_context(|| format!("graph contains files outside {}", self.root.display()))?;
            files.insert(
                local
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
        Ok(files)
    }

    fn reconcile(
        &self,
        inventory: &Inventory,
        deferred: &BTreeSet<String>,
        stop: &AtomicBool,
    ) -> Result<()> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let request = self
                .embedding
                .as_ref()
                .map(|(p, m)| (p.as_str(), m.as_str()));
            match canonical::index_sources(
                &self.root,
                &self.database,
                false,
                Level::Full,
                false,
                request,
                canonical::Sources::Watching(inventory, deferred),
            ) {
                Ok(report) => {
                    if let Refresh::Failed { class, reason } = report.embeddings {
                        bail!(
                            "watch post-processing reported warnings: Embedding refresh failed: {class}: {reason}"
                        );
                    }
                    return Ok(());
                }
                Err(e) if e.to_string() == "another writer is using this graph" => {
                    thread::sleep(Duration::from_millis(100))
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn run(&self, stop: Arc<AtomicBool>) -> Result<()> {
        let indexed = self.previous_files()?;
        let mut health = Health::new(&self.root);
        let mut observer = Observations::start()?;
        health.report(&self.root, "initial-build", 0);
        let run = (|| -> Result<()> {
            let first = Inventory::discover(&self.root, self.follow)?;
            observer.sync(&first.directories)?;
            // Install the watches before choosing the snapshot that enters the database.
            let mut inventory = Inventory::discover(&self.root, self.follow)?;
            // Reconcile previously indexed files. A source first seen on disk before the
            // observer started enters the graph only when the user edits it.
            let mut deferred: BTreeSet<_> = first
                .files
                .iter()
                .filter(|p| !indexed.contains(*p) && !first.followed_files.contains(*p))
                .cloned()
                .collect();
            deferred
                .retain(|p| inventory.stamps.get(Path::new(p)) == first.stamps.get(Path::new(p)));
            self.reconcile(&inventory, &deferred, &stop)?;
            observer.sync(&inventory.directories)?;
            health.report(&self.root, "watching", observer.watched());
            eprintln!(
                "INFO: Watching {} for changes... (Ctrl+C to stop)",
                self.root.display()
            );
            let mut heartbeat = Instant::now();
            let mut last_scan = Instant::now();
            let mut first_event = None;
            while !stop.load(Ordering::Relaxed) {
                if observer.changed()? && first_event.is_none() {
                    first_event = Some(Instant::now());
                }
                if first_event.is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
                    || last_scan.elapsed() >= Duration::from_secs(5)
                {
                    ensure!(
                        self.root.is_dir(),
                        "watch root disappeared: {}",
                        self.root.display()
                    );
                    let next = Inventory::discover(&self.root, self.follow)?;
                    observer.sync(&next.directories)?;
                    if next.stamps != inventory.stamps {
                        deferred.retain(|p| {
                            next.stamps.get(Path::new(p)).is_some_and(|stamp| {
                                inventory.stamps.get(Path::new(p)) == Some(stamp)
                            })
                        });
                        self.reconcile(&next, &deferred, &stop)?;
                        health.record_event();
                        health.report(&self.root, "watching", observer.watched());
                    }
                    inventory = next;
                    last_scan = Instant::now();
                    first_event = None;
                }
                if heartbeat.elapsed() >= Duration::from_secs(10) {
                    health.report(&self.root, "watching", observer.watched());
                    heartbeat = Instant::now();
                }
            }
            Ok(())
        })();
        health.clear();
        if run.is_ok() {
            eprintln!("INFO: Watch stopped.");
        }
        run
    }
}

pub(super) fn foreground(options: Options) -> Result<()> {
    if options.embedding_provider.is_some() != options.embedding_model.is_some() {
        eprintln!(
            "usage: code-review-graph [-h] [-v] <command> ...\ncode-review-graph: error: --embedding-provider and --embedding-model must be supplied together"
        );
        std::process::exit(2);
    }
    let root = super::root(options.location.repo.as_deref(), false)?;
    let db = store::database(&root, options.location.data_dir.as_deref());
    let legacy = options.location.data_dir.is_none()
        && db == root.join(".code-review-graph/graph.db")
        && root.join(".code-review-graph.db").is_file();
    ensure!(
        db.is_file() || legacy,
        "No graph found at {}. Run `code-review-graph build` first.",
        db.display()
    );
    if db.is_file() && db.metadata()?.len() > 0 {
        store::check_root(&store::open(&db)?, &db, &root)?;
    }
    let session = Session {
        root,
        database: db,
        follow: discovery::follow_links(options.links.explicit()),
        embedding: options.embedding_provider.zip(options.embedding_model),
    };
    let stop = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    session.run(stop)
}

pub(super) fn with_auto(root: &Path, auto: bool, serve: impl FnOnce() -> Result<()>) -> Result<()> {
    if !auto {
        return serve();
    }
    let session = Session {
        root: root.to_owned(),
        database: store::database(root, None),
        follow: discovery::follow_links(None),
        embedding: None,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let handle = thread::Builder::new()
        .name("crg-watch".into())
        .spawn(move || {
            if let Err(error) = session.run(worker_stop) {
                eprintln!(
                    "ERROR: Auto-watch for {} stopped: {error:#}",
                    session.root.display()
                );
            }
        })?;
    eprintln!("INFO: Auto-watch started for {}", root.display());
    let served = serve();
    stop.store(true, Ordering::Relaxed);
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("auto-watch thread panicked"))?;
    served
}
