use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GoFacts {
    pub declarations: BTreeMap<String, Definition>,
    pub imports: Vec<(usize, String)>,
    pub(crate) selectors: BTreeMap<usize, Vec<Selector>>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Definition {
    pub params: Option<String>,
    pub doc: Option<String>,
    pub extra: Value,
    pub bases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selector {
    pub(crate) caller: String,
    pub(crate) raw: String,
    pub(crate) receiver: String,
    pub own: bool,
}

impl GoFacts {
    pub fn selector(&self, caller: &str, raw: &str, line: usize) -> Option<&Selector> {
        self.selectors
            .get(&line)?
            .iter()
            .find(|s| s.caller == caller && s.raw == raw)
    }

    pub fn evidence(&self, caller: &str, raw: &str, line: usize) -> Value {
        self.selector(caller, raw, line).map_or(Value::Null, |s| {
            if s.own {
                json!({"receiver":s.receiver,"go_method_receiver":true})
            } else {
                json!({"receiver":s.receiver})
            }
        })
    }
}
