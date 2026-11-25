"""Build and update a language corpus using independent reference and native processes."""

import argparse
import json
import os
import sqlite3
from pathlib import Path
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import new_repo, verify_source


def compare(language_name, corpus, assertions, change, proof_checks):
    p = argparse.ArgumentParser(
        description=f"Compare the pinned {language_name} corpus against both native CLIs"
    )
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--proof-binary", type=Path, required=True)
    args = p.parse_args()
    source = args.upstream.resolve()
    verify_source(source)
    files = corpus(source)
    native, proof = str(args.binary.resolve()), str(args.proof_binary.resolve())
    failures = []
    with tempfile.TemporaryDirectory(prefix=f"crg-{language_name}-") as tmp:
        temp = Path(tmp).resolve()
        root = temp / "repo"
        new_repo(root, files)
        env = {
            k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "FASTMCP_", "PYTHON"))
        }
        env.update(
            CRG_SERIAL_PARSE="1",
            CRG_HOME=str(temp / "home"),
            PYTHONDONTWRITEBYTECODE="1",
            PYTHONHASHSEED="0",
        )
        for command in ("build", "update"):
            if command == "update":
                change(root, files)
            for post in ("none", "full"):
                graphs = []
                for language, executable in (
                    ("python", [sys.executable, "-m", "code_review_graph"]),
                    ("rust", [native]),
                ):
                    directory = temp / (language + "-" + post)
                    opts = dict(env, CRG_DATA_DIR=str(directory))
                    if language == "python":
                        opts["PYTHONPATH"] = str(source)
                    subprocess.run(
                        [
                            *executable,
                            command,
                            "--repo",
                            str(root),
                            "-q",
                            *(["--skip-postprocess"] if post == "none" else []),
                        ],
                        env=opts,
                        cwd=root,
                        check=True,
                        timeout=120,
                    )
                    if command == "build" and post == "none":
                        assertions(directory / "graph.db", root)
                    graphs.append(read_graph(directory / "graph.db", root))
                    adoption = temp / ("adopted-" + post)
                    if language == "python" and command == "build":
                        adoption.mkdir()
                        with sqlite3.connect(directory / "graph.db") as saved, sqlite3.connect(adoption / "graph.db") as clone:
                            saved.backup(clone)
                    if language == "rust" and command == "update":
                        subprocess.run(
                            [native, "update", "--repo", str(root), "-q", *(["--skip-postprocess"] if post == "none" else [])],
                            env=dict(env, CRG_DATA_DIR=str(adoption)), cwd=root, check=True, timeout=120,
                        )
                        adopted = read_graph(adoption / "graph.db", root)
                        for group, expected in graphs[0].items():
                            if expected != adopted[group]:
                                failures.append(f"adopt/{post}/{group}")
                                print(f"adopt/{post}/{group}: missing {(expected-adopted[group]).most_common(2)}, extra {(adopted[group]-expected).most_common(2)}")
                        print(f"adopt/{post}: {sum(adopted[k] == v for k,v in graphs[0].items())}/{len(adopted)} matched on a Python database")
                for group, expected in graphs[0].items():
                    actual = graphs[1][group]
                    missing, extra = expected - actual, actual - expected
                    name = f"{command}/{post}/{group}"
                    print(
                        f"{name}: {expected.total()} upstream, {missing.total()} missing, {extra.total()} extra"
                    )
                    if missing or extra:
                        failures.append(name)
                        print(
                            f"{name}: missing {missing.most_common(4)}; extra {extra.most_common(4)}",
                            file=sys.stderr,
                        )
            subprocess.run(
                [proof, command, "--repo", str(root)], env=env, check=True, capture_output=True
            )

            def callers(target):
                out = subprocess.check_output(
                    [proof, "query", "callers-of", target, "--repo", str(root)], env=env
                )
                return {row["name"] for row in json.loads(out)["results"]}

            proof_checks(callers, command)
    return int(bool(failures))
