"""Compare Rust call target resolution and confidence on frozen upstream resolver test fixtures."""

import argparse
import ast
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from differential import new_repo, verify_source


def fixture_args(source):
    for filename in ("test_rust_scoped_calls.py", "test_scoped_calls_need_evidence.py"):
        tree = ast.parse((source / "tests" / filename).read_text())
        constants = {
            n.targets[0].id: n.value
            for n in tree.body
            if isinstance(n, ast.Assign) and isinstance(n.targets[0], ast.Name)
        }
        for test in tree.body:
            if (
                not isinstance(test, ast.FunctionDef)
                or not test.name.startswith("test_")
                or "php" in test.name
            ):
                continue
            for call in ast.walk(test):
                if (
                    not isinstance(call, ast.Call)
                    or not isinstance(call.func, ast.Name)
                    or call.func.id not in ("_build", "_targets")
                ):
                    continue
                files = call.args[1]
                assert isinstance(files, ast.Dict)
                yield (
                    test.name,
                    {
                        ast.literal_eval(k): ast.literal_eval(
                            constants[v.id] if isinstance(v, ast.Name) else v
                        )
                        for k, v in zip(files.keys, files.values)
                    },
                )
    receiver = ast.parse((source / "tests/test_untyped_receiver_method_calls.py").read_text())
    strings = {
        n.targets[0].id: ast.literal_eval(n.value)
        for n in receiver.body
        if isinstance(n, ast.Assign) and isinstance(n.targets[0], ast.Name)
    }
    yield "untyped_receiver", {"Cargo.toml": strings["CARGO"], "src/lib.rs": strings["LIB_RS"]}


def calls(database):
    with sqlite3.connect(database) as db:
        rows = db.execute("""SELECT source_qualified,target_qualified,file_path,line,confidence,confidence_tier,target_resolution,extra
                           FROM edges WHERE kind IN ('CALLS','TESTED_BY') ORDER BY 1,2,3,4""")
        return [(*r[:-1], json.loads(r[-1] or "{}")) for r in rows]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--upstream", type=Path, required=True)
    args = p.parse_args()
    binary, source = args.binary.resolve(), args.upstream.resolve()
    verify_source(source)
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("CRG_", "GIT_", "PYTHON", "FASTMCP_"))
    }
    fixtures = list(fixture_args(source))
    assert len(fixtures) == 15, [n for n, _ in fixtures]
    count = 0
    with tempfile.TemporaryDirectory(prefix="crg-rust-scoped-") as t:
        data = Path(t).resolve()
        for i, (name, files) in enumerate(fixtures):
            root = data / f"repo-{i}"
            new_repo(root, files)
            results = []
            for target, cli in [
                ("reference", [sys.executable, "-B", "-m", "code_review_graph"]),
                ("rust", [str(binary)]),
            ]:
                path = data / f"{i}-{target}"
                cfg = dict(
                    env,
                    CRG_DATA_DIR=str(path),
                    CRG_HOME=str(data / "registry"),
                    PYTHONPATH=str(source),
                    CRG_SERIAL_PARSE="1",
                )
                subprocess.run(
                    [*cli, "build", "-q", "--repo", str(root)], cwd=root, env=cfg, check=True
                )
                results.append(calls(path / "graph.db"))
            assert results[0] and results[0] == results[1], (name, results)
            count += len(results[0])
    print(
        f"{len(fixtures)} upstream Rust fixtures matched {count} call and test edges, targets, confidence, and receiver evidence."
    )


if __name__ == "__main__":
    main()
