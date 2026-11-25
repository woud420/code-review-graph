use crate::cli::query::text;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub(super) fn aggregate(data: &Value, mode: &str) -> Value {
    let nodes = data["nodes"].as_array().unwrap();
    let edges = data["edges"].as_array().unwrap();
    let mut target = IndexMap::<&str, String>::new();
    let mut groups = IndexMap::<String, Value>::new();
    let mut details = Map::new();
    let by_file = mode == "file";
    if !by_file {
        for item in data["communities"].as_array().unwrap() {
            let id = item["id"].to_string();
            for member in item["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                target.insert(member, id.clone());
            }
            if item["size"].as_i64().unwrap_or(0) > 0 {
                groups.insert(id.clone(), json!({"qualified_name":format!("__community__{id}"),"name":item["name"],"kind":"Community","file_path":"",
                    "line_start":null,"line_end":null,"language":item["dominant_language"],"community_id":item["id"],"member_count":item["size"],
                    "description":item["description"],"id":item["id"]}));
            }
        }
    }
    let mut pending: HashMap<String, Vec<&Value>> = HashMap::new();
    let mut unknown = 0;
    for n in nodes {
        let name = text(n, "qualified_name");
        let file = text(n, "file_path");
        let group = if by_file {
            if file.is_empty() {
                continue;
            }
            let item=groups.entry(file.to_owned()).or_insert_with(||{
                let tail=file.replace('\\',"/");let parts:Vec<_>=tail.split('/').collect();
                let short=parts[parts.len().saturating_sub(2)..].join("/");
                json!({"qualified_name":file,"name":short,"kind":"File","file_path":file,"line_start":null,"line_end":null,"language":"","community_id":null,"symbol_count":0})
            });
            if n["kind"] != "File" {
                item["symbol_count"] = (item["symbol_count"].as_u64().unwrap() + 1).into();
            }
            if !text(n, "language").is_empty() {
                item["language"] = n["language"].clone();
            }
            if item["community_id"].is_null() {
                item["community_id"] = n["community_id"].clone();
            }
            file.to_owned()
        } else {
            let id = target
                .get(name)
                .cloned()
                .or_else(|| n["community_id"].as_i64().map(|n| n.to_string()))
                .unwrap_or_else(|| {
                    unknown += 1;
                    "-1".into()
                });
            pending.entry(id.clone()).or_default().push(n);
            id
        };
        target.insert(name, group);
    }
    if unknown > 0 {
        groups.insert("-1".into(),json!({"qualified_name":"__community__-1","name":"Uncategorized","kind":"Community","file_path":"","line_start":null,"line_end":null,"language":"","community_id":-1,"member_count":unknown,"description":"Nodes not assigned to any community","id":-1}));
    }
    if !by_file {
        for id in target.values() {
            if !details.contains_key(id) {
                details.insert(
                    id.clone(),
                    json!({"nodes":pending.remove(id).unwrap_or_default(),"edges":[]}),
                );
            }
        }
    }
    let mut pairs = IndexMap::<(&str, &str), usize>::new();
    for e in edges {
        let (Some(a), Some(b)) = (target.get(text(e, "source")), target.get(text(e, "target")))
        else {
            continue;
        };
        if a == b {
            if !by_file {
                details[a]["edges"].as_array_mut().unwrap().push(e.clone());
            }
            continue;
        }
        let pair = if !by_file && a.parse::<i64>().ok() > b.parse::<i64>().ok() {
            (b.as_str(), a.as_str())
        } else {
            (a.as_str(), b.as_str())
        };
        *pairs.entry(pair).or_default() += 1;
    }
    let links=pairs.into_iter().map(|((a,b),count)|if by_file{json!({"source":a,"target":b,"kind":"DEPENDS_ON","weight":count})}else{
        json!({"source":format!("__community__{a}"),"target":format!("__community__{b}"),"kind":"CROSS_COMMUNITY","weight":count})
    }).collect::<Vec<_>>();
    let mut result = json!({"nodes":groups.into_values().collect::<Vec<_>>(),"edges":links,"stats":data["stats"],"flows":data["flows"],"communities":data["communities"],"mode":mode});
    if !by_file {
        result["community_details"] = details.into();
    }
    result
}
