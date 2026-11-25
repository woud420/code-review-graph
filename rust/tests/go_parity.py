"""Compare the public and proof Go indexers with immutable Go source/regressions."""
import argparse
import ast
from contextlib import closing
from hashlib import sha256
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import git, new_repo, verify_source

FIXTURES = {
    "sample_go.go": "45f271fe4f00c8e070aa0d1c0ec9357a855ec2aa9f8e49b4222630987f17555e",
    "sample_dead_guard.go": "25ae8e719993ed268825f24b4d98e4749b4737a35fa7c72eb0dffaa41a9b2dd7",
}
WITNESS = {
    "go.mod": "module example.org/local\n\ngo 1.22\nreplace example.org/dependency => ./localdep\n",
    "pkg/fees/lib_test.go": "package fees\nfunc helper() {}\nfunc TestEstimate() { helper() }\nfunc helperWrapper() { helper() }\n",
    "tests/helpers.go": "package helpers\nfunc Run(){}\nfunc test(){ Run() }\n",
    "tools/go.mod": "module example.org/tooling\ngo 1.22\n",
    "tools/inner/formatter.go": "package inner\nfunc Format(){}\n",
    "tools/tool.go": 'package tools\nimport (\n"example.org/tooling/inner"\n"example.org/local/pkg/fees"\n)\n',
    "localdep/sub/lib.go": "package lib\nfunc External() {}\n",
    "pkg/fees/lib.go": "package fees\nfunc Quote() int { return 1 }\n",
    "main.go": '''package main

import (
    fees "example.org/local/pkg/fees"
    _ "example.org/dependency/sub"
    "fmt"
    "example.com/missing"
)

// Invoice contains the amount.
type Invoice struct { Base }
type Base struct{}
type Receipt interface { Base }
func (bill *Invoice) Total() int { return 1 }
func (bill *Invoice) Print() { fmt.Println(bill.Total(), fees.Quote()) }
''',
}


def expectations(db, root):
    with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)) as conn:
        def calls(file, method):
            return set(conn.execute("SELECT target_qualified,line FROM edges WHERE kind='CALLS' AND source_qualified=?",
                                    (str(root / file) + "::" + method,)))
        sample = str(root / "sample_go.go")
        assert calls("sample_go.go", "ShadowA.CallsAfterShadowScope") == {("Save", 116), (sample + "::ShadowA.Save", 118)}
        assert calls("sample_go.go", "InMemoryRepo.SaveAndReturn") == {(sample + "::InMemoryRepo.Save", 42)}
        assert calls("sample_dead_guard.go", "caller") == {(str(root / "sample_dead_guard.go") + "::live_helper", 27)}
        imports = {(target, json.loads(extra or "{}").get("import_scope")) for target, extra in
                   conn.execute("SELECT target_qualified,extra FROM edges WHERE kind='IMPORTS_FROM' AND file_path=?",
                                (str(root / "main.go"),))}
        assert imports == {(str(root/"pkg/fees"), "package"), (str(root/"localdep/sub"), "package"),
                           ("fmt", None), ("example.com/missing", None)}, imports
        assert conn.execute("SELECT count(*) FROM edges WHERE kind='INHERITS'").fetchone()[0] >= 4


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--proof-binary", type=Path, required=True)
    args = p.parse_args()
    source, native, proof = args.upstream.resolve(), str(args.binary.resolve()), str(args.proof_binary.resolve())
    verify_source(source)
    files = dict(WITNESS)
    for name, hash_value in FIXTURES.items():
        raw = (source / "tests/fixtures" / name).read_bytes()
        assert sha256(raw).hexdigest() == hash_value
        files[name] = raw.decode()
    for path, fn in [("test_go_embeddings.py", "test_grouped_declarations_keep_separate_embedding_owners")]:
        tree = ast.parse((source / "tests" / path).read_text())
        example = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == fn)
        files["grouped.go"] = ast.literal_eval(example.body[0].value).decode()

    with tempfile.TemporaryDirectory(prefix="crg-go-oracle-") as name:
        temp = Path(name).resolve()
        root = temp / "repo"
        new_repo(root, files)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "FASTMCP_"))}
        env.update(CRG_SERIAL_PARSE="1", CRG_HOME=str(temp/"home"), PYTHONDONTWRITEBYTECODE="1")
        results = {}
        def compare(phase, command, skip):
            graphs = []
            for engine, binary in [("reference", [sys.executable, "-m", "code_review_graph"]), ("rust", [native])]:
                directory = temp / (engine + ("-raw" if skip else "-full"))
                options = dict(env, CRG_DATA_DIR=str(directory))
                if engine == "reference":
                    options["PYTHONPATH"] = str(source)
                subprocess.run([*binary, command, "--repo", str(root), "-q", *(["--skip-postprocess"] if skip else [])],
                               env=options, cwd=root, check=True, timeout=80)
                db = directory / "graph.db"
                if phase == "initial" and skip:
                    expectations(db, root)
                graphs.append(read_graph(db, root))
            for group, expected in graphs[0].items():
                actual = graphs[1][group]
                missing, extra = expected-actual, actual-expected
                results[f"{phase}/{'raw' if skip else 'full'}/{group}"] = (expected.total(),missing.total(),extra.total())
                if missing or extra:
                    print(f"{phase}/{group}: missing {missing.most_common(4)}; extra {extra.most_common(4)}", file=sys.stderr)
        compare("initial", "build", True)
        compare("initial", "build", False)
        subprocess.run([proof, "build", "--repo", str(root)], cwd=root, env=env, check=True, capture_output=True)
        def query(target):
            result = subprocess.check_output([proof, "query", "callers-of", target, "--repo", str(root)], cwd=root, env=env)
            return {r["name"] for r in json.loads(result)["results"]}
        assert query("sample_go.go::InMemoryRepo.Save") == {"InMemoryRepo.SaveAndReturn"}
        assert query("sample_dead_guard.go::dead_false_call") == set()
        (root / "sample_dead_guard.go").unlink()
        (root / "main.go").write_text(files["main.go"].replace("Total", "Amount"))
        git(root, "commit", "-qam", "change receiver method and delete a Go source")
        compare("incremental", "update", True)
        compare("incremental", "update", False)
        subprocess.run([proof, "update", "--repo", str(root)], cwd=root, env=env, check=True, capture_output=True)
        assert query("main.go::Invoice.Amount") == {"Invoice.Print"} and not query("main.go::Invoice.Total")
        failures = sum(missing+extra for _, missing, extra in results.values())
        for case, (total, missing, extra) in results.items():
            print(f"{case}: {total} upstream rows, {missing} missing, {extra} extra")
        return int(bool(failures))

if __name__ == "__main__":
    sys.exit(main())
