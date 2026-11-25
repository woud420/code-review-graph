use super::{Source, import_target, qualified};
use crate::model::{Import, ParsedFile, PythonReceiver, PythonType};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

pub(super) struct ReturnIndex(HashMap<String, HashMap<String, String>>);

impl ReturnIndex {
    pub fn read(staging: &rusqlite::Connection) -> rusqlite::Result<Self> {
        let mut returns = HashMap::<String, HashMap<String, String>>::new();
        let mut stmt = staging.prepare(
            "SELECT f.path,d.key,json_extract(d.value,'$.returns')
            FROM files f,json_each(f.facts,'$.python.definitions') d
            WHERE json_extract(d.value,'$.returns') IS NOT NULL AND json_extract(d.value,'$.module_level')=1",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            returns
                .entry(row.get(0)?)
                .or_default()
                .insert(row.get(1)?, row.get(2)?);
        }
        Ok(Self(returns))
    }
}

pub(super) struct CallEvidence<'a> {
    root: &'a Path,
    file: &'a str,
    facts: &'a ParsedFile,
    source: &'a Source,
    paths: &'a BTreeSet<String>,
    returns: &'a ReturnIndex,
    sites: HashMap<(&'a str, usize, &'a str), &'a PythonReceiver>,
    expressions: HashMap<(&'a str, usize, &'a str), Vec<&'a PythonReceiver>>,
    by_method: HashMap<(&'a str, usize, &'a str), &'a PythonReceiver>,
    properties: HashSet<(&'a str, usize, &'a str)>,
    imports: BTreeMap<&'a str, &'a Import>,
}

impl<'a> CallEvidence<'a> {
    pub fn new(
        root: &'a Path,
        file: &'a str,
        facts: &'a ParsedFile,
        source: &'a Source,
        paths: &'a BTreeSet<String>,
        returns: &'a ReturnIndex,
    ) -> Self {
        let mut sites = HashMap::new();
        let mut by_method = HashMap::new();
        let mut expressions: HashMap<_, Vec<_>> = HashMap::new();
        for site in &facts.python.receivers {
            sites.insert((site.caller.as_str(), site.line, site.raw.as_str()), site);
            let method = site.raw.rsplit('.').next().unwrap();
            let key = (site.caller.as_str(), site.line, method);
            by_method.insert(key, site);
            if site.name.is_none() {
                expressions.entry(key).or_default().push(site);
            }
        }
        let properties = facts
            .python
            .members
            .iter()
            .filter(|m| m.property)
            .map(|m| (m.caller.as_str(), m.line, m.raw.as_str()))
            .collect();
        let mut imports = BTreeMap::new();
        for binding in facts
            .imports
            .iter()
            .chain(facts.python.scoped_imports.iter().map(|s| &s.binding))
        {
            imports.insert(binding.local.as_str(), binding);
            if binding.imported == "*"
                && binding.local == binding.module
                && let Some((head, _)) = binding.local.split_once('.')
            {
                imports.insert(head, binding);
            }
        }
        Self {
            root,
            file,
            facts,
            source,
            paths,
            returns,
            sites,
            by_method,
            expressions,
            properties,
            imports,
        }
    }

    fn module(&self, name: &str) -> Option<String> {
        let binding = self.imports.get(name)?;
        let module = if binding.imported == "*" {
            if binding.local == binding.module && binding.local != name {
                name.to_owned()
            } else {
                binding.module.clone()
            }
        } else {
            let separator = if binding.module.ends_with('.') {
                ""
            } else {
                "."
            };
            format!("{}{}{}", binding.module, separator, binding.imported)
        };
        import_target(self.file, &module, "python", self.paths)
            .map(|p| self.root.join(p).to_string_lossy().into_owned())
    }

    fn origin(&self, name: &str) -> Option<String> {
        if self
            .facts
            .symbols
            .iter()
            .any(|s| s.name == name && s.kind == "Class")
        {
            return Some(self.root.join(self.file).to_string_lossy().into_owned());
        }
        let binding = self.imports.get(name)?;
        import_target(self.file, &binding.module, "python", self.paths)
            .map(|p| self.root.join(p).to_string_lossy().into_owned())
    }

    fn return_type(&self, callee: &str) -> Option<&str> {
        if let Some(ty) = self.returns.0.get(self.file).and_then(|m| m.get(callee)) {
            return Some(ty);
        }
        let (file, name) = if let Some((module, name)) = callee.split_once('.') {
            let file = self.module(module)?;
            let relative = Path::new(&file).strip_prefix(self.root).ok()?.to_str()?;
            (relative.to_owned(), name)
        } else {
            let binding = self.imports.get(callee)?;
            (
                import_target(self.file, &binding.module, "python", self.paths)?,
                callee,
            )
        };
        self.returns.0.get(&file)?.get(name).map(String::as_str)
    }

    fn constructor(&self, callee: &str) -> Option<Value> {
        let name = callee.rsplit('.').next()?;
        let is_class = self
            .facts
            .symbols
            .iter()
            .any(|s| s.name == callee && s.kind == "Class");
        (self.imports.contains_key(callee)
            || is_class
            || (callee.contains('.')
                && name.chars().next()?.is_uppercase()
                && self.imports.contains_key(callee.split('.').next()?)))
        .then(|| class(name))
    }
    fn binding(&self, site: &PythonReceiver, bound: Option<(&str, &str)>) -> Value {
        let binding = match &site.binding {
            Some(PythonType::Named(ty)) => annotation(ty, false),
            Some(PythonType::Parameter(ty)) => annotation(ty, true),
            Some(PythonType::Builtin) if site.name.is_some() => {
                json!({"receiver_binding":"builtin"})
            }
            Some(PythonType::Returned { callee, element }) => {
                if site.name.is_none() {
                    self.constructor(callee).unwrap_or(Value::Null)
                } else if let Some(ret) = self.return_type(callee) {
                    crate::graph::annotation_name(ret, *element)
                        .map(|ty| annotation(ty, false))
                        .unwrap_or(Value::Null)
                } else {
                    element
                        .is_none()
                        .then(|| self.constructor(callee))
                        .flatten()
                        .or_else(|| {
                            bound.and_then(|(_, sym)| {
                                sym.rsplit_once('.').map(|(name, _)| class(name))
                            })
                        })
                        .unwrap_or(Value::Null)
                }
            }
            _ => Value::Null,
        };
        if !binding.is_null() {
            return binding;
        }
        if let Some(name) = &site.name {
            if site.binding.is_none()
                && site.raw.starts_with("self.")
                && let Some(parent) = self
                    .facts
                    .python
                    .definitions
                    .get(&site.caller)
                    .and_then(|d| d.parent.as_deref())
                && let Some(field) = self.facts.python.fields.get(&format!("{parent}.{name}"))
            {
                let mut candidates = Vec::new();
                for ty in field {
                    let value = match ty {
                        PythonType::Named(ty) | PythonType::Parameter(ty) => {
                            Some(annotation(ty, false))
                        }
                        PythonType::Builtin => Some(json!({"receiver_binding":"builtin"})),
                        PythonType::Returned { callee, element } => self
                            .return_type(callee)
                            .and_then(|ret| crate::graph::annotation_name(ret, *element))
                            .map(|ret| annotation(ret, false))
                            .or_else(|| self.constructor(callee)),
                    };
                    if let Some(value) = value
                        && !candidates.contains(&value)
                    {
                        candidates.push(value);
                    }
                }
                if candidates.len() == 1 {
                    return candidates.pop().unwrap();
                }
            }
            if let Some(path) = self.module(name) {
                return json!({"receiver_binding":"module","receiver_module":bound.map(|(f,_)|self.root.join(f).to_string_lossy().into_owned()).unwrap_or(path)});
            }
            if self.imports.contains_key(name.as_str())
                || self.facts.python.definitions.contains_key(name)
            {
                return class(name);
            }
        }
        json!({"receiver_binding":"unknown"})
    }

    pub fn describe(
        &self,
        caller: &str,
        raw: &str,
        line: usize,
        bound: Option<(&str, &str)>,
    ) -> Value {
        let site_key = (caller, line, raw);
        let method = raw.rsplit('.').next().unwrap_or(raw);
        if let Some(site) = self.facts.syntax.site(caller, raw, line) {
            let mut extra = site.extra.clone();
            if self.source.language == "scala" && bound.is_none() && site.target.is_none() {
                let prefix = format!("{raw}:S");
                let path = self.root.join(self.file).to_string_lossy().into_owned();
                let identities: Vec<_> = self
                    .facts
                    .symbols
                    .iter()
                    .filter(|s| s.name == raw || s.name.starts_with(&prefix))
                    .map(|s| qualified(&path, &s.name))
                    .collect();
                if identities.len() > 1 {
                    return json!({"ambiguous_targets":identities.iter().take(20).collect::<Vec<_>>(),"ambiguous_target_count":identities.len(),"ambiguous_targets_truncated":identities.len()>20,"collision_endpoint":qualified(&path,raw),"confidence":0.4,"confidence_tier":"AMBIGUOUS"});
                }
            }

            if self.source.language == "php"
                && site.target.is_none()
                && let Some((bound_file, _)) = bound
            {
                let imported = raw
                    .split_once("::")
                    .and_then(|(scope, _)| self.facts.php_imports.get(scope))
                    .and_then(|fqn| {
                        crate::parser::php::imports::local_file(self.root, self.file, fqn, |p| {
                            self.paths.contains(p)
                        })
                    });
                if imported.is_some_and(|path| path == bound_file) {
                    extra["scoped_resolution"] = json!("import");
                } else {
                    extra["scoped_resolved"] = json!(true);
                    extra["scoped_via"] = json!("single_match");
                }
            }
            if self.source.language == "csharp" && bound.is_some() {
                extra["scoped_resolved"] = json!(true);
                extra["scoped_via"] = json!("csharp_namespace");
                if let Some(obj) = extra.as_object_mut() {
                    obj.remove("unresolved_targets");
                }
            }
            return extra;
        }
        if !self.facts.cfamily.language.is_empty() {
            return self.facts.cfamily.evidence(
                caller,
                raw,
                line,
                &self.root.join(self.file).to_string_lossy(),
            );
        }
        if matches!(
            self.source.language,
            "javascript" | "typescript" | "tsx" | "vue" | "html"
        ) {
            let extra = self
                .facts
                .typescript
                .members
                .iter()
                .rev()
                .find(|(source, call, l, _)| source == caller && call == raw && *l == line);
            let Some((_, _, _, meta)) = extra else {
                return Value::Null;
            };
            let mut meta = meta.clone();
            if meta["receiver"] == "this"
                && let Some((_, target)) = bound
                && target != raw
            {
                meta["inherited_endpoint"] =
                    json!(qualified(&self.root.join(self.file).to_string_lossy(), raw));
            }
            return meta;
        }
        if self.source.language == "go" {
            return self.facts.go.evidence(caller, raw, line);
        }
        if self.source.language == "bash" {
            return if bound.is_some_and(|(file, _)| file != self.file) {
                json!({"bare_call_target":raw})
            } else {
                Value::Null
            };
        }
        if self.source.language != "python" {
            if let Some((receiver, _)) = raw.rsplit_once('.') {
                return json!({"receiver":receiver});
            }
            return Value::Null;
        }
        if bound.is_some() && self.properties.contains(&site_key) {
            return json!({"python_property_read":true});
        }
        let Some(site) = self.sites.get(&site_key).or_else(|| {
            raw.contains('.')
                .then(|| self.by_method.get(&(caller, line, method)))
                .flatten()
        }) else {
            if let Some((file, _)) = bound
                && file != self.file
                && !self.source.extra_edges.direct_name(raw)
            {
                return json!({"bare_call_target":raw});
            }
            if bound.is_none()
                && let Some(base) = self
                    .facts
                    .python
                    .ambiguities
                    .iter()
                    .find(|a| a.rsplit('.').next() == Some(method))
            {
                let variants = self
                    .facts
                    .symbols
                    .iter()
                    .filter(|s| s.name.split(":S").next() == Some(base))
                    .map(|s| qualified(&self.root.join(self.file).to_string_lossy(), &s.name))
                    .collect::<BTreeSet<_>>();
                let count = variants.len();
                let targets = variants.into_iter().take(20).collect::<Vec<_>>();
                return json!({"ambiguous_targets":targets,"ambiguous_target_count":count,"ambiguous_targets_truncated":count>20,
                    "collision_endpoint":qualified(&self.root.join(self.file).to_string_lossy(),base),"confidence":0.4,"confidence_tier":"AMBIGUOUS"});
            }
            return Value::Null;
        };
        let name = site.name.as_deref();
        if matches!(name, Some("cls" | "self"))
            && let Some(parent) = self
                .facts
                .python
                .definitions
                .get(caller)
                .and_then(|d| d.parent.as_deref())
        {
            let mut extra = json!({"receiver":name});
            if bound.is_none() {
                extra["claimed_endpoint"] = json!(format!(
                    "{}::{parent}.{method}",
                    self.root.join(self.file).display()
                ));
            }
            return extra;
        }
        let direct = site
            .declared
            .as_deref()
            .and_then(base_type)
            .map(|t| (t, "typed_receiver"))
            .or_else(|| match &site.binding {
                Some(PythonType::Parameter(ty)) => base_type(ty).map(|t| (t, "typed_receiver")),
                _ => name
                    .filter(|n| {
                        n.chars().next().is_some_and(char::is_uppercase)
                            && self.source.extra_edges.direct_name(n)
                    })
                    .map(|n| (n, "class_receiver")),
            });
        if let Some((ty, kind)) = direct
            && let Some(origin) = self.origin(ty)
            && self.source.extra_edges.direct_name(ty)
        {
            let mut extra = json!({"receiver":name,"receiver_resolution":kind,"receiver_type":ty});
            if bound.is_none() {
                extra["claimed_endpoint"] = json!(format!("{origin}::{ty}.{method}"));
            }
            return extra;
        }
        let mut extra = self.binding(site, bound);
        if name.is_none()
            && let Some(sites) = self.expressions.get(&(caller, line, method))
            && sites.len() > 1
        {
            let mut known = sites
                .iter()
                .map(|s| self.binding(s, None))
                .filter(|v| v["receiver_binding"] != "unknown");
            if let Some(first) = known.next() {
                extra = if known.all(|x| x == first) {
                    first
                } else {
                    json!({"receiver_binding":"unknown"})
                };
            }
        }
        if let Some(name) = name {
            extra["receiver"] = json!(name);
        } else {
            extra["receiver_form"] = json!("expression");
        }
        if bound.is_some() {
            extra["bare_call_target"] = json!(method);
        }
        extra
    }
}

fn class(name: &str) -> Value {
    json!({"receiver_binding":"class","receiver_class":name})
}

fn annotation(ty: &str, parameter: bool) -> Value {
    base_type(ty)
        .map(class)
        .unwrap_or_else(|| json!({"receiver_binding":if parameter {"unknown"}else{"builtin"}}))
}

fn base_type(mut ty: &str) -> Option<&str> {
    for _ in 0..4 {
        ty = ty.trim().trim_start_matches(['\'', '"']);
        let raw = ty
            .split(|c: char| c != '.' && c != '_' && !c.is_alphanumeric())
            .next()?;
        let name = raw.rsplit('.').next()?;
        if matches!(name, "Optional" | "Annotated" | "Type") {
            ty = ty.split_once('[')?.1;
            continue;
        }
        return (!matches!(
            name,
            "" | "Array"
                | "Collection"
                | "Dict"
                | "Iterable"
                | "List"
                | "Map"
                | "Mapping"
                | "MutableList"
                | "MutableMap"
                | "MutableMapping"
                | "ReadonlyArray"
                | "Sequence"
                | "Set"
                | "Tuple"
                | "Union"
                | "dict"
                | "frozenset"
                | "list"
                | "set"
                | "tuple"
                | "None"
                | "bool"
                | "boolean"
                | "float"
                | "int"
                | "number"
                | "str"
                | "string"
                | "unknown"
                | "void"
        ))
        .then_some(name);
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::canonical::{index, postprocess::Level};
    use anyhow::Result;
    use rusqlite::Connection;
    use serde_json::{Value, json};
    use std::fs;

    #[test]
    fn call_site_keeps_declared_inferred_and_conflicting_receiver_evidence() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        fs::write(
            root.join("library.py"),
            "class Worker:\n    def start(self): pass\n\ndef make() -> Worker:\n    return Worker()\n",
        )?;
        fs::write(
            root.join("caller.py"),
            "from pathlib import PurePosixPath, PureWindowsPath\nfrom library import Worker, make\n\ndef entry(w: Worker):\n    w.start()\n    another = make()\n    another.start()\n    return PurePosixPath().is_absolute() or PureWindowsPath().is_absolute()\n",
        )?;
        let path = root.join(".code-review-graph/graph.db");
        index(&root, &path, true, Level::Full, false)?;
        let db = Connection::open(path)?;
        let site = |line: i64, method: &str| -> Result<(Value, String)> {
            let (raw,tier):(String,String)=db.query_row("SELECT extra,confidence_tier FROM edges WHERE kind='CALLS' AND file_path=? AND line=?
                AND (target_qualified=? OR target_qualified LIKE '%'||'.'||?)",
                rusqlite::params![root.join("caller.py").to_str(),line,method,method],|r|Ok((r.get(0)?,r.get(1)?)))?;
            Ok((serde_json::from_str(&raw)?, tier))
        };
        assert_eq!(
            site(5, "start")?,
            (
                json!({"receiver":"w","receiver_type":"Worker","receiver_resolution":"typed_receiver"}),
                "EXTRACTED".into()
            )
        );
        assert_eq!(
            site(7, "start")?,
            (
                json!({"receiver":"another","receiver_class":"Worker","receiver_binding":"class","bare_call_target":"start"}),
                "INFERRED".into()
            )
        );
        assert_eq!(
            site(8, "is_absolute")?,
            (
                json!({"receiver_binding":"unknown","receiver_form":"expression"}),
                "UNRESOLVED".into()
            )
        );
        Ok(())
    }
}
