use super::*;
use crate::cli::store;

const SECTIONS: [&str; 7] = [
    "risk",
    "communities",
    "hubs",
    "coupling",
    "impact",
    "gaps",
    "graph",
];
pub(in crate::cli) fn parse(raw: &str) -> std::result::Result<Vec<String>, String> {
    let requested: Vec<_> = raw
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let unknown: Vec<_> = requested
        .iter()
        .map(String::as_str)
        .filter(|k| *k != "all" && !SECTIONS.contains(k))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown detail section(s): {}; choose from {}, or all",
            unknown.join(", "),
            SECTIONS.join(", ")
        ));
    }
    Ok(SECTIONS
        .iter()
        .filter(|k| requested.iter().any(|s| s == "all" || s == **k))
        .map(|s| s.to_string())
        .collect())
}

fn items<'a>(report: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    report[key].as_array().into_iter().flatten()
}
fn location(n: &Value) -> Value {
    subset(n, &["qualified_name", "file_path", "line_start"])
}
fn ids(report: &Value) -> Result<String> {
    Ok(serde_json::to_string(
        &items(report, "changed_functions")
            .map(|n| &n["id"])
            .collect::<Vec<_>>(),
    )?)
}
fn communities(conn: &Connection, ids: &str) -> Result<Value> {
    let total: i64 = conn.query_row("SELECT count(*) FROM communities", [], |r| r.get(0))?;
    let mut touched = Vec::new();
    let mut unassigned = 0;
    let mut stmt=conn.prepare("SELECT n.community_id,c.name,c.size,count(*) FROM nodes n LEFT JOIN communities c ON c.id=n.community_id WHERE n.id IN(SELECT value FROM json_each(?1)) GROUP BY n.community_id ORDER BY count(*) DESC,n.community_id")?;
    let mut rows = stmt.query([ids])?;
    let mut count = 0;
    while let Some(r) = rows.next()? {
        let n: i64 = r.get(3)?;
        if let Some(id) = r.get::<_, Option<i64>>(0)? {
            count += 1;
            if touched.len() < 10 {
                touched.push(json!({"id":id,"name":clean(&r.get::<_,Option<String>>(1)?.unwrap_or_else(||"?".into())),"size":r.get::<_,Option<i64>>(2)?.unwrap_or_default(),"changed":n}));
            }
        } else {
            unassigned = n;
        }
    }
    Ok(json!({"in_graph":total,"touched_total":count,"unassigned":unassigned,"touched":touched}))
}

fn hubs(conn: &Connection, report: &Value) -> Result<Value> {
    let mut degree = HashMap::new();
    let mut stmt=conn.prepare("SELECT qualified_name,(SELECT count(*) FROM edges e WHERE e.target_qualified=n.qualified_name)+(SELECT count(*) FROM edges e WHERE e.source_qualified=n.qualified_name) FROM nodes n WHERE kind NOT IN('File','Test') AND NOT is_test")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (s, d) = row?;
        if d > 0 {
            degree.insert(s, d);
        }
    }
    let mut candidates = items(report, "changed_functions")
        .filter_map(|n| degree.get(text(n, "qualified_name")).map(|d| (n, *d)))
        .collect::<Vec<_>>();
    candidates.sort_by(|(a, x), (b, y)| {
        y.cmp(x)
            .then_with(|| text(a, "qualified_name").cmp(text(b, "qualified_name")))
    });
    let total = candidates.len();
    let mut out = Vec::new();
    for (n, d) in candidates.into_iter().take(10) {
        let linked:i64=conn.query_row("SELECT count(DISTINCT c.community_id) FROM nodes current JOIN edges e ON e.kind='CALLS' AND (e.source_qualified=current.qualified_name OR e.target_qualified=current.qualified_name) JOIN nodes c ON c.qualified_name=CASE WHEN e.source_qualified=current.qualified_name THEN e.target_qualified ELSE e.source_qualified END WHERE current.id=?1 AND c.community_id IS NOT current.community_id",[n["id"].as_i64()],|r|r.get(0))?;
        let mut x = location(n);
        x["degree"] = json!(d);
        x["rank"] = json!(1 + degree.values().filter(|&&i| i > d).count());
        x["communities_linked"] = json!(linked);
        out.push(x);
    }
    Ok(json!({"ranked":degree.len(),"total":total,"nodes":out}))
}
fn family(lang: &str) -> &str {
    match lang {
        "tsx" | "typescript" | "vue" | "svelte" => "javascript",
        "cpp" | "objc" => "c",
        "luau" => "lua",
        "notebook" => "python",
        s => s,
    }
}
fn coupling(conn: &Connection, ids: &str) -> Result<Value> {
    let mut stmt=conn.prepare("SELECT s.qualified_name,t.qualified_name,e.kind,s.language,t.language,s.community_id,t.community_id,sc.name,tc.name FROM edges e JOIN nodes s ON e.source_qualified=s.qualified_name JOIN nodes t ON e.target_qualified=t.qualified_name LEFT JOIN communities sc ON sc.id=s.community_id LEFT JOIN communities tc ON tc.id=t.community_id WHERE e.kind NOT IN('CONTAINS','TESTED_BY') AND s.kind NOT IN('File','Test') AND t.kind NOT IN('File','Test') AND NOT s.is_test AND NOT t.is_test AND (s.id IN(SELECT value FROM json_each(?1)) OR t.id IN(SELECT value FROM json_each(?1))) GROUP BY s.qualified_name,t.qualified_name,e.kind")?;
    let mut rows = stmt.query([ids])?;
    let mut items = Vec::new();
    let mut cross_community = 0;
    let mut cross_language = 0;
    while let Some(r) = rows.next()? {
        let src = r.get::<_, String>(0)?;
        let dst = r.get::<_, String>(1)?;
        let kind = r.get::<_, String>(2)?;
        let sl = r.get::<_, Option<String>>(3)?.unwrap_or_default();
        let tl = r.get::<_, Option<String>>(4)?.unwrap_or_default();
        let cross = !sl.is_empty() && !tl.is_empty() && family(&sl) != family(&tl);
        let communities = r
            .get::<_, Option<i64>>(5)?
            .zip(r.get::<_, Option<i64>>(6)?)
            .is_some_and(|(a, b)| a != b);
        if !cross && !communities {
            continue;
        }
        let mut reasons = Vec::new();
        if cross {
            cross_language += 1;
            reasons.push("cross-language")
        }
        if communities {
            cross_community += 1;
            reasons.push("cross-community")
        }
        let row = json!({"source":clean(&src),"target":clean(&dst),"kind":kind,"reasons":reasons,"source_language":sl,"target_language":tl,"source_community":r.get::<_,Option<String>>(7)?.map(|s|clean(&s)),"target_community":r.get::<_,Option<String>>(8)?.map(|s|clean(&s))});
        items.push(((2 - reasons.len(), !cross, src, dst, kind), row));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(
        json!({"total":items.len(),"cross_community":cross_community,"cross_language":cross_language,"edges":items.into_iter().take(10).map(|(_,v)|v).collect::<Vec<_>>()}),
    )
}
fn callers(conn: &Connection, n: &Value) -> Result<Vec<String>> {
    Ok(conn.prepare("SELECT DISTINCT source_qualified FROM edges WHERE kind='CALLS' AND target_qualified=?1 ORDER BY source_qualified")?.query_map([text(n,"qualified_name")],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

fn impact(conn: &Connection, root: &Path, files: &[String], report: &Value) -> Result<Value> {
    let depth = cap("CRG_MAX_IMPACT_DEPTH", 2);
    let radius = super::impact::traversal::run(
        conn,
        root,
        files,
        depth,
        cap("CRG_MAX_IMPACT_NODES", 500),
        false,
    )?;
    let mut counts = HashMap::new();
    for n in &radius.impacted {
        *counts.entry(text(n, "file_path")).or_insert(0usize) += 1;
    }
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(a, x), (b, y)| y.cmp(x).then_with(|| a.cmp(b)));
    let mut named = Vec::new();
    for n in items(report, "review_priorities") {
        let sources = callers(conn, n)?;
        if !sources.is_empty() {
            let mut r = location(n);
            r["total"] = json!(sources.len());
            r["callers"] = json!(sources.iter().take(5).map(|s| clean(s)).collect::<Vec<_>>());
            named.push(r);
        }
    }
    Ok(
        json!({"depth":depth,"impacted_nodes":radius.total,"impacted_files":ranked.len(),"truncated":radius.total>radius.impacted.len(),"top_files":ranked.into_iter().take(10).map(|(p,n)|json!({"file_path":p,"nodes":n})).collect::<Vec<_>>(),"callers":named}),
    )
}
fn gaps(conn: &Connection, report: &Value) -> Result<Value> {
    let mut rows = Vec::new();
    for gap in items(report, "test_gaps") {
        let qn = text(gap, "qualified_name");
        let callers = callers(conn, gap)?.len();
        let flows:i64=conn.query_row("SELECT count(*) FROM flow_memberships m JOIN nodes n ON n.id=m.node_id WHERE n.qualified_name=?1",[qn],|r|r.get(0))?;
        rows.push(((std::cmp::Reverse(callers),std::cmp::Reverse(flows),qn),json!({"qualified_name":qn,"file_path":gap["file"],"line_start":gap["line_start"],"callers":callers,"flows":flows,"coverage":gap["coverage"]})));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(
        json!({"total":report["test_gaps_total"],"hotspots":rows.into_iter().take(10).map(|(_,v)|v).collect::<Vec<_>>()}),
    )
}
fn graph(conn: &Connection) -> Result<Value> {
    let counts = store::Counts::read(conn)?;
    let count = |sql| conn.query_row(sql, [], |r| r.get::<_, i64>(0));
    Ok(
        json!({"nodes":counts.nodes(),"edges":counts.edges(),"files":counts.files(),"languages":&counts.languages[..counts.languages.len().min(20)],"communities":count("SELECT count(*) FROM communities")?,"community_detector":store::metadata(conn,"community_detector")?,"igraph_installed":false,"flows":count("SELECT count(*) FROM flows")?,"last_updated":counts.updated,"build_type":store::metadata(conn,"last_build_type")?,"version":"2.4.0","schema_version":store::metadata(conn,"schema_version")?.and_then(|s|s.parse::<i64>().ok())}),
    )
}
pub(super) fn build(
    conn: &Connection,
    root: &Path,
    files: &[String],
    report: &Value,
    sections: &[String],
    churn: bool,
) -> Result<Value> {
    let ids = ids(report)?;
    let mut details = json!({"sections":sections});
    for name in sections {
        details[name] = match name.as_str() {
            "risk" => {
                let (counts, _) = mapping::churn(root, churn);
                let mut rows = Vec::new();
                for n in items(report, "review_priorities") {
                    let mut data = risk::signals(
                        conn,
                        n,
                        root,
                        *counts.get(text(n, "file_path")).unwrap_or(&0),
                    )?;
                    for t in data.as_object_mut().unwrap().values_mut() {
                        t["score"] = json!(rounded(t["score"].as_f64().unwrap_or_default()));
                    }
                    let mut out = location(n);
                    out["risk_score"] = n["risk_score"].clone();
                    out["signals"] = data;
                    rows.push(out);
                }
                json!(rows)
            }
            "communities" => communities(conn, &ids)?,
            "hubs" => hubs(conn, report)?,
            "coupling" => coupling(conn, &ids)?,
            "impact" => impact(conn, root, files, report)?,
            "gaps" => gaps(conn, report)?,
            "graph" => graph(conn)?,
            _ => anyhow::bail!("unknown detail section: {name}"),
        };
    }
    Ok(details)
}
