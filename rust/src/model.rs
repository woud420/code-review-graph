use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ParsedFile {
    pub symbols: Vec<Symbol>,
    pub calls: Vec<Call>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub caller: String,
    pub callee: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    pub local: String,
    pub imported: String,
    pub module: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Export {
    pub name: String,
    pub local: String,
    pub module: Option<String>,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Hit {
    pub file: String,
    pub name: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub files: i64,
    pub symbols: i64,
    pub edges: i64,
}

#[derive(Debug, Serialize)]
pub struct BuildReport {
    pub files_parsed: usize,
    pub files_deleted: usize,
    pub files_unchanged: usize,
    pub files: i64,
    pub symbols: i64,
    pub edges: i64,
}
