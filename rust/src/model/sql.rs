use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SqlFacts {
    #[serde(default)]
    pub dbt_membership: Option<bool>,
    pub declarations: BTreeMap<String, SqlDeclaration>,
    pub tables: Vec<(usize, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlDeclaration {
    pub name: String,
    pub parent: Option<String>,
    pub extra: Value,
}
