"""Compare CLI streams and the actual rendered graph files, including full HTML and sidecars."""

import argparse
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

from differential import new_repo
from public_parity import pinned_reference, run


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    args = p.parse_args()
    source = args.upstream.resolve()
    pinned_reference(source)
    with tempfile.TemporaryDirectory(prefix="crg-render-") as temp:
        folder = Path(temp).resolve()
        repo = folder / "repo"
        new_repo(
            repo,
            {
                "core.py": "def dep(): return 1\ndef read(): return dep()\ndef process(): return read()\ndef main(): return process()\n",
                "tests/test_core.py": "from core import main\ndef test_smoke(): assert main() == 1\n",
                "config.json": '{"title":"runtime-only","nested":{"location":false}}',
            },
        )
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
        for flags in (
            ["--format", "json"],
            ["--format", "graphml"],
            ["--format", "cypher"],
            ["--format", "obsidian"],
            [],
            ["--mode", "file"],
            ["--mode", "community"],
            ["--sidecar"],
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


if __name__ == "__main__":
    main()
