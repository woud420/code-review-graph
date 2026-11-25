mod roots;
mod transport;
use crate::cli::store;
use anyhow::{Result, bail, ensure};
use roots::Worktrees;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
use transport::{Event, Peer};

const CAP: usize = 256;
const HELLO: &str = "multi-worktree-init";
fn error(id: &Value, code: i32, message: impl ToString) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.to_string()}})
}

struct Route {
    peer: u64,
    caller: Value,
}
struct Router {
    roots: Worktrees,
    tools: Option<String>,
    watch: bool,
    init: Option<Value>,
    peers: HashMap<PathBuf, Peer>,
    requests: HashMap<String, Route>,
    replies: HashMap<String, Route>,
    next: u64,
    tx: mpsc::Sender<Event>,
}
impl Router {
    fn serial(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    async fn peer(&mut self, root: &Path, handshake: bool) -> Result<&mut Peer> {
        let db = roots::resolve(&store::database(root, None));
        if let Some(peer) = self.peers.get(root) {
            ensure!(
                peer.db == db,
                "the graph database for {} changed to {} while its server is running; restart this MCP session",
                root.display(),
                db.display()
            );
        } else {
            if let Some((other, _)) = self.peers.iter().find(|(_, p)| p.db == db) {
                bail!(
                    "worktrees must not share a graph database: {} and {} resolve to {}",
                    root.display(),
                    other.display(),
                    db.display()
                );
            }
            ensure!(
                self.peers.len() < 32,
                "too many active worktree MCP children"
            );
            let checked = self
                .roots
                .authorize(root.to_str().unwrap_or_default())
                .await?;
            ensure!(
                checked == root,
                "worktree root changed during child startup"
            );
            let id = self.serial();
            let mut peer = Peer::start(
                root,
                db,
                id,
                self.tools.as_deref(),
                self.watch,
                self.tx.clone(),
            )?;
            if handshake && let Some(params) = &self.init {
                peer.initialize(params)?;
            }
            self.peers.insert(root.into(), peer);
        }
        Ok(self.peers.get_mut(root).expect("initialized peer"))
    }
    async fn forward(&mut self, root: &Path, mut msg: Value, handshake: bool) -> Result<()> {
        let id = self.peer(root, handshake).await?.id;
        let mut internal = None;
        if let Some(original) = msg.get("id").cloned() {
            ensure!(
                !self.requests.values().any(|r| r.caller == original),
                "duplicate in-flight JSON-RPC request id"
            );
            ensure!(
                self.requests.len() < CAP,
                "too many in-flight JSON-RPC requests"
            );
            let key = format!("multi-worktree-client-{}", self.serial());
            self.requests.insert(
                key.clone(),
                Route {
                    peer: id,
                    caller: original,
                },
            );
            msg["id"] = json!(key);
            internal = Some(key);
        }
        let result = self
            .peers
            .get_mut(root)
            .expect("initialized peer")
            .forward(msg);
        if result.is_err()
            && let Some(key) = internal
        {
            self.requests.remove(&key);
        }
        result
    }
    fn by_id(&mut self, id: u64) -> Option<&mut Peer> {
        self.peers.values_mut().find(|p| p.id == id)
    }
    async fn notify_peer(&mut self, id: u64, msg: Value) -> Result<()> {
        if let Some(peer) = self.by_id(id)
            && let Err(e) = peer.send(msg)
        {
            self.stopped(id, &e.to_string()).await?;
        }
        Ok(())
    }
    async fn client(&mut self, mut msg: Value) -> Result<bool> {
        let id = msg["id"].clone();
        let mut root = self.roots.primary.clone();
        let method = msg["method"].as_str().unwrap_or("").to_owned();
        if !msg.is_object() {
            self.emit(error(
                &Value::Null,
                -32600,
                "JSON-RPC message must be an object",
            ))
            .await?;
            return Ok(true);
        }
        match method.as_str() {
            "exit" => return Ok(false),
            "initialize" => {
                let v = msg
                    .get("params")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if !v.is_object() {
                    self.emit(error(&id, -32602, "initialize params must be an object"))
                        .await?;
                    return Ok(true);
                }
                self.init = Some(v);
            }
            "notifications/initialized" => {
                if let Some(id) = self.peers.get(&root).map(|p| p.id) {
                    self.notify_peer(id, msg).await?;
                }
                return Ok(true);
            }
            "notifications/cancelled" => {
                let original = &msg["params"]["requestId"];
                if let Some(key) = self
                    .requests
                    .iter()
                    .find(|(_, r)| r.caller == *original)
                    .map(|(k, _)| k.clone())
                    && let Some(route) = self.requests.remove(&key)
                    && let Some(p) = self.by_id(route.peer)
                    && let Err(e) = p.cancel(&key, msg)
                {
                    self.stopped(route.peer, &e.to_string()).await?;
                }
                return Ok(true);
            }
            "" if msg.get("id").is_some() => {
                if let Some(key) = id.as_str()
                    && let Some(route) = self.replies.remove(key)
                {
                    msg["id"] = route.caller;
                    self.notify_peer(route.peer, msg).await?;
                }
                return Ok(true);
            }
            "tools/call" => {
                if !msg["params"].is_null() && !msg["params"].is_object() {
                    self.emit(error(&id, -32602, "tools/call params must be an object"))
                        .await?;
                    return Ok(true);
                }
                let args = &msg["params"]["arguments"];
                if !args.is_null() && !args.is_object() {
                    self.emit(error(&id, -32602, "tools/call arguments must be an object"))
                        .await?;
                    return Ok(true);
                }
                if let Some(raw) = args.get("repo_root").filter(|v| !v.is_null()) {
                    let authorized = if let Some(p) = raw.as_str() {
                        self.roots.authorize(p).await
                    } else {
                        Err(anyhow::anyhow!(
                            "repo_root must be a non-empty absolute path"
                        ))
                    };
                    match authorized {
                        Ok(verified) => {
                            root = verified;
                            msg["params"]["arguments"]["repo_root"] = json!(root);
                        }
                        Err(e) => {
                            if msg.get("id").is_some() {
                                self.emit(error(&id, -32602, e)).await?;
                            }
                            return Ok(true);
                        }
                    }
                }
            }
            _ => {}
        }
        let needs_reply = msg.get("id").is_some();
        if let Err(e) = self.forward(&root, msg, method != "initialize").await
            && needs_reply
        {
            self.emit(error(&id, -32001, e)).await?;
        }
        Ok(true)
    }
    async fn emit(&self, msg: Value) -> Result<()> {
        transport::write(&mut tokio::io::stdout(), &msg).await?;
        Ok(())
    }
    async fn child(&mut self, id: u64, mut msg: Value) -> Result<()> {
        let Some(peer) = self.by_id(id) else {
            return Ok(());
        };
        match peer.finish_initialization(&msg) {
            Ok(true) => return Ok(()),
            Err(e) => return self.stopped(id, &e.to_string()).await,
            Ok(false) => {}
        }
        if let Some(original) = msg.get("id").cloned() {
            if msg.get("method").is_none() {
                let Some(key) = original.as_str() else {
                    return Ok(());
                };
                if self.requests.get(key).is_none_or(|r| r.peer != id) {
                    return Ok(());
                }
                let route = self.requests.remove(key).expect("validated response");
                msg["id"] = route.caller;
            } else {
                if self.replies.len() >= CAP {
                    self.by_id(id).expect("known peer").send(error(
                        &original,
                        -32002,
                        "too many in-flight requests",
                    ))?;
                    return Ok(());
                }
                let key = format!("multi-worktree-server-{}", self.serial());
                self.replies.insert(
                    key.clone(),
                    Route {
                        peer: id,
                        caller: original,
                    },
                );
                msg["id"] = json!(key);
            }
        }
        self.emit(msg).await
    }
    async fn stopped(&mut self, id: u64, reason: &str) -> Result<()> {
        let Some(path) = self
            .peers
            .iter()
            .find_map(|(path, p)| (p.id == id).then(|| path.clone()))
        else {
            return Ok(());
        };
        let peer = self.peers.remove(&path).expect("known peer");
        tokio::spawn(peer.stop());
        self.replies.retain(|_, r| r.peer != id);
        let failed: Vec<_> = self
            .requests
            .extract_if(|_, r| r.peer == id)
            .map(|(_, r)| r.caller)
            .collect();
        for caller in failed {
            self.emit(error(
                &caller,
                -32001,
                format!("{reason}: {}", path.display()),
            ))
            .await?;
        }
        Ok(())
    }
    async fn run(
        mut self,
        mut read: mpsc::Receiver<Event>,
        signal: Arc<AtomicUsize>,
    ) -> Result<usize> {
        let mut heartbeat = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                _=heartbeat.tick()=>{
                    if signal.load(Ordering::Relaxed)!=0{break;}
                    let expired:Vec<_>=self.peers.values().filter(|p|p.startup_timed_out()).map(|p|p.id).collect();
                    for id in expired{self.stopped(id,"MCP child did not initialize").await?;}
                }
                event=read.recv()=>match event{
                    Some(Event::Client(m))=>if !self.client(m).await?{break;},
                    Some(Event::ClientError(s))=>self.emit(error(&Value::Null,-32700,s)).await?,
                    Some(Event::Child(id,m))=>self.child(id,m).await?,
                    Some(Event::Stopped(id,s))=>self.stopped(id,&s).await?,
                    Some(Event::End)|None=>break,
                }
            }
        }
        let mut clean = tokio::task::JoinSet::new();
        for (_, peer) in self.peers {
            clean.spawn(peer.stop());
        }
        while clean.join_next().await.is_some() {}
        Ok(signal.load(Ordering::Relaxed))
    }
}

fn invalid(cmd: &str, message: impl std::fmt::Display) -> ! {
    if cmd == "mcp" {
        // The v2.4.0 alias exits 1 here. Keep the actual configuration error visible.
        eprintln!("code-review-graph mcp: error: {message}");
        std::process::exit(1);
    }
    let usage = "usage: code-review-graph serve [-h] [--repo REPO] [--auto-watch]\n                               [--multi-worktree] [--tools TOOLS] [--http]\n                               [--host ADDR] [--port PORT] [--multi-repo]";
    eprintln!("{usage}\ncode-review-graph {cmd}: error: {message}");
    std::process::exit(2);
}

pub(in crate::cli) fn serve(
    cmd: &str,
    repo: Option<&Path>,
    tools: Option<String>,
    watch: bool,
) -> Result<()> {
    if std::env::var("CRG_DATA_DIR").is_ok_and(|s| !s.trim().is_empty()) {
        invalid(
            cmd,
            "--multi-worktree cannot be combined with CRG_DATA_DIR; use per-repository Registry data_dir entries instead",
        );
    }
    let root = crate::cli::root(repo, false)?;
    let signal = Arc::new(AtomicUsize::new(0));
    #[cfg(unix)]
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register_usize(sig, signal.clone(), sig as usize)?;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let code = rt.block_on(async {
        let roots = Worktrees::new(&root)
            .await
            .unwrap_or_else(|e| invalid(cmd, e));
        let (tx, rx) = mpsc::channel(16);
        transport::read_client(tx.clone());
        Router {
            roots,
            tools,
            watch,
            init: None,
            peers: HashMap::new(),
            requests: HashMap::new(),
            replies: HashMap::new(),
            next: 0,
            tx,
        }
        .run(rx, signal)
        .await
    })?;
    // Tokio's blocking stdin reader can still be waiting on the parent after a signal.
    rt.shutdown_timeout(Duration::from_millis(100));
    if code != 0 {
        std::process::exit(128 + code as i32);
    }
    Ok(())
}
