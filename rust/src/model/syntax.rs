use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyntaxFacts {
    #[serde(default)]
    pub file: Value,
    pub declarations: BTreeMap<String, Definition>,
    pub edges: Vec<Edge>,
    pub sites: BTreeMap<usize, Vec<Site>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Definition {
    #[serde(default)]
    pub returns: Option<String>,
    #[serde(default)]
    pub modifiers: Option<String>,
    pub decorators: Vec<String>,
    pub params: Option<String>,
    pub doc: Option<String>,
    pub extra: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edge {
    pub kind: String,
    pub owner: String,
    pub target: String,
    pub line: usize,
    pub extra: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Site {
    pub caller: String,
    pub raw: String,
    pub target: Option<String>,
    pub extra: Value,
}

impl SyntaxFacts {
    pub fn site(&self, caller: &str, raw: &str, line: usize) -> Option<&Site> {
        self.sites
            .get(&line)?
            .iter()
            .find(|s| s.raw == raw && s.caller == caller)
    }
}
