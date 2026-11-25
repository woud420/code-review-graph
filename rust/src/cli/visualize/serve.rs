use anyhow::Result;
use axum::{
    Router,
    extract::{Path as Segment, State},
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use std::{
    fmt::Write as _,
    fs,
    path::{Component, Path, PathBuf},
};

fn html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn link(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            write!(out, "%{b:02X}").unwrap();
        }
    }
    out
}

fn directory(path: &Path, display: &str) -> std::io::Result<Response> {
    let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    let title = format!("Directory listing for {}", html(display));
    let mut body = format!(
        "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n</head>\n<body>\n<h1>{title}</h1>\n<hr>\n<ul>\n"
    );
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let target = if path.is_dir() {
            format!("{name}/")
        } else {
            name.clone()
        };
        let label = if path.is_symlink() {
            format!("{name}@")
        } else {
            target.clone()
        };
        writeln!(
            body,
            "<li><a href=\"{}\">{}</a></li>",
            link(&target),
            html(&label)
        )
        .unwrap();
    }
    body.push_str("</ul>\n<hr>\n</body>\n</html>\n");
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response())
}

fn response(dir: &Path, name: &str, uri: &Uri) -> Option<Response> {
    let path = Path::new(name);
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let mut file = dir.join(path).canonicalize().ok()?;
    if !file.starts_with(dir) {
        return None;
    }
    if file.is_dir() {
        if !uri.path().ends_with('/') {
            let mut to = format!("{}/", uri.path());
            if let Some(q) = uri.query() {
                write!(to, "?{q}").unwrap();
            }
            return Some(Redirect::permanent(&to).into_response());
        }
        match ["index.html", "index.htm"]
            .map(|name| file.join(name))
            .into_iter()
            .find(|p| p.is_file())
        {
            Some(index) => {
                file = index.canonicalize().ok()?;
                if !file.starts_with(dir) {
                    return None;
                }
            }
            None => return directory(&file, &format!("/{}", name)).ok(),
        }
    }
    let mime = match file.extension().and_then(|s| s.to_str()) {
        Some("html" | "htm") => "text/html",
        Some("js" | "mjs") => "text/javascript",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("css") => "text/css",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    Some(([(header::CONTENT_TYPE, mime)], fs::read(file).ok()?).into_response())
}

async fn asset(dir: PathBuf, name: String, uri: Uri) -> Response {
    tokio::task::spawn_blocking(move || response(&dir, &name, &uri))
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

pub(super) fn run(dir: &Path) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 8765)).await?;
            let router = Router::new()
                .route(
                    "/",
                    get(|State(dir), uri: Uri| asset(dir, String::new(), uri)),
                )
                .route(
                    "/{*name}",
                    get(|State(dir), Segment(name), uri: Uri| asset(dir, name, uri)),
                )
                .with_state(dir.canonicalize()?);
            println!("Serving at http://localhost:8765/graph.html\nPress Ctrl+C to stop.");
            std::io::Write::flush(&mut std::io::stdout())?;
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            println!("\nServer stopped.");
            Ok(())
        })
}
