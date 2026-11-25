mod data;
mod group;
use super::{query::text, wire};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;
use std::{collections::HashMap, fmt::Write as _, fs, path::Path};

fn graphml(data: &Value) -> Result<String> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\"\n  xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n  xsi:schemaLocation=\"http://graphml.graphdrawing.org/xmlns http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd\">\n",
    );
    for (key, node, kind) in [
        ("qualified_name", "node", "string"),
        ("name", "node", "string"),
        ("kind", "node", "string"),
        ("file", "node", "string"),
        ("language", "node", "string"),
        ("community", "node", "int"),
        ("edge_kind", "edge", "string"),
    ] {
        writeln!(
            out,
            "  <key id=\"{key}\" for=\"{node}\" attr.name=\"{}\" attr.type=\"{kind}\"/>",
            if key == "edge_kind" { "kind" } else { key }
        )?;
    }
    let xml = |s: &str| {
        s.chars()
            .filter(|c| {
                matches!(*c, '\t' | '\n' | '\r')
                    || (*c >= ' ' && *c != '\u{fffe}' && *c != '\u{ffff}')
            })
            .collect::<String>()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    out.push_str("  <graph id=\"code-review-graph\" edgedefault=\"directed\">\n");
    let mut ids = HashMap::new();
    for (id, n) in data["nodes"].as_array().unwrap().iter().enumerate() {
        ids.entry(text(n, "qualified_name")).or_insert(id);
        writeln!(out, "    <node id=\"n{id}\">")?;
        for (k, field) in [
            ("qualified_name", "qualified_name"),
            ("name", "name"),
            ("kind", "kind"),
            ("file", "file_path"),
            ("language", "language"),
        ] {
            writeln!(
                out,
                "      <data key=\"{k}\">{}</data>",
                xml(text(n, field))
            )?;
        }
        if let Some(cid) = n["community_id"].as_i64() {
            writeln!(out, "      <data key=\"community\">{cid}</data>")?;
        }
        out.push_str("    </node>\n");
    }
    for (id, e) in data["edges"].as_array().unwrap().iter().enumerate() {
        let (Some(a), Some(b)) = (ids.get(text(e, "source")), ids.get(text(e, "target"))) else {
            continue;
        };
        writeln!(
            out,
            "    <edge id=\"e{id}\" source=\"n{a}\" target=\"n{b}\">\n      <data key=\"edge_kind\">{}</data>\n    </edge>",
            xml(text(e, "kind"))
        )?;
    }
    out.push_str("  </graph>\n</graphml>");
    Ok(out)
}

fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut out = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    out.write_all(bytes)?;
    out.as_file().sync_all()?;
    out.persist(path)?;
    Ok(())
}

pub(super) fn write(
    conn: &Connection,
    db: &Path,
    format: &str,
    mode: &str,
    sidecar: bool,
) -> Result<()> {
    if format != "html" && (sidecar || mode != "auto") {
        eprintln!(
            "visualize: {} has no effect on --format {format}",
            if sidecar { "--sidecar" } else { "--mode" }
        );
        std::process::exit(2);
    }
    let path = db.with_file_name(format!("graph.{format}"));
    let mut data = data::read(conn)?;
    if format != "html" {
        let body = match format {
            "json" => format!("{}\n", serde_json::to_string_pretty(&data)?),
            "graphml" => graphml(&data)?,
            _ => unreachable!(),
        };
        save(&path, body.as_bytes())?;
        let label = if format == "json" { "JSON" } else { "GraphML" };
        if format == "json" {
            eprintln!("INFO: {label} exported to {}", path.display());
        } else {
            eprintln!(
                "INFO: {label} exported to {} ({} nodes, {} edges)",
                path.display(),
                data["nodes"].as_array().unwrap().len(),
                data["edges"].as_array().unwrap().len()
            );
        }
        println!("{label} exported: {}", path.display());
        return Ok(());
    }
    let nodes = data["nodes"].as_array().unwrap();
    let edges = data["edges"].as_array().unwrap();
    let effective = if mode != "auto" {
        mode
    } else if nodes.len() <= 3000 && edges.len() <= 9000 {
        "full"
    } else if !data["communities"].as_array().unwrap().is_empty()
        || nodes.iter().any(|n| !n["community_id"].is_null())
    {
        "community"
    } else {
        "file"
    };
    if mode == "auto" && effective != "full" {
        eprintln!(
            "INFO: auto mode: {} nodes / {} edges exceeds full-render budget (3000 nodes / 9000 edges) — using {effective} aggregation",
            nodes.len(),
            edges.len()
        );
    }
    let template = if effective == "full" {
        include_str!("../../../assets/visualization/full.html")
    } else {
        data = group::aggregate(&data, effective);
        include_str!("../../../assets/visualization/aggregated.html")
    };
    let data = wire::json_text(&data, false).replace("</", "<\\/");
    let (script, payload) = if sidecar {
        save(
            &db.with_file_name("graph.data.js"),
            format!("window.__CRG_GRAPH_DATA__ = {data};\n").as_bytes(),
        )?;
        (
            "<script src=\"graph.data.js\"></script>",
            "window.__CRG_GRAPH_DATA__",
        )
    } else {
        ("", data.as_str())
    };
    let html = template
        .replace("__SIDECAR_SCRIPT__", script)
        .replace("__GRAPH_DATA__", payload);
    let library = db.with_file_name("d3.v7.min.js");
    let expected = include_bytes!("../../../assets/visualization/d3.v7.min.js").as_slice();
    if fs::read(&library).as_deref().ok() != Some(expected) {
        save(&library, expected)?;
    }
    save(&path, html.as_bytes())?;
    println!("Visualisation ({mode}): {}", path.display());
    if sidecar {
        println!(
            "Payload sidecar: {}",
            db.with_file_name("graph.data.js").display()
        );
    }
    println!("Open in browser to explore.");
    Ok(())
}
