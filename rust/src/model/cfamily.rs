use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CFamily {
    pub language: String,
    pub declarations: Vec<Definition>,
    pub imports: Vec<(usize, String)>,
    pub sites: BTreeMap<usize, Vec<Site>>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Definition {
    pub identity: String,
    pub name: String,
    pub parent: Option<String>,
    pub container: Option<String>,
    pub params: Option<String>,
    pub doc: Option<String>,
    pub start: usize,
    pub bases: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Site {
    pub caller: String,
    pub raw: String,
    pub receiver: Option<String>,
    pub target: Option<String>,
    pub candidates: Vec<String>,
    pub ambiguous: bool,
    pub unresolved: bool,
}

impl CFamily {
    pub fn site(&self, caller: &str, raw: &str, line: usize) -> Option<&Site> {
        self.sites
            .get(&line)?
            .iter()
            .find(|s| s.caller == caller && s.raw == raw)
    }

    pub fn evidence(&self, caller: &str, raw: &str, line: usize, path: &str) -> Value {
        let Some(site) = self.site(caller, raw, line) else {
            return Value::Null;
        };
        let mut extra = json!({});
        if let Some(receiver) = &site.receiver {
            extra["receiver"] = json!(receiver);
        }
        if site.ambiguous || site.unresolved {
            let prefix = if site.ambiguous {
                "ambiguous"
            } else {
                "unresolved"
            };
            extra[format!("{prefix}_target_count")] = json!(site.candidates.len());
            extra[format!("{prefix}_targets_truncated")] = json!(site.candidates.len() > 20);
            extra[format!("{prefix}_targets")] = json!(
                site.candidates
                    .iter()
                    .take(20)
                    .map(|n| format!("{path}::{n}"))
                    .collect::<Vec<_>>()
            );
        }
        extra
    }
}
