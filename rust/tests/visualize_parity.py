"""Compare CLI streams and the actual rendered graph files, including full HTML and sidecars."""

import argparse
import importlib.util
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

from differential import git, new_repo
from public_parity import pinned_reference, run


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--serve", action="store_true", help="also compare the live loopback servers")
    args = p.parse_args()
    source = args.upstream.resolve()
    pinned_reference(source)
    with tempfile.TemporaryDirectory(prefix="crg-render-") as temp:
        folder = Path(temp).resolve()
        repo = folder / "repo"
        new_repo(
            repo,
            {
                **{
                    name: (source / "tests/fixtures" / name).read_text()
                    for name in ("sample_python.py", "caller_example.py")
                },
                ".gitignore": ".code-review-graph/\n",
                "core.py": "def dep(): return 1\ndef read(): return dep()\ndef process(): return read()\ndef main(): return process()\n",
                "tests/test_core.py": "from core import main\ndef test_smoke(): assert main() == 1\n",
                "config.json": '{"title":"runtime-only","nested":{"location":false}}',
            },
        )
        with (repo / "core.py").open("a") as modified:
            modified.write("\n# Second committed change.\n")
        git(repo, "add", "core.py")
        git(repo, "commit", "-qm", "change core")
        python = [sys.executable, "-m", "code_review_graph"]
        native = [str(args.binary.resolve())]
        env = {
            k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "FASTMCP_"))
        }
        env.update(
            CRG_HOME=str(folder / "registry"),
            CRG_PARSE_EXECUTOR="serial",
            PYTHONDONTWRITEBYTECODE="1",
            PYTHONHASHSEED="0",
        )
        pyenv = env | {"PYTHONPATH": str(source)}
        seed = run([*python, "build", "-q"], repo, pyenv)
        assert seed["exit"] == 0, seed
        data = repo / ".code-review-graph"
        with sqlite3.connect(data / "graph.db") as db:
            db.execute(
                "UPDATE metadata SET value='2099-01-01T00:00:00+00:00' WHERE key='last_updated'"
            )
            flow_id, flow_name = db.execute("SELECT id,name FROM flows ORDER BY criticality DESC LIMIT 1").fetchone()
        for flags in (
            ["--format", "json"],
            ["--format", "graphml"],
            ["--format", "cypher"],
            ["--format", "obsidian"],
            [],
            ["--mode", "file"],
            ["--mode", "community"],
            ["--sidecar"],
            ["--seed-symbol", "read"],
            ["--seed-symbol", "core.py::read", "--depth", "0"],
            ["--seed-symbol", "__init__", "--seed-symbol", "read", "--seed-file", "*/tests/*.py"],
            ["--seed-symbol", "read", "--depth", "3", "--render-depth", "2", "--sidecar"],
            ["--seed-file", "sample_python.py", "--max-nodes", "2"],
            ["--seed-changed"],
            ["--seed-flow", str(flow_id)],
            ["--seed-flow", flow_name[:max(len(flow_name) // 2, 1)].upper()],
            ["--path-from", "setup_and_run", "--path-to", "create_auth_service"],
            ["--path-from", "main", "--path-to", "dep", "--max-nodes", "4"],
            ["--path-from", "dep", "--path-to", "main"],
            ["--path-from", "main", "--path-to", "config.json"],
            ["--path-from", "main", "--path-to", "dep", "--seed-symbol", "read"],

        ):
            outputs = []
            for label, cmd, opts in [("reference", python, pyenv), ("native", native, env)]:
                command = run([*cmd, "visualize", "--repo", str(repo), *flags], repo, opts)
                assert command["exit"] == 0, (flags, command)
                files = {}
                for path in sorted(data.iterdir()):
                    if path.name in (
                        "graph.json",
                        "graph.graphml",
                        "graph.cypher",
                        "graph.html",
                        "graph.data.js",
                        "d3.v7.min.js",
                    ):
                        files[path.name] = path.read_bytes()
                        path.unlink()
                vault = data / "obsidian"
                if vault.is_dir():
                    files.update(
                        {p.relative_to(data).as_posix(): p.read_bytes() for p in vault.glob("*.md")}
                    )
                    shutil.rmtree(vault)
                assert files, flags
                outputs.append((command, files))
            (a, exp), (b, got) = outputs
            assert a == b, (flags, a, b)
            assert exp.keys() == got.keys(), (flags, exp.keys(), got.keys())
            for name, expected in exp.items():
                actual = got[name]
                if actual != expected:
                    at = next(
                        (i for i, (x, y) in enumerate(zip(expected, actual)) if x != y),
                        min(len(actual), len(expected)),
                    )
                    raise AssertionError(
                        (
                            flags,
                            name,
                            at,
                            expected[max(0, at - 50) : at + 100],
                            actual[max(0, at - 50) : at + 100],
                        )
                    )
            print(f"{flags or ['default HTML']}: identical console and {', '.join(exp)}")

        checks = [
            (0, ["--seed-changed", "--seed-changed-base", "HEAD"]),
            (2, ["--seed-symbol", "not_a_symbol"]),
            (2, ["--seed-file", "missing.py"]),
            (2, ["--seed-flow", "missing flow"]),
            (2, ["--seed-symbol", "main", "--depth", "-1"]),
            (2, ["--seed-symbol", "main", "--render-depth", "-1"]),
            (2, ["--seed-symbol", "main", "--depth", "1", "--render-depth", "2"]),
            (2, ["--seed-symbol", "main", "--max-nodes", "0"]),
            (2, ["--path-from", "main"]),
            (2, ["--path-to", "dep"]),
            (2, ["--path-from", "main", "--path-to", "dep", "--max-nodes", "2"]),
            (2, ["--seed-changed-base", "HEAD"]),
            (2, ["--format", "json", "--sidecar"]),
            (2, ["--format", "cypher", "--seed-file", "core.py"]),
            (2, ["--format", "graphml", "--depth", "1"]),
            (2, ["--mode", "community", "--seed-symbol", "main"]),
            (2, ["--max-nodes", "1"]),
        ]
        if importlib.util.find_spec("matplotlib") is None:
            checks.append((1, ["--format", "svg"]))
        for code, flags in checks:
            a, b = [run([*cmd, "visualize", *flags], repo, opt)
                    for cmd, opt in ((python, pyenv), (native, env))]
            assert a == b and a["exit"] == code, (flags, a, b)
            assert not any(data.glob("graph.html")) and not any(data.glob("graph.svg")), flags
        print(f"{len(checks)} empty/invalid requests and optional dependencies match")
        # A malformed value in a disconnected legacy row must never be decoded by
        # a local seed. A whole-graph SELECT * would fail to read this integer.
        with sqlite3.connect(data / "graph.db") as db:
            assert db.execute("UPDATE nodes SET line_start='unreadable' WHERE name='location' OR file_path LIKE '%/config.json'").rowcount > 0
        nearby = []
        for cmd, opts in ((python, pyenv), (native, env)):
            result = run([*cmd, "visualize", "--seed-symbol", "read", "--depth", "0", "--sidecar"], repo, opts)
            assert result["exit"] == 0, result
            nearby.append((result, (data/"graph.data.js").read_bytes()))
        assert nearby[0] == nearby[1]
        print("Seeded reads skip disconnected row payloads and preserve whole-graph totals")
        if args.serve:
            from visualize_serve import compare
            compare(repo, data, python, native, pyenv, env)


if __name__ == "__main__":
    main()
