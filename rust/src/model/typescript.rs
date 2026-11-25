use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TypeScriptFacts {
    pub containers: BTreeMap<String, String>,
    pub kinds: BTreeMap<String, Value>,
    pub bases: BTreeMap<String, Vec<String>>,
    pub references: Vec<(String, String, usize)>,
    pub members: Vec<(String, String, usize, Value)>,
}
