mod layout;
use crate::cli::query::text;
use anyhow::{Result, ensure};
use indexmap::IndexMap;
use serde_json::Value;
use std::{collections::BTreeSet, fmt::Write as _, path::Path};

fn xml(s: &str) -> String {
    s.chars()
        .filter(|c| {
            matches!(*c, '\t' | '\n' | '\r') || (*c >= ' ' && *c != '\u{fffe}' && *c != '\u{ffff}')
        })
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn color(kind: &str) -> &'static str {
    match kind {
        "File" => "#6c757d",
        "Class" => "#0d6efd",
        "Function" => "#198754",
        "Type" => "#ffc107",
        "Test" => "#dc3545",
        _ => "#adb5bd",
    }
}

pub(super) fn write(data: &Value, path: &Path) -> Result<()> {
    let mut nodes = IndexMap::new();
    for node in data["nodes"].as_array().unwrap() {
        nodes.insert(text(node, "qualified_name"), node);
    }
    ensure!(!nodes.is_empty(), "Graph is empty, nothing to export");
    let mut links = vec![BTreeSet::new(); nodes.len()];
    for e in data["edges"].as_array().unwrap() {
        if let (Some(a), Some(b)) = (
            nodes.get_index_of(text(e, "source")),
            nodes.get_index_of(text(e, "target")),
        ) {
            links[a].insert(b);
        }
    }
    let pos = layout::positions(&links);
    let mut out = String::from(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 900 677" role="img" aria-labelledby="title">
<title id="title">Code Review Graph</title>
<defs><marker id="arrow" markerWidth="4" markerHeight="4" refX="3.8" refY="2" orient="auto"><path d="M0,0 L4,2 L0,4 z"/></marker></defs>
<rect width="900" height="677" fill="white"/>
<g id="edges" fill="none" stroke="black" opacity="0.2" marker-end="url(#arrow)">
"##,
    );
    for (a, targets) in links.iter().enumerate() {
        let [x, y] = pos[a];
        for &b in targets {
            let [u, v] = pos[b];
            if a == b {
                writeln!(out, "<path d=\"M{x:.4},{y:.4} c-13,-19 13,-19 0,-3\"/>")?;
            } else {
                let len = (u - x).hypot(v - y).max(0.001);
                let (ux, uy) = ((u - x) / len, (v - y) / len);
                writeln!(
                    out,
                    "<path d=\"M{:.4},{:.4} L{:.4},{:.4}\"/>",
                    x + 3. * ux,
                    y + 3. * uy,
                    u - 3. * ux,
                    v - 3. * uy
                )?;
            }
        }
    }
    out.push_str("</g>\n<g id=\"nodes\" opacity=\"0.8\">\n");
    for (node, &[x, y]) in nodes.values().zip(&pos) {
        writeln!(
            out,
            "<circle cx=\"{x:.4}\" cy=\"{y:.4}\" r=\"2.74\" fill=\"{}\"/>",
            color(text(node, "kind"))
        )?;
    }
    out.push_str("</g>\n");
    if nodes.len() <= 100 {
        out.push_str("<g id=\"labels\" font-family=\"sans-serif\" font-size=\"6\" text-anchor=\"middle\" dominant-baseline=\"central\">\n");
        for (node, &[x, y]) in nodes.values().zip(&pos) {
            let label = text(node, "name").split('\n').next().unwrap_or_default();
            writeln!(out, "<text x=\"{x:.4}\" y=\"{y:.4}\">{}</text>", xml(label))?;
        }
        out.push_str("</g>\n");
    }
    out.push_str("<text x=\"450\" y=\"20\" text-anchor=\"middle\" font-family=\"sans-serif\" font-size=\"14\">Code Review Graph</text>\n</svg>\n");
    super::save(path, out.as_bytes())?;
    eprintln!(
        "INFO: SVG exported to {} ({} nodes)",
        path.display(),
        nodes.len()
    );
    println!("SVG exported: {}", path.display());
    Ok(())
}
