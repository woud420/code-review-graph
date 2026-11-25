use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
    time::Instant,
};

pub(super) enum Event {
    Client(Value),
    ClientError(&'static str),
    End,
    Child(u64, Value),
    Stopped(u64, String),
}
async fn frame(
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    cap: usize,
) -> Option<Result<Value, &'static str>> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let buf = reader.fill_buf().await.ok()?;
        if buf.is_empty() {
            if bytes.is_empty() && !oversized {
                return None;
            }
            break;
        }
        let newline = buf.iter().position(|b| *b == b'\n');
        let count = newline.map_or(buf.len(), |i| i + 1);
        if bytes.len() + count > cap {
            oversized = true;
        }
        if !oversized {
            bytes.extend_from_slice(&buf[..count]);
        }
        reader.consume(count);
        if newline.is_some() {
            break;
        }
    }
    Some(if oversized {
        Err("JSON-RPC frame is too large")
    } else {
        serde_json::from_slice(&bytes).map_err(|_| "invalid JSON-RPC message")
    })
}

pub(super) fn read_client(tx: mpsc::Sender<Event>) {
    tokio::spawn(async move {
        let mut input = BufReader::new(tokio::io::stdin());
        while let Some(value) = frame(&mut input, 4 << 20).await {
            let event = match value {
                Ok(v) => Event::Client(v),
                Err(e) => Event::ClientError(e),
            };
            if tx.send(event).await.is_err() {
                return;
            }
        }
        let _ = tx.send(Event::End).await;
    });
}

pub(super) async fn write(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

pub(super) struct Peer {
    process: Child,
    input: mpsc::Sender<Value>,
    pub(super) id: u64,
    root: PathBuf,
    pub(super) db: PathBuf,
    initializing: Option<Instant>,
    waiting: Vec<Value>,
}
impl Peer {
    pub(super) fn start(
        root: &Path,
        db: PathBuf,
        id: u64,
        tools: Option<&str>,
        watch: bool,
        tx: mpsc::Sender<Event>,
    ) -> Result<Self> {
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("serve")
            .arg("--repo")
            .arg(root)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(tools) = tools {
            cmd.arg("--tools").arg(tools);
        }
        if watch {
            cmd.arg("--auto-watch");
        }
        for (key, _) in std::env::vars_os() {
            if key == "CRG_DATA_DIR"
                || key == "CRG_REPO_ROOT"
                || key.to_string_lossy().starts_with("GIT_")
            {
                cmd.env_remove(key);
            }
        }
        let mut process = cmd.spawn()?;
        let mut stdin = process.stdin.take().expect("piped stdin");
        let stdout = process.stdout.take().expect("piped stdout");
        let (input, mut receiver) = mpsc::channel::<Value>(super::CAP + 2);
        let errors = tx.clone();
        tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                if write(&mut stdin, &message).await.is_err() {
                    let _ = errors
                        .send(Event::Stopped(id, "MCP child stopped".into()))
                        .await;
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut output = BufReader::new(stdout);
            while let Some(v) = frame(&mut output, 64 << 20).await {
                match v {
                    Ok(value) if value.is_object() => {
                        if tx.send(Event::Child(id, value)).await.is_err() {
                            return;
                        }
                    }
                    failed => {
                        let reason = match failed {
                            Ok(_) => "MCP child emitted a non-object message",
                            Err("JSON-RPC frame is too large") => {
                                "MCP child emitted an oversized JSON-RPC frame"
                            }
                            _ => "MCP child emitted invalid JSON",
                        };
                        let _ = tx.send(Event::Stopped(id, reason.into())).await;
                        return;
                    }
                }
            }
            let _ = tx.send(Event::Stopped(id, "MCP child exited".into())).await;
        });
        Ok(Self {
            process,
            input,
            id,
            root: root.to_owned(),
            db,
            initializing: None,
            waiting: Vec::new(),
        })
    }
    pub(super) fn send(&self, msg: Value) -> Result<()> {
        self.input
            .try_send(msg)
            .map_err(|_| anyhow!("MCP child stopped for {}", self.root.display()))
    }
    pub(super) fn initialize(&mut self, params: &Value) -> Result<()> {
        self.send(
            json!({"jsonrpc":"2.0","id":super::HELLO,"method":"initialize","params":params}),
        )?;
        self.initializing = Some(Instant::now());
        Ok(())
    }
    pub(super) fn finish_initialization(&mut self, reply: &Value) -> Result<bool> {
        if reply["id"] != super::HELLO
            || reply.get("method").is_some()
            || self.initializing.take().is_none()
        {
            return Ok(false);
        }
        if reply.get("error").is_some() {
            bail!("MCP child initialization failed");
        }
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}))?;
        for message in std::mem::take(&mut self.waiting) {
            self.send(message)?;
        }
        Ok(true)
    }
    pub(super) fn startup_timed_out(&self) -> bool {
        self.initializing
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(60))
    }
    pub(super) fn cancel(&mut self, key: &str, mut message: Value) -> Result<()> {
        if let Some(index) = self
            .waiting
            .iter()
            .position(|v| v["id"].as_str() == Some(key))
        {
            self.waiting.remove(index);
            return Ok(());
        }
        message["params"]["requestId"] = json!(key);
        self.send(message)
    }
    pub(super) fn forward(&mut self, msg: Value) -> Result<()> {
        if self.initializing.is_some() {
            if self.waiting.len() >= super::CAP {
                bail!("too many in-flight requests");
            }
            self.waiting.push(msg);
            Ok(())
        } else {
            self.send(msg)
        }
    }
    pub(super) async fn stop(mut self) {
        drop(self.input);
        if tokio::time::timeout(Duration::from_secs(10), self.process.wait())
            .await
            .is_err()
        {
            let _ = self.process.kill().await;
        }
    }
}
