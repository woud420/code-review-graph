use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JsonFacts {
    pub file: Value,
    pub paths: BTreeMap<String, PathInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathInfo {
    pub parent: Option<String>,
    pub extra: Value,
}
