"""Build the same source path with the pinned public Python CLI and the native CLI."""
import os
import subprocess
import sys

from canonical_parity import read_graph


def compare_builds(scratch, root, upstream, binary, step, update):
    results = []
    for name, cmd in (("python", [sys.executable, "-m", "code_review_graph"]), ("rust", [str(binary)])):
        data = scratch / name
        env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "FASTMCP_"))}
        env.update(CRG_HOME=str(scratch / "home"), CRG_DATA_DIR=str(data), CRG_SERIAL_PARSE="1",
                   PYTHONHASHSEED="0", PYTHONDONTWRITEBYTECODE="1")
        if name == "python":
            env["PYTHONPATH"] = str(upstream)
        op = "update" if name == "rust" and update else "build"
        subprocess.run([*cmd, op, "--quiet", "--repo", str(root)], cwd=root, env=env, check=True, timeout=60)
        results.append(read_graph(data / "graph.db", root))
    reference, native = results
    for key, rows in reference.items():
        if rows != native[key]:
            raise AssertionError(f"{step} {key}: missing {list(rows-native[key])[:2]}, extra {list(native[key]-rows)[:2]}")
    return native
