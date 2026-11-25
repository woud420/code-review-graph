"""Exercise the released ReScript cross-module fixture through both public build tools."""
import argparse
import ast
import os
from pathlib import Path
import sys
import tempfile

from canonical_parity import read_graph
from differential import git, new_repo, verify_source
from mcp_contract import WireClient


def fixtures(upstream):
    tree = ast.parse((upstream / "tests/test_multilang.py").read_text())
    suite = next(node for node in tree.body if getattr(node, "name", None) == "TestRescriptCrossModuleResolver")
    setup = next(node for node in suite.body if getattr(node, "name", None) == "_build")
    return {ast.literal_eval(call.func.value.right): ast.literal_eval(call.args[0])
            for call in ast.walk(setup) if isinstance(call, ast.Call) and isinstance(call.func, ast.Attribute)
            and call.func.attr == "write_text"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-rescript-") as folder:
        temp = Path(folder).resolve()
        root = temp / "repo"
        files = fixtures(upstream)
        assert len(files) == 4
        new_repo(root, files)
        clients = []
        try:
            for i, program in enumerate(([sys.executable, "-m", "code_review_graph"], [str(binary)])):
                env = dict(os.environ, CRG_DATA_DIR=str(temp/str(i)), CRG_HOME=str(temp/"registry"),
                           CRG_SERIAL_PARSE="1", PYTHONDONTWRITEBYTECODE="1", PYTHONHASHSEED="0")
                if not i:
                    env["PYTHONPATH"] = str(upstream)
                client = WireClient([*program, "serve", "--repo", str(root)], root, env)
                clients.append(client)
                client.call("initialize", {"protocolVersion":"2025-11-25","capabilities":{},
                                           "clientInfo":{"name":"rescript-parity","version":"1"}})
                client.send({"method":"notifications/initialized"})
            for label, request in [("raw", {"full_rebuild":True,"postprocess":"none"}),
                                   ("full", {"full_rebuild":True}), ("edit", {}), ("unchanged", {})]:
                if label == "edit":
                    path = root / "Caller.res"
                    path.write_text(path.read_text().replace("safeParse", "trim"))
                    git(root, "commit", "-qam", "edit calls in the open module")
                results = []
                for i, client in enumerate(clients):
                    reply = client.call("tools/call", {"name":"build_or_update_graph_tool","arguments":request})["response"]["result"]
                    assert not reply.get("isError"), reply
                    results.append((reply["structuredContent"]["rescript_resolution"],
                                    read_graph(temp/str(i)/"graph.db", root)))
                stats, expected = results[0]
                assert results[1][0] == stats, (label, stats, results[1][0])
                if label != "unchanged":
                    assert stats["files_indexed"] == 4 and stats["calls_resolved"] >= 3 and stats["imports_resolved"] >= 2, stats
                for key, rows in expected.items():
                    actual = results[1][1][key]
                    assert actual == rows, (label,key,(rows-actual).most_common(3),(actual-rows).most_common(3))
                print(f"{label}: ReScript module stats {stats}, all nine graph projections matched")
        finally:
            for client in clients:
                client.close()


if __name__ == "__main__":
    main()
