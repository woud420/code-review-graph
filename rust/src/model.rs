pub mod cfamily;
pub mod go;
pub mod hcl;
pub mod json;
pub mod ruby;
pub mod sql;
pub mod syntax;
mod test_paths;
pub mod typescript;
pub mod yaml;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub use test_paths::test_file;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ParsedFile {
    #[serde(default)]
    pub ruby: ruby::RubyFacts,
    #[serde(default)]
    pub php_imports: BTreeMap<String, String>,
    #[serde(default)]
    pub hcl: hcl::HclFacts,
    #[serde(default)]
    pub yaml: yaml::YamlFacts,
    #[serde(default)]
    pub syntax: syntax::SyntaxFacts,
    #[serde(default)]
    pub sql: sql::SqlFacts,
    #[serde(default)]
    pub typescript: typescript::TypeScriptFacts,
    #[serde(default)]
    pub bash_imports: Vec<(usize, String)>,
    pub symbols: Vec<Symbol>,
    pub calls: Vec<Call>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    #[serde(default)]
    pub python: PythonFacts,
    #[serde(default)]
    pub json: json::JsonFacts,
    #[serde(default)]
    pub go: go::GoFacts,
    #[serde(default)]
    pub cfamily: cfamily::CFamily,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct PythonFacts {
    pub scoped_imports: Vec<ScopedImport>,
    pub definitions: BTreeMap<String, PythonDefinition>,
    pub ambiguities: BTreeSet<String>,
    pub members: Vec<PythonMember>,
    #[serde(default)]
    pub receivers: Vec<PythonReceiver>,
    #[serde(default)]
    pub fields: BTreeMap<String, Vec<PythonType>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedImport {
    pub start: usize,
    pub end: usize,
    pub binding: Import,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct PythonDefinition {
    #[serde(default)]
    pub module_level: bool,
    pub parent: Option<String>,
    pub params: Option<String>,
    pub returns: Option<String>,
    pub modifiers: Option<String>,
    pub bases: Vec<String>,
    #[serde(default)]
    pub decorators: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonMember {
    pub caller: String,
    pub line: usize,
    pub raw: String,
    pub attr: String,
    pub receiver: PythonType,
    pub property: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonReceiver {
    pub caller: String,
    pub line: usize,
    pub raw: String,
    pub name: Option<String>,
    pub binding: Option<PythonType>,
    #[serde(default)]
    pub declared: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PythonType {
    Builtin,
    Parameter(String),
    Named(String),
    Returned {
        callee: String,
        element: Option<usize>,
    },
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
