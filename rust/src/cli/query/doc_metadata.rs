use super::*;

pub(super) fn add(data: &mut Value, raw: Option<String>) {
    let extra = raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let kind = text(data, "kind");
    if matches!(kind, "YamlPath" | "JsonPath") {
        let key = if kind == "JsonPath" { "json" } else { "yaml" };
        let mut meta = json!({});
        for key in ["schema_path", "example_path", "value_type"] {
            if let Some(v) = extra[key].as_str() {
                meta[key] = json!(clean(v));
            }
        }
        for key in [
            "document_index",
            "occurrence_count",
            "duplicate_key_occurrence_count",
            "duplicate_group_count",
        ] {
            if extra[key].is_i64() || extra[key].is_u64() {
                meta[key] = extra[key].clone();
            }
        }
        for key in [
            "duplicate_key",
            "duplicate_groups_truncated",
            "is_alias",
            "anchor_definition",
            "yaml_merge",
            "lines_truncated",
        ] {
            if extra[key].is_boolean() {
                meta[key] = extra[key].clone();
            }
        }
        if let Some(list) = extra["value_types"].as_array() {
            meta["value_types"] = json!(
                list.iter()
                    .take(20)
                    .filter_map(Value::as_str)
                    .map(clean)
                    .collect::<Vec<_>>()
            );
        }
        if let Some(list) = extra["line_samples"].as_array() {
            let lines: Vec<_> = list
                .iter()
                .take(20)
                .filter(|v| v.is_i64() || v.is_u64())
                .collect();
            meta["line_samples"] = json!(lines);
            let count = extra
                .get("occurrence_count")
                .map(Value::as_i64)
                .unwrap_or(Some(list.len() as i64));
            if let Some(count) = count {
                meta["line_samples_omitted"] = json!((count - lines.len() as i64).max(0));
            }
        }
        data[key] = meta;
    } else if kind == "File" && data["language"] == "json" {
        let mut meta = json!({"truncated":extra["json_truncated"]==true,"values_indexed":false});
        for k in ["locale_file", "data_file"] {
            if extra[format!("json_{k}")] == true {
                meta[k] = json!(true);
            }
        }
        data["json"] = meta;
    } else if kind == "File" && extra.get("values_indexed").is_some() {
        let mut meta = json!({});
        for k in [
            "document_count",
            "duplicate_key_count",
            "unsupported_key_count",
        ] {
            meta[k] = extra.get(format!("yaml_{k}")).cloned().unwrap_or(json!(0));
        }
        meta["truncated"] = json!(extra["yaml_truncated"] == true);
        meta["values_indexed"] = json!(false);
        if extra["yaml_locale_file"] == true {
            meta["locale_file"] = json!(true);
        }
        if let Some(list) = extra["yaml_duplicate_keys"].as_array() {
            let mut duplicates = Vec::new();
            for item in list.iter().take(100).filter(|x| x.is_object()) {
                let lines: Vec<_> = item["line_samples"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(20)
                    .filter(|v| v.is_i64() || v.is_u64())
                    .collect();
                duplicates.push(json!({"document_index":item["document_index"],"path":clean(text(item,"path")),"occurrence_count":item["occurrence_count"],"line_samples":lines,"lines_truncated":item["lines_truncated"]==true}));
            }
            meta["duplicate_keys"] = json!(duplicates);
        }
        data["yaml"] = meta;
    }
}
