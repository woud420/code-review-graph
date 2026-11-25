"""Compare the two servers building fresh databases, including measured-time contracts."""
import argparse
from datetime import datetime
import json
import math
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

from differential import new_repo, git
from mcp_contract import WireClient
from public_parity import FILES, pinned_reference


def run(server, repo, env):
    client = WireClient(server+["serve","--repo",str(repo)],repo,env)
    results={}
    try:
        client.call("initialize", {"protocolVersion":"2025-11-25","capabilities":{},
                                  "clientInfo":{"name":"build-parity","version":"1"}})
        client.send({"method":"notifications/initialized"})
        for name, args in [("raw build",{"full_rebuild":True,"postprocess":"none"}),
                           ("raw no-op",{"postprocess":"none"}),("full build",{"full_rebuild":True}),
                           ("minimal no-op",{}),("changed",{"postprocess":"minimal"})]:
            if name=="changed":
                (repo/"lib.ts").write_text(FILES["lib.ts"].replace("target","renamed"))
                git(repo,"commit","-qam","rename")
            answer=client.call("tools/call",{"name":"build_or_update_graph_tool","arguments":args})["response"]["result"]
            if answer.get("isError") or "structuredContent" not in answer:
                raise AssertionError(f"{name}: {answer}")
            value=answer["structuredContent"]
            assert json.loads(answer["content"][0]["text"])==value
            timing=value.get("postprocess_timing",{})
            for field,duration in timing.items():
                assert isinstance(duration,(float,int)) and math.isfinite(duration) and duration>=0
                timing[field]="measured duration"
            graph=value["_graph"]
            datetime.fromisoformat(graph["updated_at"])
            assert graph["age_seconds"]>=0
            with sqlite3.connect(repo/".code-review-graph/graph.db") as db:
                assert graph["updated_at"]==db.execute("SELECT value FROM metadata WHERE key='last_updated'").fetchone()[0]
            graph["updated_at"]="verified database write time"
            graph["age_seconds"]="measured age"
            results[name]=value
    finally:
        client.close()
    return results


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream",type=Path,required=True)
    parser.add_argument("--binary",type=Path,required=True)
    args=parser.parse_args()
    os.environ.update(GIT_AUTHOR_DATE="2000-01-01T00:00:00Z",GIT_COMMITTER_DATE="2000-01-01T00:00:00Z",GIT_CONFIG_GLOBAL=os.devnull,GIT_CONFIG_NOSYSTEM="1")
    source=args.upstream.resolve()
    pinned_reference(source)
    with tempfile.TemporaryDirectory(prefix="crg-build-tool-") as tmp:
        tmp=Path(tmp).resolve()
        root=tmp/"fixture"
        new_repo(root,FILES)
        shutil.copytree(root,tmp/"pristine")
        env={k:v for k,v in os.environ.items() if not k.startswith(("CRG_","PYTHON"))}
        env.update(CRG_SERIAL_PARSE="1",CRG_HOME=str(tmp/"registry"),PYTHONDONTWRITEBYTECODE="1",
                   GIT_AUTHOR_DATE="2000-01-01T00:00:00Z",GIT_COMMITTER_DATE="2000-01-01T00:00:00Z")
        expected=run([sys.executable,"-m","code_review_graph"],root,dict(env,PYTHONPATH=str(source)))
        shutil.rmtree(root)
        shutil.copytree(tmp/"pristine", root)
        actual=run([str(args.binary.resolve())],root,env)
        for case in expected:
            wanted,got=expected[case],actual[case]
            if wanted!=got:
                delta={k:(wanted.get(k),got.get(k)) for k in wanted.keys()|got.keys() if k not in wanted or k not in got or got[k]!=wanted[k]}
                raise AssertionError(f"{case}: {delta}")
    print(f"{len(expected)} real MCP builds, updates and no-ops matched; durations and write times validated separately")
if __name__=="__main__":
    main()
