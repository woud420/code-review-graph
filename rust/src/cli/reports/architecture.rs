use super::*;
use std::collections::HashMap;

struct Coupling {
    a: i64,
    b: i64,
    count: usize,
    kinds: Vec<(String, usize)>,
}
pub(super) fn tokens(v: &Value) -> usize {
    let raw = v.to_string();
    raw.chars()
        .map(|c| if c.is_ascii() { 1 } else { 6 * c.len_utf16() })
        .sum::<usize>()
        .div_ceil(4)
}

pub(super) fn run(conn: &Connection, args: &Value, hints: &Hints) -> Result<Value> {
    let cap = positive(args, "max_results", 100, 200)?;
    let member_cap = positive(args, "max_members", 10, 25)?;
    let minimal = args["detail_level"].as_str().unwrap_or("minimal") == "minimal";
    let mut groups = group_rows(conn, "size DESC", 0)?;
    let mut names = HashMap::new();
    let mut membership = HashMap::new();
    let mut members = conn.prepare("SELECT qualified_name FROM nodes WHERE community_id=?")?;
    for group in &mut groups {
        let id = group["id"].as_i64().unwrap_or_default();
        names.insert(id, text(group, "name").to_owned());
        let qns = members
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let clean_qns: Vec<_> = qns.iter().map(|q| clean(q)).collect();
        for q in &clean_qns {
            membership.insert(q.clone(), id);
        }
        group["members"] = json!(clean_qns);
    }
    let mut links = Vec::new();
    let mut pair_order = HashMap::new();
    let mut pairs: Vec<Coupling> = Vec::new();
    let mut stmt=conn.prepare("SELECT source_qualified,target_qualified,kind FROM edges WHERE kind!='TESTED_BY' ORDER BY id")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let from: String = row.get(0)?;
        let to: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let (Some(&a), Some(&b)) = (membership.get(&from), membership.get(&to)) else {
            continue;
        };
        if a == b {
            continue;
        }
        let (low, high) = (a.min(b), a.max(b));
        let idx = *pair_order.entry((low, high)).or_insert_with(|| {
            pairs.push(Coupling {
                a: low,
                b: high,
                count: 0,
                kinds: Vec::new(),
            });
            pairs.len() - 1
        });
        let pair = &mut pairs[idx];
        pair.count += 1;
        if let Some((_, count)) = pair.kinds.iter_mut().find(|(k, _)| k == &kind) {
            *count += 1;
        } else {
            pair.kinds.push((kind.clone(), 1));
        }
        links.push(json!({"source_community":a,"target_community":b,"edge_kind":kind,"source":clean(&from),"target":clean(&to)}));
    }
    pairs.sort_by_key(|p| std::cmp::Reverse(p.count));
    let test = regex::Regex::new(
        r"(?i)(^test[-/]|[-/]test([:/]|$)|it:should|describe:|spec[-/]|[-/]spec$)",
    )?;
    let mut warnings: Vec<_> = pairs
        .iter()
        .filter(|p| p.count > 10 && !test.is_match(&names[&p.a]) && !test.is_match(&names[&p.b]))
        .map(|p| {
            format!(
                "High coupling ({} edges) between '{}' and '{}'",
                p.count, names[&p.a], names[&p.b]
            )
        })
        .collect();
    let baseline = if minimal {
        tokens(&json!({"communities":groups,"cross_community_edges":links,"warnings":warnings}))
    } else {
        0
    };
    if minimal {
        groups = groups
            .iter()
            .map(|g| subset(g, &["id", "name", "size", "cohesion", "dominant_language"]))
            .collect();
        links=pairs.into_iter().map(|mut p|{
            p.kinds.sort_by_key(|(_,count)|std::cmp::Reverse(*count));
            let kinds=p.kinds.into_iter().take(3).map(|(k,_)|k).collect::<Vec<_>>();
            json!({"source_community":names[&p.a],"target_community":names[&p.b],"edge_count":p.count,"top_kinds":kinds})
        }).collect();
    } else {
        for group in &mut groups {
            if let Some(members) = group["members"].as_array_mut()
                && members.len() > member_cap
            {
                let total = members.len();
                members.truncate(member_cap);
                group["members_total"] = json!(total);
                group["members_truncated"] = json!(true);
            }
        }
    }
    let total = links.len();
    let warning_count = warnings.len();
    links.truncate(cap);
    warnings.truncate(cap);
    let label = if minimal {
        "community pairs"
    } else {
        "cross-community edges"
    };
    let mut out = json!({"status":"ok","summary":format!("Architecture: {} communities, {total} {label}{}, {warning_count} warning(s)",groups.len(),shown(links.len(),total)),"communities":groups,"cross_community_edges":links,"warnings":warnings,"cross_community_edges_total":total,"truncated":total>links.len()||warning_count>warnings.len()});
    hints.add(&mut out, "get_architecture_overview_tool");
    if baseline > 0 {
        let returned = tokens(&out);
        super::super::history::record(
            conn,
            "get_architecture_overview_tool",
            "minimal",
            None,
            baseline,
            returned,
        );
        let saved = baseline.saturating_sub(returned);
        out["context_savings"] = json!({"estimated":true,"saved_tokens":saved,"saved_percent":((saved as f64/baseline as f64)*100.).round_ties_even() as usize});
    }
    Ok(out)
}
