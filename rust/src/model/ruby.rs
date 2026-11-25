use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct RubyFacts {
    pub names: BTreeMap<String, RubyName>,
    pub imports: Vec<(String, String, usize)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RubyName {
    pub name: String,
    pub parent: Option<String>,
    pub container: String,
}
