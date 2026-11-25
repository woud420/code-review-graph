use tree_sitter::Language;
pub(super) fn get(name: &str) -> Option<Language> {
    Some(
        match name {
            "python" => tree_sitter_python::LANGUAGE,
            "javascript" => tree_sitter_javascript::LANGUAGE,
            "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            "tsx" => tree_sitter_typescript::LANGUAGE_TSX,
            "rust" => tree_sitter_rust::LANGUAGE,
            "go" => tree_sitter_go::LANGUAGE,
            "java" => tree_sitter_java::LANGUAGE,
            "csharp" => tree_sitter_c_sharp::LANGUAGE,
            "c_sharp" => tree_sitter_c_sharp::LANGUAGE,
            "ruby" => tree_sitter_ruby::LANGUAGE,
            "c" => tree_sitter_c::LANGUAGE,
            "cpp" => tree_sitter_cpp::LANGUAGE,
            "kotlin" => tree_sitter_kotlin_ng::LANGUAGE,
            "swift" => tree_sitter_swift::LANGUAGE,
            "php" => tree_sitter_php::LANGUAGE_PHP,
            "scala" => tree_sitter_scala::LANGUAGE,
            "solidity" => tree_sitter_solidity::LANGUAGE,
            "dart" => tree_sitter_dart::LANGUAGE,
            "r" => tree_sitter_r::LANGUAGE,
            "perl" => tree_sitter_perl::LANGUAGE,
            "lua" => tree_sitter_lua::LANGUAGE,
            "luau" => tree_sitter_luau::LANGUAGE,
            "objc" => tree_sitter_objc::LANGUAGE,
            "bash" => tree_sitter_bash::LANGUAGE,
            "elixir" => tree_sitter_elixir::LANGUAGE,
            "zig" => tree_sitter_zig::LANGUAGE,
            "powershell" => tree_sitter_powershell::LANGUAGE,
            "julia" => tree_sitter_julia::LANGUAGE,
            "gdscript" => tree_sitter_gdscript::LANGUAGE,
            "nix" => tree_sitter_nix::LANGUAGE,
            "verilog" => tree_sitter_verilog::LANGUAGE,
            "sql" => tree_sitter_sequel::LANGUAGE,
            "hcl" => tree_sitter_hcl::LANGUAGE,
            "yaml" => tree_sitter_yaml::LANGUAGE,
            "json" => tree_sitter_json::LANGUAGE,
            "xml" => tree_sitter_xml::LANGUAGE_XML,
            "html" => tree_sitter_html::LANGUAGE,
            _ => return None,
        }
        .into(),
    )
}

pub(super) fn builtin_extension(s: &str) -> bool {
    matches!(
        s,
        "astro"
            | "bash"
            | "c"
            | "cc"
            | "cjs"
            | "cpp"
            | "cs"
            | "cts"
            | "cxx"
            | "dart"
            | "ex"
            | "exs"
            | "fnc"
            | "gd"
            | "go"
            | "h"
            | "hcl"
            | "hh"
            | "hpp"
            | "htm"
            | "html"
            | "ipynb"
            | "java"
            | "jl"
            | "js"
            | "json"
            | "jsx"
            | "ksh"
            | "kt"
            | "kts"
            | "liquid"
            | "lua"
            | "luau"
            | "m"
            | "mjs"
            | "mts"
            | "nix"
            | "pck"
            | "php"
            | "pkb"
            | "pks"
            | "pl"
            | "plb"
            | "pls"
            | "pm"
            | "prc"
            | "properties"
            | "ps1"
            | "psd1"
            | "psm1"
            | "py"
            | "r"
            | "rb"
            | "res"
            | "resi"
            | "resource"
            | "robot"
            | "rs"
            | "scala"
            | "sh"
            | "sol"
            | "sql"
            | "sv"
            | "svelte"
            | "svh"
            | "swift"
            | "t"
            | "tf"
            | "trg"
            | "ts"
            | "tsx"
            | "v"
            | "vb"
            | "vh"
            | "vue"
            | "xs"
            | "xsl"
            | "xslt"
            | "yaml"
            | "yml"
            | "zig"
            | "zsh"
    )
}

pub(super) fn builtin_name(s: &str) -> bool {
    matches!(
        s,
        "bash"
            | "c"
            | "cpp"
            | "csharp"
            | "dart"
            | "elixir"
            | "gdscript"
            | "go"
            | "hcl"
            | "html"
            | "java"
            | "javascript"
            | "json"
            | "julia"
            | "kotlin"
            | "liquid"
            | "lua"
            | "luau"
            | "nix"
            | "notebook"
            | "objc"
            | "perl"
            | "php"
            | "powershell"
            | "properties"
            | "python"
            | "r"
            | "rescript"
            | "robot"
            | "ruby"
            | "rust"
            | "scala"
            | "solidity"
            | "sql"
            | "svelte"
            | "swift"
            | "tsx"
            | "typescript"
            | "vbnet"
            | "verilog"
            | "vue"
            | "xslt"
            | "yaml"
            | "zig"
    )
}
