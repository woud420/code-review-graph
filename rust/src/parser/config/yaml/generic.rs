use super::super::json::child_path;
use super::*;
use std::collections::{BTreeSet, HashMap};

pub(super) fn locale(path: &Path) -> bool {
    super::super::json::file_class(path) == Some("json_locale_file")
}

pub(super) fn decoration<'t>(mut node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    while matches!(
        node.kind(),
        "document" | "block_node" | "flow_node" | "block_sequence_item"
    ) {
        let mut walk = node.walk();
        if let Some(d) = node.named_children(&mut walk).find(|n| n.kind() == kind) {
            return Some(d);
        }
        let mut walk = node.walk();
        let n = node
            .named_children(&mut walk)
            .find(|n| !matches!(n.kind(), "comment" | "tag" | "anchor"))?;
        node = n;
    }
    None
}

pub(super) fn scalar_type<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    if let Some(t) = decoration(node, "tag") {
        return source[t.byte_range()]
            .strip_prefix("!!")
            .filter(|s| {
                matches!(
                    *s,
                    "str" | "bool" | "int" | "float" | "null" | "timestamp" | "binary"
                )
            })
            .unwrap_or("scalar");
    }
    let n = peel(node);
    match n.kind() {
        "block_mapping" | "flow_mapping" => "mapping",
        "block_sequence" | "flow_sequence" => "sequence",
        "plain_scalar" => match n.named_child(0).map(|c| c.kind()) {
            Some("boolean_scalar") => "bool",
            Some("integer_scalar") => "int",
            Some("float_scalar") => "float",
            Some("null_scalar") => "null",
            Some("timestamp_scalar") => "timestamp",
            _ if matches!(
                source[n.byte_range()].to_ascii_lowercase().as_str(),
                "yes" | "no" | "on" | "off"
            ) =>
            {
                "bool"
            }
            _ => "str",
        },
        "document" | "block_sequence_item" => "null",
        _ => "str",
    }
}

pub(super) fn key(node: Node<'_>, src: &str) -> Option<(String, String)> {
    let n = peel(node);
    let raw = match n.kind() {
        "single_quote_scalar" => src[n.byte_range()]
            .strip_prefix('\'')?
            .strip_suffix('\'')?
            .replace("''", "'"),
        "double_quote_scalar" => serde_json::from_str::<String>(&src[n.byte_range()])
            .unwrap_or_else(|_| scalar(n, src).unwrap_or("").into()),
        "plain_scalar" => src[n.byte_range()].to_owned(),
        _ => return None,
    };
    let tag = if n.kind() == "plain_scalar" && raw == "<<" {
        "merge"
    } else {
        scalar_type(node, src)
    };
    Some((raw, format!("tag:yaml.org,2002:{tag}")))
}

struct Location {
    depth: usize,
    schema: String,
    example: String,
    identity: String,
}
impl Location {
    fn root() -> Self {
        Self {
            depth: 0,
            schema: "$".into(),
            example: "$".into(),
            identity: "$".into(),
        }
    }
    fn child(&self, name: &str, occurrence: usize) -> Self {
        let mut identity = child_path(&self.identity, name);
        if occurrence > 1 {
            identity.push_str(&format!("#{occurrence}"));
        }
        Self {
            depth: self.depth + 1,
            schema: child_path(&self.schema, name),
            example: child_path(&self.example, name),
            identity,
        }
    }
    fn item(&self, index: usize) -> Self {
        Self {
            depth: self.depth + 1,
            schema: format!("{}[*]", self.schema),
            example: format!("{}[{index}]", self.example),
            identity: format!("{}[{index}]", self.identity),
        }
    }
}

struct Shape<'t, 's, 'c> {
    doc: usize,
    name: &'s str,
    out: &'c mut Collector<'t>,
    anchors: HashMap<String, String>,
    by_id: HashMap<String, usize>,
}

fn bump(map: &mut Value, key: &str) {
    map[key] = json!(map[key].as_u64().unwrap_or(0) + 1);
}

fn append(map: &mut Value, key: &str, value: Value) {
    if !map[key].is_array() {
        map[key] = json!([]);
    }
    map[key].as_array_mut().unwrap().push(value);
}

impl Shape<'_, '_, '_> {
    fn visit(
        &mut self,
        n: Option<Node<'_>>,
        at: &Location,
        parent: &str,
        lines: (usize, usize),
        meta: Value,
        merge: bool,
    ) {
        let (first, last) = lines;
        if at.depth > 100
            || self.out.file.symbols.len() >= 10000
            || [&at.schema, &at.example, &at.identity]
                .iter()
                .any(|s| s.chars().count() > 512)
        {
            self.out.file.yaml.file["yaml_truncated"] = json!(true);
            return;
        }
        let text = self.out.text;
        let node = n.map(peel);
        if let Some(alias) = node.filter(|v| v.kind() == "alias") {
            let target = text[alias.byte_range()].trim_start_matches('*');
            let Some(target) = self.anchors.get(target).cloned() else {
                return;
            };
            let alias_line = alias.start_position().row + 1;
            let id = format!("yaml:{}:alias:{}@{alias_line}", self.doc, at.identity);
            let mut extra = json!({"document_index":self.doc,"path":at.example,"schema_path":at.schema,"source_file":self.name,"value_type":"alias","is_alias":true,"yaml_merge":merge});
            extra
                .as_object_mut()
                .unwrap()
                .extend(meta.as_object().unwrap().clone());
            self.out.declare(
                Symbol {
                    name: id.clone(),
                    kind: "YamlPath".into(),
                    start_line: first.min(alias_line),
                    end_line: alias_line.max(first),
                },
                Declaration {
                    name: at.example.clone(),
                    parent: None,
                    extra,
                },
                parent,
            );
            self.out.edge(
                "REFERENCES",
                &id,
                &target,
                true,
                alias_line,
                json!({"yaml_alias":true,"yaml_merge":merge}),
            );
            return;
        }
        let ty = n.map_or("null", |v| scalar_type(v, text));
        let tag = meta["key_tag"].as_str();
        let anchor = n.and_then(|v| decoration(v, "anchor"));
        let id = if let Some(a) = anchor {
            let id = format!(
                "yaml:{}:anchor:{}:{}",
                self.doc,
                text[..a.start_byte()].chars().count(),
                at.identity
            );
            self.anchors.insert(
                text[a.byte_range()].trim_start_matches('&').into(),
                id.clone(),
            );
            id
        } else {
            format!("yaml:{}:{}", self.doc, at.schema)
        };
        if let Some(&index) = self.by_id.get(&id) {
            let symbol = &mut self.out.file.symbols[index];
            symbol.start_line = symbol.start_line.min(first);
            symbol.end_line = symbol.end_line.max(last);
            self.out
                .edge("CONTAINS", parent, &id, true, first, json!({}));
            let d = self.out.file.yaml.declarations.get_mut(&id).unwrap();
            let extra = &mut d.extra;
            bump(extra, "occurrence_count");
            if extra["line_samples"]
                .as_array()
                .is_some_and(|s| s.len() < 10000)
            {
                append(extra, "line_samples", json!(first));
                append(extra, "range_samples", json!([first, last.max(first)]));
            } else {
                extra["lines_truncated"] = json!(true);
            }
            let mut types: BTreeSet<String> = extra["value_types"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            types.insert(ty.into());
            extra["value_type"] = json!(if types.len() == 1 { ty } else { "mixed" });
            extra["value_types"] = json!(types);
            if let Some(tag) = tag {
                let mut tags = BTreeSet::new();
                tags.extend(
                    extra["key_tags"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                );
                if let Some(first) = extra["key_tag"].as_str() {
                    tags.insert(first);
                }
                tags.insert(tag);
                extra["key_tags"] = json!(tags);
            }
        } else {
            let mut extra = json!({"document_index":self.doc,"path":at.schema,"schema_path":at.schema,"example_path":at.example,"source_file":self.name,"value_type":ty,"value_types":[ty],"occurrence_count":1,"line_samples":[first],"range_samples":[[first,last.max(first)]],"lines_truncated":false,"is_alias":false});
            extra
                .as_object_mut()
                .unwrap()
                .extend(meta.as_object().unwrap().clone());
            if anchor.is_some() {
                extra["anchor_definition"] = json!(true);
                if let Some(tag) = tag {
                    extra["key_tags"] = json!([tag]);
                }
            }
            self.by_id.insert(id.clone(), self.out.file.symbols.len());
            self.out.declare(
                Symbol {
                    name: id.clone(),
                    kind: "YamlPath".into(),
                    start_line: first,
                    end_line: last.max(first),
                },
                Declaration {
                    name: at.schema.clone(),
                    parent: None,
                    extra,
                },
                parent,
            );
        }
        if meta["duplicate_key"] == true {
            let e = &mut self.out.file.yaml.declarations.get_mut(&id).unwrap().extra;
            e["duplicate_key"] = json!(true);
            bump(e, "duplicate_key_occurrence_count");
            if meta["duplicate_group_start"] == true {
                bump(e, "duplicate_group_count");
                if e["duplicate_group_samples"]
                    .as_array()
                    .is_none_or(|s| s.len() < 100)
                {
                    append(
                        e,
                        "duplicate_group_samples",
                        meta["duplicate_group"].clone(),
                    );
                } else {
                    e["duplicate_groups_truncated"] = json!(true);
                }
            }
        }
        if let Some(node) = node {
            if ty == "mapping" {
                self.mapping(node, at, &id);
            }
            if ty == "sequence" {
                for (i, n) in items(node).into_iter().enumerate() {
                    self.visit(
                        Some(n),
                        &at.item(i),
                        &id,
                        (line(n), last_line(n, text)),
                        json!({"sequence_index_sample":i}),
                        merge,
                    );
                }
            }
        }
    }
    fn mapping(&mut self, node: Node<'_>, at: &Location, parent: &str) {
        let pairs = entries(node);
        let text = self.out.text;
        let mut groups = HashMap::<(String, String), Vec<usize>>::new();
        for (k, _) in &pairs {
            if let Some(signature) = key(*k, text) {
                groups.entry(signature).or_default().push(line(*k));
            }
        }
        let mut seen = HashMap::<(String, String), usize>::new();
        let mut spellings = HashMap::<String, usize>::new();
        for (k, v) in pairs {
            let Some(sig) = key(k, text) else {
                bump(&mut self.out.file.yaml.file, "yaml_unsupported_key_count");
                continue;
            };
            let (raw, tag) = &sig;
            let lines = &groups[&sig];
            let occurrence = seen.entry(sig.clone()).or_default();
            *occurrence += 1;
            let spelling = spellings.entry(raw.clone()).or_default();
            *spelling += 1;
            let position = at.child(raw, *spelling);
            let mut meta = json!({"raw_key":raw,"key_tag":tag,"duplicate_key":lines.len()>1,"key_occurrence":*occurrence});
            if lines.len() > 1 {
                let group = json!({"example_path":position.example.chars().take(512).collect::<String>(),"occurrence_count":lines.len(),"line_samples":lines.iter().take(20).collect::<Vec<_>>(),"lines_truncated":lines.len()>20});
                meta["duplicate_group"] = group.clone();
                meta["duplicate_group_start"] = json!(*occurrence == 1);
                if *occurrence == 1 {
                    let file = &mut self.out.file.yaml.file;
                    bump(file, "yaml_duplicate_key_count");
                    if file["yaml_duplicate_keys"].as_array().unwrap().len() < 100 {
                        let mut summary = group;
                        summary["path"] = summary["example_path"].clone();
                        summary["document_index"] = json!(self.doc);
                        append(file, "yaml_duplicate_keys", summary);
                    }
                }
            }
            let merge = tag == "tag:yaml.org,2002:merge";
            if merge {
                meta["merge_key"] = json!(true);
            }
            let first = line(k);
            self.visit(
                v,
                &position,
                parent,
                (first, v.map_or(first, |n| last_line(n, text)).max(first)),
                meta,
                merge,
            );
        }
    }
}

pub(super) fn parse(path: &Path, docs: &[Node<'_>], out: &mut Collector<'_>) {
    out.file.yaml.language = "yaml".into();
    out.file.yaml.file = json!({"config_format":path.extension().and_then(|s|s.to_str()),"yaml_document_count":docs.len(),"yaml_duplicate_key_count":0,"yaml_duplicate_keys":[],"yaml_unsupported_key_count":0,"yaml_truncated":false,"values_indexed":false});
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    for (doc, &node) in docs.iter().enumerate() {
        let range = (line(node), last_line(node, out.text));
        let mut shape = Shape {
            doc,
            name,
            out,
            anchors: HashMap::new(),
            by_id: HashMap::new(),
        };
        shape.visit(
            Some(node),
            &Location::root(),
            "<module>",
            range,
            json!({"document_root":true}),
            false,
        );
    }
}
