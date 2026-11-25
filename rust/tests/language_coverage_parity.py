"""Exercise shipped suffixes missing from the canonical multilingual fixture and custom grammars."""
import argparse
import ast
import json
from pathlib import Path
import os
import sqlite3
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import new_repo, verify_source


def all_suffixes(upstream, binary, base, environment):
    # Pin and inspect the public parser map so new suffixes enter this gate.
    module = ast.parse((upstream / "code_review_graph/parser.py").read_text())
    extensions = next(ast.literal_eval(node.value) for node in module.body
                      if isinstance(node, ast.AnnAssign) and getattr(node.target, "id", "") == "EXTENSION_TO_LANGUAGE")
    repo = base / "suffixes"
    texts = {".ipynb": '{"metadata":{},"cells":[],"nbformat":4,"nbformat_minor":5}\n',
             ".json": "{}\n", ".yml": "key: 1\n", ".yaml": "key: 1\n", ".properties": "setting=value\n"}
    files = {"files/" + ("application" if ext == ".properties" else "sample") + ext: texts.get(ext, "\n")
             for ext in extensions}
    new_repo(repo, files)
    file_sets = []
    for index, head in enumerate(([sys.executable, "-m", "code_review_graph"], [str(binary)])):
        data = base / f"suffix-data-{index}"
        result = subprocess.run([*head, "build", "--repo", str(repo), "--data-dir", str(data), "--skip-postprocess", "-q"],
                                cwd=repo, env=environment, capture_output=True, text=True, timeout=90)
        assert result.returncode == 0, (result.stdout, result.stderr)
        file_sets.append({(path, lang) for record in read_graph(data / "graph.db", repo)["files"]
                          for path, _, lang in [json.loads(record)]})
    left, right = file_sets
    assert left == right, {"missing_file_languages": sorted(left - right), "extra_file_languages": sorted(right - left)}
    print(f"all {len(extensions)} released suffixes: {len(left)} indexed file/language pairs agree")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    source, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(source)
    files = {
        "env/app.ps1": 'function Get-Thing { Write-Output "hello" }\nGet-Thing\n',
        "web/component.svelte": '<script lang="ts">\nfunction greet(n: string) {return n;}\nfunction run() {greet("x");}\n</script>\n<h1>{run()}</h1>\n',
        "web/page.astro": '---\nconst title = "greeting";\n---\n<h1>{title}</h1>\n',
        "legacy/Program.vb": 'Module Program\n    Sub Main()\n        SayHi()\n    End Sub\n    Sub SayHi()\n    End Sub\nEnd Module\n',
        "suite.robot": '*** Test Cases ***\nMy Smoke Test\n    Log    Hello\n',
        "web/HOME.BLADE.PHP": "{{-- before\n@include('hidden') --}}\n@@extends('escaped')\n@extends('layouts.app')\n@component('alert')\n@include(\"summary\")\n@livewire('counter')\n",
        "web/invalid.blade.php": "\n@include('still.visible')\n",
        "upper.PY": 'def upper():\n    return 1\n',
        "feature.TS": 'export function feature(): number { return 2; }\n',
        "ordinary.php": '<?php\n// @include("ignored")\nclass Regular {}\n',
        "data.acm": 'struct Item;\nuse external::config;\nfn convert(i: i32) -> i32 { i }\nfn call(i: i32) -> i32 { convert(i) }\n',
        "module.jcustom": "class Example { Result build() { return null; } }\n",
        "ignored.shadow": "fn should_not_appear() {}\n",
        "ignored.invalid": "class Ignored {}\n",
        ".code-review-graph/languages.toml": '[languages.acme]\nextensions=[".acm"]\ngrammar="rust"\nfunction_node_types=["function_item"]\nclass_node_types=["struct_item"]\nimport_node_types=["use_declaration"]\ncall_node_types=["call_expression"]\n'
            '[languages.jcustom]\nextensions=[".jcustom"]\ngrammar="java"\nfunction_node_types=["method_declaration"]\nname_field="name"\n'
            '[languages.invalid]\nextensions=[".invalid", ".ts"]\ngrammar="java"\nclass_node_types=["class_declaration"]\n'
            '[languages.shadow]\nextensions=[".jcustom", ".shadow"]\ngrammar="rust"\nfunction_node_types=["function_item"]\n',
    }
    expected = {"app.ps1": "powershell", "component.svelte": "svelte", "page.astro": "typescript",
                "Program.vb": "vbnet", "suite.robot": "robot", "HOME.BLADE.PHP": "blade",
                "upper.PY": "python", "feature.TS": "typescript", "ordinary.php": "php", "invalid.blade.php": "blade", "data.acm": "acme", "module.jcustom": "jcustom"}
    with tempfile.TemporaryDirectory(prefix="crg-shipped-languages-") as tmp:
        base = Path(tmp).resolve()
        repo = base / "source"
        new_repo(repo, files)
        invalid = repo / "web/invalid.blade.php"
        invalid.write_bytes(b"\xff" + invalid.read_bytes())
        env = os.environ | {"CRG_HOME": str(base / "registry"), "CRG_PARSE_EXECUTOR": "serial",
                            "PYTHONHASHSEED": "0", "PYTHONPATH": str(source), "PYTHONDONTWRITEBYTECODE": "1"}
        all_suffixes(source, binary, base, env)
        commands = ([sys.executable, "-m", "code_review_graph"], [str(binary)])
        for label, command in (("fresh", "build"), ("edited", "update")):
            if label == "edited":
                for filename in ("web/component.svelte", "upper.PY", "data.acm"):
                    path = repo / filename
                    path.write_text(path.read_text().replace("greet", "welcome").replace("upper", "edited").replace("convert", "parse"))
            reports = []
            for index, head in enumerate(commands):
                data = base / str(index)
                result = subprocess.run([*head, command, "--repo", str(repo), "--data-dir", str(data), "-q"],
                               cwd=repo, env=env, capture_output=True, text=True, timeout=90)
                assert result.returncode == 0, (result.stdout, result.stderr)
                reports.append(read_graph(data / "graph.db", repo))
            assert not any("ignored." in row for row in reports[0]["files"])
            assert any("module.jcustom::build" in row for row in reports[0]["symbols"])
            if label=="fresh":
                for supported in (True, False):
                    data = base / ("adopt" if supported else "unknown")
                    data.mkdir()
                    with sqlite3.connect(base / "0" / "graph.db") as src, sqlite3.connect(data / "graph.db") as dest:
                        src.backup(dest)
                        if not supported:
                            dest.execute("UPDATE nodes SET language='future_language' WHERE kind='File' AND file_path LIKE '%/data.acm'")
                    before = read_graph(data / "graph.db", repo)
                    result = subprocess.run([str(binary), "update", "--repo", str(repo), "--data-dir", str(data), "-q"],
                                            cwd=repo, env=env, capture_output=True, text=True, timeout=90)
                    if supported:
                        assert result.returncode == 0, result.stderr
                    else:
                        assert result.returncode != 0 and 'future_language' in result.stderr, result.stderr
                    assert read_graph(data / "graph.db", repo) == before
            for ref, count in expected.items():
                assert any(ref in row and f'"{count}"' in row for row in reports[0]["files"]), (ref, count)
            errors = {}
            for metric in reports[0]:
                left, right = (r[metric] for r in reports)
                if left != right:
                    errors[metric] = {"missing": list((left-right).elements())[:4], "extra": list((right-left).elements())[:4]}
            if errors:
                raise AssertionError(f"{label}: {errors}")
            print(f"{label}: {len(expected)} language and suffix cases, all nine canonical projections match")


if __name__ == "__main__":
    main()
