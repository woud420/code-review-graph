"""Compare retrieval benchmark rows against the pinned Python CLI using a local fixture."""

import argparse
import csv
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from differential import new_repo, verify_source

BENCHMARKS = "agent_baseline,multi_hop_retrieval"


def solve(cli, env, cwd, config):
    cwd.mkdir()
    result = subprocess.run(
        [
            *cli,
            "eval",
            "--repo",
            str(config.with_suffix("")),
            "--benchmark",
            BENCHMARKS,
            "--output-dir",
            "rows",
        ],
        cwd=cwd,
        env=env,
        capture_output=True,
        check=True,
    )
    tables = {table.name: table.read_bytes() for table in (cwd / "rows").glob("*.csv")}
    assert len(tables) == 2 and b"Completed 2 benchmark(s)" in result.stdout, (result, tables)
    shutil.rmtree(cwd)
    return result.stdout, tables


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-retrieval-") as name:
        temp = Path(name).resolve()
        mirror = temp / "mirror"
        new_repo(
            mirror,
            {
                "lib.py": "def get_users():\n    return 'Zoë'\n\nclass ApiClient:\n    def fetch(self):\n        return get_users()\n",
                "handler.py": "from lib import get_users\n\ndef request():\n    return get_users()\n",
                "tests/test_lib.py": "from lib import get_users\n\ndef test_users():\n    assert get_users() == 'Zoë'\n",
                "dist/ignored.py": "# get_users get_users get_users get_users\n",
            },
        )
        sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=mirror, text=True).strip()
        config = temp / "sample.yaml"
        base = {
            "name": "sample",
            "url": str(mirror),
            "commit": sha,
            "search_queries": [{"query": "get_users", "expected": "lib.py::get_users"}],
            "multi_hop_tasks": [
                {
                    "id": "caller",
                    "nl_query": "get_users",
                    "anchor_qualified_suffix": "lib.py::get_users",
                    "expected_neighbor_names": ["request", "missing"],
                },
                {
                    "id": "callee",
                    "nl_query": "request",
                    "anchor_qualified_suffix": "handler.py::request",
                    "traversal_pattern": "callees_of",
                    "expected_neighbor_names": ["get_users"],
                },
                {
                    "id": "absent",
                    "nl_query": "xyzzzaaa",
                    "anchor_qualified_suffix": "not.present",
                    "k": 1,
                },
                {
                    "id": "empty-expected",
                    "nl_query": "get_users",
                    "anchor_qualified_suffix": "lib.py::get_users",
                    "expected_neighbor_names": [],
                },
            ],
        }
        env = {k: v for k, v in os.environ.items() if k in ("PATH", "TMPDIR", "TMP", "TEMP")}
        env.update(
            HOME=str(temp),
            CRG_HOME=str(temp / "registry"),
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            PYTHONPATH=str(upstream),
            CRG_SERIAL_PARSE="1",
            PYTHONHASHSEED="0",
            NO_COLOR="1",
        )
        for extra in [
            {
                "agent_questions": [
                    "get_users",
                    "How does ApiClient.fetch use get_users?",
                    "tests for get_users?",
                    "the",
                    "xyzzzaaa",
                ],
                "agent_baseline_top_k": 2,
            },
            {},
        ]:
            config.write_text(json.dumps(dict(base, **extra)))
            expected = solve(
                [sys.executable, "-B", "-m", "code_review_graph"], env, temp / "runner", config
            )
            actual = solve([str(binary)], env, temp / "runner", config)
            assert expected == actual, (expected, actual)
            rows = [
                r for data in actual[1].values() for r in csv.DictReader(io.StringIO(data.decode()))
            ]
            assert any(r.get("anchor_found") == "False" for r in rows)
            assert any(r.get("matched_count") == "1" and r.get("score") == "0.5" for r in rows), (
                rows
            )
            assert any(
                r.get("graph_tokens", "0").isdigit() and int(r.get("graph_tokens", 0)) > 0
                for r in rows
            )
            if extra:
                assert any(r.get("status") == "no_graph_results" for r in rows)
                assert all("dist/ignored.py" not in r.get("top_files", "") for r in rows)
    print("Agent baseline and multi-hop retrieval matched 14 independently measured CSV rows.")


if __name__ == "__main__":
    main()
