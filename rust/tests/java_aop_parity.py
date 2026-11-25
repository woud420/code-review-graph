"""Compare persisted Spring AOP calls and build-tool resolver counts on the release's Java sources."""
import argparse
import ast
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile

from canonical_parity import read_graph
from differential import git, new_repo, verify_source
from mcp_contract import WireClient


def sources(upstream):
    tests = ast.parse((upstream / "tests/test_aop_resolver.py").read_text())
    return {node.targets[0].id.removesuffix("_SOURCE") + ".java": ast.literal_eval(node.value)
            for node in tests.body if isinstance(node, ast.Assign) and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name) and node.targets[0].id.endswith("_SOURCE")}


def requests(client, args):
    response = client.call("tools/call", {"name": "build_or_update_graph_tool", "arguments": args})["response"]["result"]
    assert not response.get("isError") and "aop_resolution" in response["structuredContent"], response
    return response["structuredContent"]["aop_resolution"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-spring-aop-") as tmp:
        base = Path(tmp).resolve()
        repo = base / "repo"
        fixtures = sources(upstream)
        new_repo(repo, fixtures)
        servers = ([sys.executable, "-m", "code_review_graph"], [str(binary)])
        env = os.environ | {"CRG_HOME": str(base / "registry"), "CRG_PARSE_EXECUTOR":"serial", "PYTHONDONTWRITEBYTECODE":"1",
                            "PYTHONPATH": str(upstream)}
        clients=[]
        for i, server in enumerate(servers):
            client = WireClient([*server, "serve", "--repo", str(repo)], repo, env | {"CRG_DATA_DIR":str(base / str(i))})
            client.call("initialize", {"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"aop-parity","version":"1"}})
            client.send({"method":"notifications/initialized"})
            clients.append(client)
        try:
            for phase, request in (("raw", {"full_rebuild": True, "postprocess":"none"}),
                                   ("full", {"full_rebuild": True}), ("edit", {}), ("delete", {})):
                if phase=="edit":
                    path=repo/"ANNOTATION.java"
                    path.write_text(path.read_text().replace("@AccessLimit\n","@Other\n"))
                    git(repo,"commit","-qam","remove annotation from the advice target")
                if phase=="delete":
                    (repo/"ANNOTATION_ASPECT_ONLY.java").unlink()
                    git(repo,"commit","-qam","delete one aspect")
                reference, native=(requests(c,request) for c in clients)
                assert native==reference,(phase,reference,native)
                assert reference["aspects_indexed"]>2 and reference["advice_resolved"]>=2 and reference["calls_created"]>3
                if phase=="edit": assert reference["stale_calls_removed"]>0
                dbs=[base/str(i)/"graph.db" for i in range(2)]
                samples=[]
                for db in dbs:
                    with sqlite3.connect(db) as c:
                        rows=c.execute("select source_qualified,target_qualified,extra from edges where kind='CALLS' and json_extract(extra,'$.aop_resolved') order by source_qualified,target_qualified").fetchall()
                        assert rows and {json.loads(extra)["pointcut_kind"] for _,_,extra in rows}=={"@annotation","execution"}
                        pair="ANNOTATION.java::AccessLimitAspect.beforeLimit","ANNOTATION.java::OrderController.placeOrder"
                        assert any(s.endswith(pair[0]) and t.endswith(pair[1]) for s,t,_ in rows) == (phase in ("raw","full"))
                    samples.append(read_graph(db,repo))
                for key,expected in samples[0].items():
                    actual=samples[1][key]
                    assert expected==actual,(phase,key,(expected-actual).most_common(2),(actual-expected).most_common(2))
                print(f"{phase}: {len(fixtures)} upstream Java sources, {len(rows)} derived edges and matching AOP counters")
        finally:
            for c in clients: c.close()


if __name__=="__main__":
    main()
