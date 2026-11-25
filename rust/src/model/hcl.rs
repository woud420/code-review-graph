use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HclFacts {
    pub types: BTreeMap<String, String>,
    pub references: Vec<(String, String, usize)>,
    pub modules: Vec<(String, usize)>,
}
