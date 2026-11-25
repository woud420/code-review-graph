use super::*;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

static STORY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.stories\.[jt]sx?$").unwrap());
static ANNOTATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Z][A-Za-z0-9_]*").unwrap());
const FRAMEWORK_BASES: [&str; 11] = [
    "Base",
    "DeclarativeBase",
    "Model",
    "BaseModel",
    "BaseSettings",
    "db.Model",
    "TableBase",
    "Stack",
    "NestedStack",
    "Construct",
    "Resource",
];

struct Edge {
    kind: String,
    src: String,
    dst: String,
    file: String,
    lang: String,
    extra: Value,
}
struct Evidence {
    incoming: HashMap<String, HashSet<String>>,
    bare: HashMap<(String, String), Vec<(String, String)>>,
    suffix: HashMap<String, HashSet<String>>,
    tested: HashMap<String, HashSet<String>>,
    members: HashSet<String>,
    imports: HashMap<String, Vec<String>>,
    bases: HashMap<String, Vec<String>>,
    counts: HashMap<String, usize>,
    types: HashSet<String>,
}
fn family(lang: &str) -> &str {
    match lang {
        "javascript" | "typescript" | "tsx" | "jsx" => "javascript",
        s => s,
    }
}
impl Evidence {
    fn read(conn: &Connection, classes: &HashSet<String>) -> Result<Self> {
        let mut e = Self {
            incoming: HashMap::new(),
            bare: HashMap::new(),
            suffix: HashMap::new(),
            tested: HashMap::new(),
            members: HashSet::new(),
            imports: HashMap::new(),
            bases: HashMap::new(),
            counts: HashMap::new(),
            types: HashSet::new(),
        };
        let mut names=conn.prepare("SELECT name,kind,is_test,coalesce(params,''),coalesce(return_type,'') FROM nodes WHERE kind IN('Function','Test','Class')")?;
        let mut rows = names.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let kind: String = row.get(1)?;
            if kind != "Test" && !row.get::<_, bool>(2)? {
                *e.counts.entry(name).or_default() += 1;
            }
            if kind != "Class" {
                for field in [row.get::<_, String>(3)?, row.get::<_, String>(4)?] {
                    e.types
                        .extend(ANNOTATION.find_iter(&field).map(|m| m.as_str().to_owned()));
                }
            }
        }
        let class_width = classes.iter().map(String::len).max().unwrap_or(0);
        let mut edges=conn.prepare("SELECT e.kind,e.source_qualified,e.target_qualified,e.file_path,coalesce(n.language,''),e.extra FROM edges e LEFT JOIN nodes n ON n.qualified_name=e.source_qualified WHERE e.kind IN('CALLS','TESTED_BY','IMPORTS_FROM','INHERITS','REFERENCES')")?;
        let mut rows = edges.query([])?;
        while let Some(row) = rows.next()? {
            let edge = Edge {
                kind: row.get(0)?,
                src: row.get(1)?,
                dst: row.get(2)?,
                file: row.get(3)?,
                lang: row.get(4)?,
                extra: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
            };
            e.record(edge, classes, class_width);
        }
        Ok(e)
    }
    fn record(&mut self, edge: Edge, classes: &HashSet<String>, class_width: usize) {
        if edge.kind == "INHERITS" {
            self.bases
                .entry(edge.src.clone())
                .or_default()
                .push(edge.dst.clone());
        }
        if edge.kind == "IMPORTS_FROM" {
            self.imports
                .entry(edge.file.clone())
                .or_default()
                .push(edge.dst.clone());
        }
        if STORY.is_match(&edge.file) {
            return;
        }
        self.incoming
            .entry(edge.dst.clone())
            .or_default()
            .insert(edge.kind.clone());
        if matches!(edge.kind.as_str(), "CALLS" | "INHERITS") {
            self.bare
                .entry((edge.kind.clone(), edge.dst.clone()))
                .or_default()
                .push((edge.lang, edge.file.clone()));
        }
        if edge.kind == "TESTED_BY" {
            self.tested.entry(edge.src).or_default().insert(edge.file);
            return;
        }
        if edge.kind != "CALLS" {
            return;
        }
        let mut written = vec![edge.dst.clone()];
        if let Some(endpoint) = edge.extra["claimed_endpoint"]
            .as_str()
            .or_else(|| edge.extra["inherited_endpoint"].as_str())
        {
            written.push(endpoint.to_owned())
        }
        if let Some(through) = edge.extra["inherited_through"].as_str() {
            written.push(format!(
                "{through}.{}",
                edge.dst.rsplit('.').next().unwrap_or("")
            ));
        }
        for name in written {
            if let Some((_, symbol)) = name.rsplit_once("::") {
                self.suffix
                    .entry(symbol.to_ascii_lowercase())
                    .or_default()
                    .insert(edge.file.clone());
            }
            let folded = name.to_ascii_lowercase();
            for (end, _) in folded.match_indices('.') {
                for (i, _) in folded[..end]
                    .char_indices()
                    .rev()
                    .take_while(|&(i, _)| end - i <= class_width)
                {
                    if classes.contains(&folded[i..end]) {
                        self.members.insert(folded[i..end].into());
                    }
                }
            }
        }
    }
    fn plausible(&self, edge_file: &str, file: &str, name: &str, root: &Path) -> bool {
        if edge_file == file || self.counts.get(name) == Some(&1) {
            return true;
        }
        let imports = self.imports.get(edge_file).into_iter().flatten();
        let hits = |raw: &str| {
            let target = raw.split("::").next().unwrap_or(raw);
            let parent = target.strip_suffix("/__init__.py").unwrap_or(target);
            target.starts_with(file) || file.starts_with(&format!("{parent}/"))
        };
        for imp in imports {
            if hits(imp)
                || self
                    .imports
                    .get(imp)
                    .into_iter()
                    .flatten()
                    .any(|modpath| hits(modpath))
            {
                return true;
            }
            if !imp.starts_with('/') {
                let path = Path::new(file)
                    .strip_prefix(root)
                    .unwrap_or(Path::new(file));
                if path
                    .parent()
                    .into_iter()
                    .flat_map(Path::iter)
                    .filter_map(|s| s.to_str())
                    .any(|s| {
                        s.len() >= 4
                            && !matches!(s, "home" | "src" | "lib" | "app")
                            && imp.contains(s)
                    })
                {
                    return true;
                }
            }
        }
        false
    }
    fn incoming(&self, qn: &str, kind: &str) -> bool {
        self.incoming.get(qn).is_some_and(|set| set.contains(kind))
    }
    fn live(&self, n: &Value, root: &Path) -> bool {
        let qn = text(n, "qualified_name");
        let name = text(n, "name");
        let parent = text(n, "parent_name");
        let file = text(n, "file_path");
        let lang = text(n, "language");
        let alt = format!("{parent}::{name}");
        if ["CALLS", "INHERITS", "IMPORTS_FROM", "REFERENCES"]
            .iter()
            .any(|k| self.incoming(qn, k) || !parent.is_empty() && self.incoming(&alt, k))
        {
            return true;
        }
        if self
            .bare
            .get(&("CALLS".into(), name.into()))
            .into_iter()
            .flatten()
            .any(|(l, p)| {
                (lang.is_empty() || family(l) == family(lang))
                    && self.plausible(p, file, name, root)
            })
        {
            return true;
        }
        if self
            .suffix
            .get(&name.to_ascii_lowercase())
            .into_iter()
            .flatten()
            .any(|p| self.plausible(p, file, name, root))
        {
            return true;
        }
        if self.tested.contains_key(qn)
            || (!parent.is_empty() && self.tested.contains_key(&alt))
            || self
                .tested
                .get(name)
                .into_iter()
                .flatten()
                .any(|p| self.plausible(p, file, name, root))
        {
            return true;
        }
        n["kind"] == "Class"
            && (self.members.contains(&name.to_ascii_lowercase())
                || self.types.contains(name)
                || self
                    .bare
                    .get(&("INHERITS".into(), name.into()))
                    .into_iter()
                    .flatten()
                    .any(|(l, _)| lang.is_empty() || family(l) == family(lang)))
    }
    fn exempt(&self, conn: &Connection, n: &Value, extra: &Value, root: &Path) -> Result<bool> {
        let name = text(n, "name");
        let file = text(n, "file_path");
        let parent = text(n, "parent_name");
        let qn = text(n, "qualified_name");
        let kind = text(n, "kind");
        let path = Path::new(file)
            .strip_prefix(root)
            .unwrap_or(Path::new(file));
        if n["is_test"] == true
            || code_review_graph_rs::model::test_file(file, root.to_str())
            || path.iter().any(|p| p == "test-utils" || p == "test_utils")
            || file.ends_with(".d.ts")
            || name.starts_with("__") && name.ends_with("__")
            || name == "constructor" && !parent.is_empty()
            || extra.get("verilog_kind").is_some()
            || extra["abstract"] == true
            || matches!(
                text(extra, "xslt_kind"),
                "match_template" | "named_template"
            ) && !text(extra, "match").is_empty()
        {
            return Ok(true);
        }
        if code_review_graph_rs::canonical::postprocess::conventional_entry(
            name,
            text(n, "language"),
            extra,
        ) {
            return Ok(true);
        }
        if ["Stack", "Construct", "Pipeline", "Resources", "Layer"]
            .iter()
            .any(|s| {
                kind == "Class" && name.ends_with(s) || kind == "Function" && parent.ends_with(s)
            })
        {
            return Ok(true);
        }
        let decorators: Vec<_> = extra["decorators"]
            .as_str()
            .into_iter()
            .chain(
                extra["decorators"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            )
            .collect();
        if decorators.iter().any(|d| {
            d.trim_start_matches('@') == "Override"
                || kind == "Class" && d.contains("dataclass")
                || kind == "Function"
                    && (matches!(
                        *d,
                        "property" | "abstractmethod" | "classmethod" | "staticmethod"
                    ) || d.ends_with(".abstractmethod")
                        || d.starts_with("HostListener"))
        }) {
            return Ok(true);
        }
        let class = if kind == "Class" {
            qn
        } else if !parent.is_empty() {
            qn.rsplit_once('.').map_or("", |(c, _)| c)
        } else {
            ""
        };
        for base in self.bases.get(class).into_iter().flatten() {
            let leaf = base.rsplit("::").next().unwrap_or(base);
            if FRAMEWORK_BASES.contains(&leaf) {
                return Ok(true);
            }
            if kind == "Function" {
                let mut stmt=conn.prepare("SELECT qualified_name,extra FROM nodes WHERE qualified_name=?1 OR qualified_name=?2 OR (parent_name=?3 AND name=?4)")?;
                let mut rows = stmt.query(params![
                    format!("{base}.{name}"),
                    format!("{file}::{base}.{name}"),
                    leaf,
                    name
                ])?;
                while let Some(r) = rows.next()? {
                    let target: String = r.get(0)?;
                    let meta: Value =
                        serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default();
                    if self.incoming(&target, "CALLS")
                        || meta["decorators"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|s| s.as_str().is_some_and(|s| s.contains("abstractmethod")))
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

pub(in crate::cli) fn find(
    conn: &Connection,
    root: &Path,
    kind: &str,
    pattern: &str,
) -> Result<Vec<Value>> {
    let mut stmt=conn.prepare("SELECT * FROM nodes WHERE kind IN('Class','Function') AND (?1='' OR kind=?1) AND (?2='' OR file_path LIKE '%'||?2||'%')")?;
    let nodes = stmt
        .query_map(params![kind, pattern], |r| {
            Ok((
                node(r)?,
                serde_json::from_str(&r.get::<_, String>("extra")?).unwrap_or_default(),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let classes = nodes
        .iter()
        .filter(|(n, _)| n["kind"] == "Class")
        .map(|(n, _)| text(n, "name").to_ascii_lowercase())
        .collect();
    let edges = Evidence::read(conn, &classes)?;
    let mut dead = Vec::new();
    for (n, extra) in nodes {
        if edges.exempt(conn, &n, &extra, root)? || edges.live(&n, root) {
            continue;
        }
        let file = Path::new(text(&n, "file_path"));
        dead.push(json!({"name":n["name"],"qualified_name":n["qualified_name"],"kind":n["kind"],"file":file,"file_path":file,"relative_path":file.strip_prefix(root).unwrap_or(file),"line":n["line_start"],"language":n["language"]}));
    }
    Ok(dead)
}
