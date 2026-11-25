"""Compare Git submodule discovery in the native and pinned reference writers."""

import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

from differential import git, new_repo
from mcp_contract import WireClient
from public_parity import pinned_reference


def build(command, root, environment, recurse):
    client = WireClient([*command, "serve", "--repo", str(root)], root, environment)
    try:
        client.call("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "submodule-parity", "version": "1"},
        })
        client.send({"method": "notifications/initialized"})
        args = {"full_rebuild": True, "postprocess": "none"}
        if recurse is not None:
            args["recurse_submodules"] = recurse
        answer = client.call("tools/call", {"name": "build_or_update_graph_tool", "arguments": args})
        result = answer["response"]["result"]
        assert not result.get("isError"), result
        payload = result["structuredContent"]
        assert payload["status"] == "ok" and json.loads(result["content"][0]["text"]) == payload
    finally:
        client.close()
    with sqlite3.connect(root / ".code-review-graph/graph.db") as db:
        symbols = db.execute("SELECT file_path,name,line_start,line_end FROM nodes ORDER BY 1,2,3,4").fetchall()
        edges = db.execute("SELECT kind,source_qualified,target_qualified FROM edges ORDER BY 1,2,3").fetchall()
    return (payload["files_parsed"], symbols, edges)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    source = args.upstream.resolve()
    pinned_reference(source)
    with tempfile.TemporaryDirectory(prefix="crg-submodule-") as directory:
        base = Path(directory).resolve()
        child, parent = base / "child", base / "parent"
        new_repo(child, {"café.py": "def nested():\n    return 1\n"})
        new_repo(parent, {"main.py": "def main():\n    return 0\n"})
        git(parent, "-c", "protocol.file.allow=always", "submodule", "add", "-q", "--", str(child), "modules/child")
        git(parent, "commit", "-qm", "include test submodule")
        (parent / "modules/child/scratch.py").write_text("def local_only(): pass\n")
        (parent / "untracked.py").write_text("def parent_only(): pass\n")
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PYTHON", "CRG_"))}
        env.update(CRG_HOME=str(base / "state"), PYTHONDONTWRITEBYTECODE="1", PYTHONHASHSEED="0")
        for flag, setting, wanted in [(None, "0", 2), (True, "0", 3), (None, "yes", 3), (False, "yes", 2)]:
            obs = []
            for command in ([sys.executable, "-m", "code_review_graph"], [str(args.binary.resolve())]):
                runtime = dict(env, CRG_RECURSE_SUBMODULES=setting)
                if command[0] == sys.executable:
                    runtime["PYTHONPATH"] = str(source)
                result = build(command, parent, runtime, flag)
                assert result[0] == wanted, (flag, setting, result)
                obs.append(result)
                shutil.rmtree(parent / ".code-review-graph")
            assert obs[0] == obs[1], (flag, setting, obs)
    print("Submodules matched on 4 fresh MCP builds with explicit and inherited settings, tracked Unicode paths and untracked files")


if __name__ == "__main__":
    main()
