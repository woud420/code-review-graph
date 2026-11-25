"""Compare explicit incremental bases, graph freshness, brief review output and optional token verification."""

import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile

from differential import git
from mcp_contract import WireClient
from public_parity import pinned_reference, run


def repo_pair(root, refenv, ref):
    root.mkdir()
    git(root, "init", "-q", "-b", "main")
    (root / "app.py").write_text("def sample():\n    return 1\n")
    git(root, "add", "app.py")
    git(root, "commit", "-qm", "baseline")
    base = subprocess.check_output(["git", "-C", root, "rev-parse", "HEAD"], text=True).strip()
    assert run(ref + ["build", "-q", "--skip-postprocess"], root, refenv)["exit"] == 0
    return base


def state(root):
    with sqlite3.connect(root / ".code-review-graph/graph.db") as conn:
        return {
            "anchor": conn.execute(
                "select value from metadata where key='git_head_sha'"
            ).fetchone(),
            "rows": conn.execute(
                "select kind,name,file_path,line_start,line_end,file_hash from nodes order by file_path,line_start,name"
            ).fetchall(),
        }


def action(command, root, env, args, tool=False):
    if not tool:
        return run(
            [*command, "update", *args, "--skip-postprocess", "--repo", str(root)], root, env
        )
    client = WireClient([*command, "serve", "--repo", str(root)], root, env)
    try:
        client.call(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "update", "version": "1"},
            },
        )
        client.send({"method": "notifications/initialized"})
        result = client.call(
            "tools/call",
            {
                "name": "build_or_update_graph_tool",
                "arguments": {"postprocess": "none", "repo_root": str(root), **args},
            },
        )
        assert result["notifications"] == []
        content = result["response"]["result"]
        assert not content["isError"], content
        data = content["structuredContent"]
        assert json.loads(content["content"][0]["text"]) == data
        graph = data.get("_graph", {})
        if "updated_at" in graph:
            from datetime import datetime

            datetime.fromisoformat(graph["updated_at"])
            graph["updated_at"] = "<timestamp>"
        if "age_seconds" in graph:
            assert isinstance(graph["age_seconds"], int) and graph["age_seconds"] >= 0
            graph["age_seconds"] = "<age>"
        return data
    finally:
        client.close()


def tokenizer_vocabulary(path):
    if path is None:
        import tomllib

        lock = Path(__file__).resolve().parents[2] / "Cargo.lock"
        packages = tomllib.loads(lock.read_text())["package"]
        version = next(p["version"] for p in packages if p["name"] == "tiktoken-rs")
        registry = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/src"
        path = next(registry.glob(f"*/tiktoken-rs-{version}/assets/cl100k_base.tiktoken"), None)
        if path is None:
            raise FileNotFoundError("Set --tokenizer-vocabulary to the cl100k_base.tiktoken file")
    return path.read_bytes()


def main():
    flags = argparse.ArgumentParser(description=__doc__)
    flags.add_argument("--upstream", type=Path, required=True)
    flags.add_argument("--binary", type=Path, required=True)
    flags.add_argument("--tokenizer-binary", type=Path)
    flags.add_argument("--tokenizer-pythonpath", type=Path)
    flags.add_argument("--tokenizer-vocabulary", type=Path)
    args = flags.parse_args()
    source, binary = args.upstream.resolve(), args.binary.resolve()
    pinned_reference(source)
    ref = [sys.executable, "-B", "-m", "code_review_graph"]
    with tempfile.TemporaryDirectory(prefix="crg-update-") as n:
        p = Path(n).resolve()
        env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("CRG_", "GIT_", "PYTHON", "FASTMCP_"))
        }
        env.update(
            HOME=str(p / "home"),
            CRG_HOME=str(p / "registry"),
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            CRG_SERIAL_PARSE="1",
            PYTHONDONTWRITEBYTECODE="1",
            PYTHONHASHSEED="0",
            FASTMCP_LOG_LEVEL="ERROR",
        )
        refenv = dict(env, PYTHONPATH=str(source))
        root = p / "repo"
        base = repo_pair(root, refenv, ref)
        git(root, "switch", "-qc", "feature")
        (root / "feature.py").write_text("def café():\n    return '日本語'\n")
        git(root, "add", "feature.py")
        git(root, "commit", "-qm", "feature")
        git(root, "switch", "-q", "main")
        (root / "other.py").write_text("def other(): return 2\n")
        git(root, "add", "other.py")
        git(root, "commit", "-qm", "main")
        git(root, "switch", "-q", "feature")
        snapshot = p / "snapshot"
        shutil.copytree(root, snapshot)

        def reset():
            shutil.rmtree(root)
            shutil.copytree(snapshot, root)

        cases = [
            ("current-scope", ["--base", "HEAD"], {}),
            ("previous", ["--base", base], {}),
            ("broken-anchor-cli", ["--base", "missing-checkpoint"], {}),
            ("broken-anchor-tool", {"base": "missing-checkpoint"}, {}),
            (
                "restored-to-base",
                {"base": base},
                {"feature.py": None, "untracked.py": "def new(): pass\n"},
            ),
            ("unknown", ["--base", "unknown-ref"], {}),
            ("unsafe-ref", ["--base", "bad ref"], {}),
            ("merge-summary", ["--base", "main", "--brief"], {}),
            ("auto-brief", ["--brief"], {}),
            ("quiet-summary", ["--base", "main", "--brief", "-q"], {}),
            ("verify", ["--base", "main", "--brief", "--verify"], {}),
            ("stale-file", ["--base", "HEAD"], {"app.py": "def sample():\n    return 42\n"}),
            ("untracked", ["--base", "HEAD"], {"added.py": "def edge(): return 5\n"}),
            ("deleted", ["--base", "HEAD"], {"app.py": None}),
            ("explicit-noop", {"base": "HEAD"}, {}),
            ("stored-anchor", {"base": base}, {}),
            ("bad-base", {"base": "unknown-ref"}, {}),
            ("explicit-full", {"base": "unknown-ref", "full_rebuild": True}, {}),
            ("explicit-readme", {"base": "HEAD"}, {"note.txt": "hello\n"}),
            (
                "panel-without-functions",
                ["--base", "HEAD", "--brief", "--verify"],
                {"note.txt": "hello there\n"},
            ),
        ]

        def compare(name, actions, files, native, pythonenv):
            observations = []
            for command, environment in ((ref, pythonenv), ([str(native)], env)):
                reset()
                if name.startswith("broken-anchor"):
                    with sqlite3.connect(root / ".code-review-graph/graph.db") as conn:
                        conn.execute(
                            "update metadata set value=? where key='git_head_sha'",
                            ("missing-checkpoint",),
                        )
                for file, body in files.items():
                    if body is None:
                        (root / file).unlink()
                    else:
                        (root / file).write_text(body)
                result = action(command, root, environment, actions, isinstance(actions, dict))
                observations.append((result, state(root)))
            assert observations[0] == observations[1], (name, observations)
            return observations[0][0]

        for name, opts, files in cases:
            result = compare(name, opts, files, binary, refenv)
            if name == "explicit-noop":
                assert result["freshness_advanced"] is False and state(root)["anchor"] == (base,)
            if name == "broken-anchor-cli":
                assert result["exit"] == 1 and "git diff failed" in result["stderr"]
            if name == "broken-anchor-tool":
                assert result["status"] == "error" and state(root)["anchor"] == (
                    "missing-checkpoint",
                )
            if name == "restored-to-base":
                assert set(result["changed_files"]) == {"feature.py", "untracked.py"}
            if name == "current-scope":
                assert not any(r[1] == "café" for r in state(root)["rows"])
            if name == "merge-summary":
                assert (
                    "Analyzed 1 changed file(s)" in result["stdout"]
                    and "Token Savings" in result["stdout"]
                )
        print(f"{len(cases)} paired CLI/MCP outputs and persisted graph rows matched.")
        if args.tokenizer_binary:
            import hashlib

            vocab = tokenizer_vocabulary(args.tokenizer_vocabulary)
            assert (
                hashlib.sha256(vocab).hexdigest()
                == "223921b76ee99bde995b7ff738513eef100fb51d18c93597a113bcffe865b2a7"
            )
            cache = p / "vocab"
            cache.mkdir()
            url = "https://openaipublic.blob.core.windows.net/encodings/cl100k_base.tiktoken"
            (cache / hashlib.sha1(url.encode()).hexdigest()).write_bytes(vocab)
            paths = [str(source)]
            if args.tokenizer_pythonpath:
                paths.append(str(args.tokenizer_pythonpath.resolve()))
            tokenizer_env = dict(
                refenv,
                PYTHONPATH=os.pathsep.join(paths),
                TIKTOKEN_CACHE_DIR=str(cache),
            )
            for name, files in [
                ("tokenizer-source", {}),
                ("tokenizer-unicode", {"extra.py": "def 日本語(): return 'café'\n"}),
            ]:
                result = compare(
                    name,
                    ["--base", "main", "--brief", "--verify"],
                    files,
                    args.tokenizer_binary.resolve(),
                    tokenizer_env,
                )
                assert "Verified (tiktoken)" in result["stdout"]
            print("2 cl100k_base panels matched the independent upstream tokenizer byte for byte.")


if __name__ == "__main__":
    main()
