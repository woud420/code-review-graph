use crate::cli::query::clean;
use anyhow::{Result, bail};
use rusqlite::{Connection, Row, types::ValueRef};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(super) const TABLES: [&str; 8] = [
    "nodes",
    "node_community",
    "edges",
    "communities",
    "flows",
    "flow_memberships",
    "nodes_fts",
    "metadata",
];
type Counts = BTreeMap<String, usize>;
type Projection = BTreeMap<String, Counts>;

fn field(row: &Row<'_>, i: usize) -> rusqlite::Result<String> {
    Ok(match row.get_ref(i)? {
        ValueRef::Null => String::new(),
        ValueRef::Integer(v) => v.to_string(),
        ValueRef::Real(v) => json!(v).to_string(),
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned(),
        ValueRef::Blob(v) => String::from_utf8_lossy(v).into_owned(),
    })
}

fn project(db: &Connection, table: &str) -> Result<Option<Projection>> {
    let flow_keys = "WITH flow_keys AS (
       SELECT f.id, coalesce(n.qualified_name,'<unresolved>') || '::' ||
       coalesce((SELECT group_concat(coalesce(p.qualified_name,'<unresolved>'), '>' ORDER BY CAST(w.key AS INTEGER))
          FROM json_each(CASE WHEN json_valid(f.path_json) THEN f.path_json ELSE '[]' END) w
          LEFT JOIN nodes p ON p.id=w.value),'') AS name FROM flows f LEFT JOIN nodes n ON n.id=f.entry_point_id)";
    let (sql,keys) = match table {
        "nodes"=>("SELECT qualified_name,kind,name,file_path,line_start,line_end,language,parent_name,params,return_type,modifiers,is_test,file_hash,extra,symbol,signature FROM nodes".into(),1),
        "node_community"=>("SELECT n.qualified_name,c.name FROM nodes n LEFT JOIN communities c ON c.id=n.community_id".into(),1),
        "edges"=>("SELECT kind,source_qualified,target_qualified,file_path,line,extra,confidence,confidence_tier FROM edges".into(),5),
        "communities"=>("SELECT c.name,c.level,p.name,c.cohesion,c.size,c.dominant_language,c.description FROM communities c LEFT JOIN communities p ON p.id=c.parent_id".into(),1),
        "flows"=>(format!("{flow_keys} SELECT k.name,f.name,f.depth,f.node_count,f.file_count,f.criticality FROM flows f JOIN flow_keys k ON k.id=f.id"),1),
        "flow_memberships"=>(format!("{flow_keys} SELECT coalesce(k.name,'<orphan flow>'),n.qualified_name,m.position FROM flow_memberships m LEFT JOIN flow_keys k ON k.id=m.flow_id LEFT JOIN nodes n ON n.id=m.node_id"),2),
        "nodes_fts"=>{
            if db.execute_batch("CREATE VIRTUAL TABLE temp.crg_fidelity_vocab USING fts5vocab(main,nodes_fts,'row')").is_err(){return Ok(None)}
            ("SELECT term,doc,cnt FROM temp.crg_fidelity_vocab".into(),1)
        }
        "metadata"=>("SELECT key,value FROM metadata WHERE key NOT IN ('last_updated','last_build_type','last_postprocessed_at')".into(),1),
        _=>bail!("unknown fidelity table {table}")
    };
    let mut stmt = db.prepare(&sql)?;
    let cols = stmt.column_count();
    let mut data = Projection::new();
    let rows = stmt.query_map([], |r| {
        let values = (0..cols)
            .map(|i| field(r, i))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((values[..keys].join("|"), values[keys..].join("|")))
    })?;
    for row in rows {
        let (key, value) = row?;
        *data.entry(key).or_default().entry(value).or_default() += 1;
    }
    Ok(Some(data))
}

fn summary(set: &Counts) -> String {
    if set.is_empty() {
        return "<absent>".into();
    }
    set.iter()
        .map(|(s, n)| {
            if *n == 1 {
                s.clone()
            } else {
                format!("{s} (x{n})")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
        .chars()
        .take(300)
        .collect()
}

fn delta(left: &Counts, right: &Counts) -> usize {
    left.iter()
        .map(|(s, n)| n.saturating_sub(right.get(s).copied().unwrap_or_default()))
        .sum()
}

fn compare(left: &Projection, right: &Projection, prefix: &str) -> (usize, String) {
    let mut differing = 0;
    let mut example = String::new();
    let blank = Counts::new();
    let strip = |s: &str| clean(&s.replace(prefix, ""));
    for key in left.keys().chain(right.keys()).collect::<BTreeSet<_>>() {
        let here = left.get(key).unwrap_or(&blank);
        let there = right.get(key).unwrap_or(&blank);
        if here != there {
            differing += delta(here, there).max(delta(there, here));
            if example.is_empty() {
                example = format!(
                    "{} :: incremental={} rebuild={}",
                    strip(key),
                    strip(&summary(here)),
                    strip(&summary(there))
                );
            }
        }
    }
    (differing, example)
}

fn names(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let sql = if table == "nodes" {
        "SELECT qualified_name FROM nodes"
    } else {
        "SELECT source_qualified||'->'||target_qualified||':'||kind FROM edges"
    };
    Ok(conn
        .prepare(sql)?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(super) fn record(
    row: &mut Value,
    incremental: &Path,
    rebuild: &Path,
    tree: &Path,
) -> Result<()> {
    let left = Connection::open(incremental)?;
    let right = Connection::open(rebuild)?;
    let prefix = format!("{}/", tree.display());
    let mut total = 0;
    let mut worst = 0;
    let mut unchecked = Vec::new();
    for table in TABLES {
        let (Some(a), Some(b)) = (project(&left, table)?, project(&right, table)?) else {
            unchecked.push(table);
            continue;
        };
        let (count, example) = compare(&a, &b, &prefix);
        total += count;
        row[format!("{table}_diff")] = json!(count);
        if count > worst {
            worst = count;
            row["worst_table"] = json!(table);
            row["example"] = json!(format!("{table}: {example}"));
        }
    }
    for table in ["nodes", "edges"] {
        let (a, b) = (names(&left, table)?, names(&right, table)?);
        row[format!("snapshot_{table}_only_in_rebuild")] = json!(b.difference(&a).count());
        row[format!("snapshot_{table}_only_in_incremental")] = json!(a.difference(&b).count());
    }
    row["total_differing_rows"] = json!(total);
    row["unchecked_tables"] = json!(unchecked.join(","));
    row["status"] = json!(if total == 0 { "ok" } else { "known_failure" });
    Ok(())
}
