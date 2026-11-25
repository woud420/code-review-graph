use super::super::registry;
use anyhow::{Result, bail};
use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) const INSTRUCTIONS: &str = "\nThis server serves several repositories: pass repo_root, the absolute path of a registered repository, on every call. list_repos_tool lists them.";

fn printable(path: &str) -> String {
    path.chars()
        .filter(|&c| c == '\t' || c == '\n' || c >= ' ')
        .take(256)
        .collect()
}

pub(super) fn registered(requested: Option<&str>) -> Result<PathBuf> {
    let Some(raw) = requested.filter(|p| !p.is_empty()) else {
        bail!(
            "repo_root is required. This server serves several repositories (serve --multi-repo), so every call must name one: pass the absolute path of a registered repository. list_repos_tool lists them."
        );
    };
    let path = Path::new(raw);
    if !path.is_absolute() {
        bail!(
            "repo_root must be an absolute path on this server, got: {}",
            printable(raw)
        );
    }
    if raw.contains('\0') {
        bail!("repo_root cannot be checked: embedded null byte");
    }
    let wanted = match path.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_owned(),
        Err(e) => bail!("repo_root cannot be checked: {e}"),
    };
    // Reload on every request so unregister takes effect in existing sessions.
    for entry in registry::list()? {
        if let Some(known) = entry["path"].as_str()
            && Path::new(known).canonicalize().is_ok_and(|p| p == wanted)
        {
            return Ok(wanted);
        }
    }
    bail!(
        "repo_root is not a registered repository: {}. This server only serves registered repositories: run `code-review-graph register <path>` to add one, or call list_repos_tool to see them.",
        printable(&wanted.to_string_lossy())
    )
}

fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn allowed(authority: &str, implicit: Option<u16>, port: u16) -> bool {
    let raw = authority.trim();
    if raw.is_empty()
        || raw.contains(['/', '\\', '?', '#', '@'])
        || raw.chars().any(char::is_whitespace)
    {
        return false;
    }
    let (host, rest) = if let Some(v6) = raw.strip_prefix('[') {
        let Some((host, tail)) = v6.split_once(']') else {
            return false;
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        (host, tail)
    } else {
        let end = raw.find(':').unwrap_or(raw.len());
        if raw.contains(['[', ']']) {
            return false;
        }
        (&raw[..end], &raw[end..])
    };
    let effective = if rest.is_empty() {
        implicit
    } else {
        let Some(digits) = rest.strip_prefix(':') else {
            return false;
        };
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let Ok(value) = digits.parse::<u16>() else {
            return false;
        };
        Some(value)
    };
    loopback(host) && effective.is_none_or(|value| value == port)
}

fn rejection(headers: &HeaderMap, port: u16) -> Option<&'static str> {
    if !headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| allowed(s, None, port))
    {
        return Some("Forbidden: unrecognized Host header");
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let valid = origin
            .to_str()
            .ok()
            .and_then(|v| v.split_once("://"))
            .is_some_and(|(scheme, authority)| {
                let default = if scheme.eq_ignore_ascii_case("http") {
                    80
                } else if scheme.eq_ignore_ascii_case("https") {
                    443
                } else {
                    return false;
                };
                allowed(authority, Some(default), port)
            });
        if !valid {
            return Some("Forbidden: cross-origin request");
        }
    }
    None
}

async fn guard(State(port): State<u16>, request: Request, next: Next) -> Response {
    if let Some(reason) = rejection(request.headers(), port) {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }
    next.run(request).await
}

async fn session_response(request: Request, next: Next) -> Response {
    let closing = request.method() == axum::http::Method::DELETE;
    let session = request.headers().get("mcp-session-id").cloned();
    let mut response = next.run(request).await;
    // The pinned clients require 200 for explicit session termination.
    if closing && response.status() == StatusCode::ACCEPTED {
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        if let Some(id) = session {
            response.headers_mut().insert("mcp-session-id", id);
        }
    }
    response
}

pub(in crate::cli) fn serve(
    root: &Path,
    tools: Option<String>,
    host: &str,
    port: u16,
    multi_repo: bool,
) -> Result<()> {
    let server = super::server(root, tools, multi_repo)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let bind_host = host
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(host);
            let listener = tokio::net::TcpListener::bind((bind_host, port)).await?;
            // The middleware handles the entire loopback range and checks the bound port.
            let config = StreamableHttpServerConfig::default().disable_allowed_hosts();
            let service = StreamableHttpService::new(
                move || Ok(server.clone()),
                Arc::new(LocalSessionManager::default()),
                config,
            );
            let mut app = Router::new()
                .route_service("/mcp", service.clone())
                .route_service("/mcp/", service)
                .layer(middleware::from_fn(session_response));
            if loopback(host) {
                app = app.layer(middleware::from_fn_with_state(
                    listener.local_addr()?.port(),
                    guard,
                ));
            }
            axum::serve(listener, app).await?;
            Ok(())
        })
}

#[derive(clap::Args)]
pub(in crate::cli) struct Options {
    #[arg(long)]
    http: bool,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    multi_repo: bool,
}

impl Options {
    pub(in crate::cli) fn run(
        self,
        repo: Option<&Path>,
        tools: Option<String>,
        auto_watch: bool,
        multi_worktree: bool,
    ) -> Result<()> {
        let error = if self.port.is_some() && !self.http {
            Some("--port requires --http")
        } else if self.host.is_some() && !self.http {
            Some("--host requires --http")
        } else if self.multi_repo && !self.http {
            Some("--multi-repo requires --http")
        } else if self.multi_repo && multi_worktree {
            Some("--multi-repo cannot be combined with --multi-worktree")
        } else if self.multi_repo && repo.is_some() {
            Some(
                "--multi-repo serves the repositories in the registry; drop --repo and run `code-review-graph register <path>` for each one",
            )
        } else if self.multi_repo && auto_watch {
            Some(
                "--multi-repo cannot be combined with --auto-watch; use `code-review-graph daemon` to watch several repositories",
            )
        } else if self.multi_repo
            && std::env::var("CRG_DATA_DIR").is_ok_and(|s| !s.trim().is_empty())
        {
            Some(
                "--multi-repo cannot be combined with CRG_DATA_DIR, which would put every repository's graph in one database; use per-repository Registry data_dir entries instead",
            )
        } else if multi_worktree && self.http {
            Some("--multi-worktree is only supported for stdio")
        } else {
            None
        };
        if let Some(message) = error {
            eprintln!(
                "usage: code-review-graph serve [-h] [--repo REPO] [--auto-watch]\n                               [--multi-worktree] [--tools TOOLS] [--http]\n                               [--host ADDR] [--port PORT] [--multi-repo]\ncode-review-graph serve: error: {message}"
            );
            std::process::exit(2);
        }
        if multi_worktree {
            return super::worktree::serve("serve", repo, tools, auto_watch);
        }
        let root = super::super::root(repo, false)?;
        super::super::watch::with_auto(&root, auto_watch, || {
            if self.http {
                serve(
                    &root,
                    tools,
                    self.host.as_deref().unwrap_or("127.0.0.1"),
                    self.port.unwrap_or(5555),
                    self.multi_repo,
                )
            } else {
                super::serve(&root, tools)
            }
        })
    }
}
