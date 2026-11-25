pub(super) fn comment(extension: &str) -> Option<&'static str> {
    match extension {
        "py" | "rb" | "sh" | "bash" | "pl" | "r" | "jl" | "ex" | "exs" | "yaml" | "yml" => {
            Some("#")
        }
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "go" | "java" | "c" | "h" | "cc" | "cpp"
        | "hpp" | "cs" | "rs" | "php" | "swift" | "kt" | "scala" | "dart" => Some("//"),
        "lua" | "sql" | "hs" => Some("--"),
        "erl" => Some("%"),
        _ => None,
    }
}

pub(super) fn probe(extension: &str) -> Option<&'static str> {
    match extension {
        "py" => Some(
            "\"\"\"Added by the incremental-fidelity benchmark.\"\"\"\n\n\ndef crg_fidelity_probe(value):\n    return value\n",
        ),
        "js" => Some(
            "// Added by the incremental-fidelity benchmark.\nfunction crgFidelityProbe(value) {\n  return value;\n}\nmodule.exports = { crgFidelityProbe };\n",
        ),
        "mjs" => Some(
            "// Added by the incremental-fidelity benchmark.\nexport function crgFidelityProbe(value) {\n  return value;\n}\n",
        ),
        "ts" => Some(
            "// Added by the incremental-fidelity benchmark.\nexport function crgFidelityProbe(value: string): string {\n  return value;\n}\n",
        ),
        "go" => Some(
            "package probe\n\n// CrgFidelityProbe is added by the incremental-fidelity benchmark.\nfunc CrgFidelityProbe(value string) string {\n\treturn value\n}\n",
        ),
        "rb" => Some(
            "# Added by the incremental-fidelity benchmark.\ndef crg_fidelity_probe(value)\n  value\nend\n",
        ),
        _ => None,
    }
}
