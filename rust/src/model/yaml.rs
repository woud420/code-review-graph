use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct YamlFacts {
    pub language: String,
    pub file: Value,
    pub declarations: BTreeMap<String, Declaration>,
    pub edges: Vec<Edge>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Declaration {
    pub name: String,
    pub parent: Option<String>,
    pub extra: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edge {
    pub kind: String,
    pub source: String,
    pub target: String,
    pub local: bool,
    pub line: usize,
    pub extra: Value,
}
