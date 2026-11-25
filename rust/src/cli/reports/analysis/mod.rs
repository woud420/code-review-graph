mod cycles;
use super::*;
use std::collections::{HashMap, HashSet};

struct Entity {
    name: String,
    qn: String,
    kind: String,
    file: String,
    community: Option<i64>,
    test: bool,
}
struct Link {
    src: usize,
    dst: usize,
    kind: String,
}
struct Topology {
    entities: Vec<Entity>,
    symbols: HashMap<String, usize>,
    keys: Vec<String>,
    links: Vec<Link>,
    adj: Vec<Vec<usize>>,
    incoming: Vec<usize>,
    outgoing: Vec<usize>,
}
impl Topology {
    fn load(conn: &Connection) -> Result<Self> {
        let entities:Vec<_>=conn.prepare("SELECT name,qualified_name,kind,file_path,community_id,is_test FROM nodes WHERE kind!='File' ORDER BY id")?.query_map([],|r|Ok(Entity{name:clean(&r.get::<_,String>(0)?),qn:r.get(1)?,kind:r.get(2)?,file:r.get(3)?,community:r.get(4)?,test:r.get(5)?}))?.collect::<rusqlite::Result<_>>()?;
        let mut symbols = HashMap::new();
        let mut keys = Vec::new();
        let mut links = Vec::new();
        let mut stmt =
            conn.prepare("SELECT source_qualified,target_qualified,kind FROM edges ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut index = |name: String| {
                *symbols.entry(name.clone()).or_insert_with(|| {
                    keys.push(name);
                    keys.len() - 1
                })
            };
            let src = index(row.get(0)?);
            let dst = index(row.get(1)?);
            links.push(Link {
                src,
                dst,
                kind: row.get(2)?,
            });
        }
        let mut adj = vec![Vec::new(); keys.len()];
        let mut incoming = vec![0; keys.len()];
        let mut outgoing = vec![0; keys.len()];
        let mut distinct = HashSet::new();
        for e in &links {
            if distinct.insert((e.src, e.dst)) {
                adj[e.src].push(e.dst);
            }
            incoming[e.dst] += 1;
            outgoing[e.src] += 1;
        }
        Ok(Self {
            entities,
            symbols,
            keys,
            links,
            adj,
            incoming,
            outgoing,
        })
    }

    fn hubs(&self) -> Vec<Value> {
        let mut ranked = Vec::new();
        for n in &self.entities {
            let Some(&i) = self.symbols.get(&n.qn) else {
                continue;
            };
            let (ind, outd) = (self.incoming[i], self.outgoing[i]);
            ranked.push((ind+outd,json!({"name":n.name,"qualified_name":n.qn,"kind":n.kind,"file":n.file,"in_degree":ind,"out_degree":outd,"total_degree":ind+outd,"community_id":n.community})));
        }
        ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        ranked.into_iter().map(|(_, row)| row).collect()
    }

    fn bridges(&self) -> Vec<Value> {
        let n = self.keys.len();
        if n < 3 {
            return Vec::new();
        }
        let mut centrality = vec![0.; n];
        // The reference samples 500 starting nodes for graphs above 5,000.
        let mut sources: Vec<_> = (0..n).collect();
        if n > 5000 {
            sources.sort_unstable_by_key(|i| {
                blake3::hash(self.keys[*i].as_bytes()).as_bytes().to_owned()
            });
            sources.truncate(500);
        }
        let count = sources.len();
        for &source in &sources {
            let mut stack = Vec::new();
            let mut parents = vec![Vec::new(); n];
            let mut paths = vec![0.; n];
            paths[source] = 1.;
            let mut distance = vec![usize::MAX; n];
            distance[source] = 0;
            let mut queue = VecDeque::from([source]);
            while let Some(v) = queue.pop_front() {
                stack.push(v);
                for &w in &self.adj[v] {
                    if distance[w] == usize::MAX {
                        distance[w] = distance[v] + 1;
                        queue.push_back(w);
                    }
                    if distance[w] == distance[v] + 1 {
                        paths[w] += paths[v];
                        parents[w].push(v);
                    }
                }
            }
            let mut delta = vec![0.; n];
            for &w in stack.iter().rev() {
                let split = (1. + delta[w]) / paths[w];
                for &v in &parents[w] {
                    delta[v] += paths[v] * split;
                }
                if w != source {
                    centrality[w] += delta[w];
                }
            }
        }
        let nodes: HashMap<_, _> = self.entities.iter().map(|node| (&node.qn, node)).collect();
        let sources: HashSet<_> = sources.into_iter().collect();
        let mut result = Vec::new();
        for (i, &raw) in centrality.iter().enumerate() {
            if raw <= 0. {
                continue;
            }
            let Some(nod) = nodes.get(&self.keys[i]) else {
                continue;
            };
            let normal = if n == count || sources.contains(&i) {
                (count - 1) * (n - 2)
            } else {
                count * (n - 2)
            };
            let score = format!("{:.6}", raw / normal as f64)
                .parse::<f64>()
                .expect("finite score");
            result.push((score,json!({"name":nod.name,"qualified_name":nod.qn,"kind":nod.kind,"file":nod.file,"betweenness":score,"community_id":nod.community})));
        }
        result.sort_by(|(a, _), (b, _)| b.total_cmp(a));
        result.into_iter().map(|(_, row)| row).collect()
    }

    fn surprises(&self) -> Vec<Value> {
        let nodes: HashMap<_, _> = self.entities.iter().map(|n| (&n.qn, n)).collect();
        let mut degree: Vec<_> = self
            .incoming
            .iter()
            .zip(&self.outgoing)
            .map(|(a, b)| a + b)
            .collect();
        if degree.is_empty() {
            return Vec::new();
        }
        degree.sort_unstable();
        let threshold = (degree[degree.len() / 2] * 3).max(10);
        let mut result = Vec::new();
        for link in &self.links {
            let (Some(a), Some(b)) = (
                nodes.get(&self.keys[link.src]),
                nodes.get(&self.keys[link.dst]),
            ) else {
                continue;
            };
            let mut score = 0;
            let mut reasons = Vec::new();
            if a.community.is_some() && b.community.is_some() && a.community != b.community {
                score += 30;
                reasons.push("cross-community");
            }
            let ext = |file: &str| {
                file.rsplit_once('.')
                    .map(|(_, e)| e.to_owned())
                    .unwrap_or_default()
            };
            let a_ext = ext(&a.file);
            let b_ext = ext(&b.file);
            if !a_ext.is_empty() && !b_ext.is_empty() && a_ext != b_ext {
                score += 20;
                reasons.push("cross-language");
            }
            let da = self.incoming[link.src] + self.outgoing[link.src];
            let db = self.incoming[link.dst] + self.outgoing[link.dst];
            if da.min(db) <= 2 && da.max(db) >= threshold {
                score += 20;
                reasons.push("peripheral-to-hub");
            }
            if a.test != b.test && link.kind == "CALLS" {
                score += 15;
                reasons.push("cross-test-boundary");
            }
            if a.kind == "Type" && link.kind == "CALLS" {
                score += 15;
                reasons.push("unusual-edge-kind");
            }
            if score > 0 {
                result.push((score,json!({"source":a.name,"source_qualified":a.qn,"target":b.name,"target_qualified":b.qn,"edge_kind":link.kind,"surprise_score":score as f64/100.,"reasons":reasons,"source_community":a.community,"target_community":b.community})));
            }
        }
        result.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        result.into_iter().map(|(_, row)| row).collect()
    }
}

pub(super) const NAMES: [&str; 5] = [
    "get_hub_nodes_tool",
    "get_bridge_nodes_tool",
    "get_surprising_connections_tool",
    "get_knowledge_gaps_tool",
    "get_suggested_questions_tool",
];
pub(super) fn run(conn: &Connection, root: &Path, name: &str, args: &Value) -> Result<Value> {
    let cap = positive(args, "top_n", if name == NAMES[2] { 15 } else { 10 }, 100)?;
    let graph = Topology::load(conn)?;
    if name == "get_suggested_questions_tool" {
        return graph.questions(conn);
    }
    if name == "get_knowledge_gaps_tool" {
        return graph.knowledge(conn, root, args);
    }
    let (key, desc, fields, hints, mut rows) = match name {
        "get_hub_nodes_tool" => (
            "hub_nodes",
            "hub node(s) ranked by degree",
            vec!["name", "kind", "total_degree"],
            [
                "get_impact_radius_tool -- check blast radius of a hub",
                "query_graph_tool callers_of -- see what calls a hub",
                "get_bridge_nodes_tool -- find architectural chokepoints",
            ],
            graph.hubs(),
        ),
        "get_bridge_nodes_tool" => (
            "bridge_nodes",
            "bridge node(s) ranked by betweenness",
            vec!["name", "kind", "betweenness"],
            [
                "get_hub_nodes_tool -- find most connected nodes",
                "get_impact_radius_tool -- check blast radius",
                "detect_changes_tool -- see if bridges are affected",
            ],
            graph.bridges(),
        ),
        _ => (
            "surprising_connections",
            "surprising connection(s) ranked by surprise score",
            vec!["source", "target", "edge_kind", "surprise_score"],
            [
                "get_architecture_overview_tool -- community structure",
                "query_graph_tool callers_of -- trace the coupling",
                "get_bridge_nodes_tool -- find chokepoints",
            ],
            graph.surprises(),
        ),
    };
    let total = rows.len();
    rows.truncate(cap);
    if args["detail_level"] == "minimal" {
        rows = rows.iter().map(|r| subset(r, &fields)).collect();
    }
    Ok(
        json!({"status":"ok","summary":format!("{total} {desc}{}",shown(rows.len(),total)),(key):rows,"count":rows.len(),"total":total,"truncated":rows.len()<total,"next_tool_suggestions":hints}),
    )
}

impl Topology {
    fn gaps(&self, conn: &Connection) -> Result<Value> {
        let tested: HashSet<_> = self
            .links
            .iter()
            .filter(|e| e.kind == "TESTED_BY")
            .map(|e| e.src)
            .collect();
        let mut isolated = Vec::new();
        let mut untested = Vec::new();
        let mut sizes: HashMap<i64, usize> = HashMap::new();
        let mut files: HashMap<i64, HashSet<&str>> = HashMap::new();
        for n in &self.entities {
            let i = self.symbols.get(&n.qn);
            let degree = i
                .map(|i| self.incoming[*i] + self.outgoing[*i])
                .unwrap_or(0);
            if degree <= 1 || degree >= 5 && !n.test && !i.is_some_and(|s| tested.contains(s)) {
                let row = json!({"name":n.name,"qualified_name":n.qn,"kind":n.kind,"file":n.file,"degree":degree});
                if degree <= 1 {
                    isolated.push(row)
                } else {
                    untested.push(row);
                }
            }
            if let Some(cid) = n.community {
                *sizes.entry(cid).or_default() += 1;
                files.entry(cid).or_default().insert(&n.file);
            }
        }
        let mut thin = Vec::new();
        let mut single = Vec::new();
        let mut stmt = conn.prepare("SELECT id,name FROM communities")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let size = sizes.get(&id).copied().unwrap_or(0);
            if size < 3 {
                thin.push(json!({"community_id":id,"name":name,"size":size}));
            } else if let Some(group) = files.get(&id).filter(|s| s.len() == 1) {
                single.push(
                    json!({"community_id":id,"name":name,"size":size,"file":group.iter().next()}),
                );
            }
        }
        untested.sort_by_key(|n| std::cmp::Reverse(n["degree"].as_i64()));
        untested.truncate(20);
        isolated.truncate(50);
        Ok(
            json!({"isolated_nodes":isolated,"thin_communities":thin,"untested_hotspots":untested,"single_file_communities":single}),
        )
    }

    fn questions(&self, conn: &Connection) -> Result<Value> {
        let mut rows = Vec::new();
        let mut priorities = [0usize; 3];
        let mut append = |category: &str, question: String, target: Value, priority: usize| {
            priorities[priority] += 1;
            rows.push(json!({"category":category,"question":question,"target":target,"priority":(["high","medium","low"][priority])}));
        };
        for b in self.bridges().iter().take(3) {
            append(
                "bridge_node",
                format!(
                    "'{}' is a critical connector between multiple code regions. Is it adequately tested and documented?",
                    text(b, "name")
                ),
                b["qualified_name"].clone(),
                0,
            );
        }
        let tested: HashSet<_> = self
            .links
            .iter()
            .filter(|e| e.kind == "TESTED_BY")
            .map(|e| &self.keys[e.src])
            .collect();
        for h in self.hubs().iter().take(3) {
            if !tested
                .iter()
                .any(|n| n.as_str() == text(h, "qualified_name"))
            {
                append(
                    "hub_risk",
                    format!(
                        "Hub node '{}' has {} connections but no direct test coverage. Should it be tested?",
                        text(h, "name"),
                        h["total_degree"]
                    ),
                    h["qualified_name"].clone(),
                    0,
                );
            }
        }
        for s in self.surprises().iter().take(3) {
            if s["reasons"]
                .as_array()
                .is_some_and(|rs| rs.contains(&json!("cross-community")))
            {
                append(
                    "surprising_connection",
                    format!(
                        "'{}' (community {}) calls '{}' (community {}). Is this coupling intentional?",
                        text(s, "source"),
                        s["source_community"],
                        text(s, "target"),
                        s["target_community"]
                    ),
                    s["source_qualified"].clone(),
                    1,
                );
            }
        }
        let gaps = self.gaps(conn)?;
        for c in gaps["thin_communities"]
            .as_array()
            .into_iter()
            .flatten()
            .take(2)
        {
            append(
                "thin_community",
                format!(
                    "Community '{}' has only {} member(s). Should it be merged with a neighbor?",
                    text(c, "name"),
                    c["size"]
                ),
                json!(format!("community:{}", c["community_id"])),
                2,
            );
        }
        for h in gaps["untested_hotspots"]
            .as_array()
            .into_iter()
            .flatten()
            .take(2)
        {
            append(
                "untested_hotspot",
                format!(
                    "'{}' has {} connections but no test coverage. Is this a risk?",
                    text(h, "name"),
                    h["degree"]
                ),
                h["qualified_name"].clone(),
                1,
            );
        }
        Ok(
            json!({"questions":rows,"count":rows.len(),"by_priority":{"high":priorities[0],"medium":priorities[1],"low":priorities[2]},"next_tool_suggestions":["get_knowledge_gaps_tool -- structural weaknesses","detect_changes_tool -- risk-scored review","get_architecture_overview_tool -- community map"]}),
        )
    }

    fn knowledge(&self, conn: &Connection, root: &Path, args: &Value) -> Result<Value> {
        let cap = positive(args, "max_per_category", 15, 50)?;
        let mut gaps = self.gaps(conn)?;
        gaps["import_cycles"] = json!(cycles::find(conn, root)?);
        let mut totals = json!({});
        let mut total = 0;
        let mut cut = false;
        for (category, records) in gaps.as_object_mut().expect("gap categories") {
            let rows = records.as_array_mut().expect("gap rows");
            totals[category] = json!(rows.len());
            total += rows.len();
            cut |= rows.len() > cap;
            rows.truncate(cap);
            if args["detail_level"] == "minimal" {
                for row in rows {
                    *row = subset(
                        row,
                        &[
                            "name",
                            "qualified_name",
                            "community_id",
                            "size",
                            "degree",
                            "load_time",
                        ],
                    );
                }
            }
        }
        Ok(
            json!({"status":"ok","summary":totals,"gaps":gaps,"total_gaps":total,"truncated":cut,"next_tool_suggestions":["refactor_tool dead_code -- find unused symbols","get_hub_nodes_tool -- find high-impact nodes","get_suggested_questions_tool -- review prompts"]}),
        )
    }
}
