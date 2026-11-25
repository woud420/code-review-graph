"""Compare explicit post-processing, registry state, and opt-in read-only writes."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

from canonical_parity import read_graph
from differential import new_repo
from mcp_contract import WireClient
from public_parity import FILES, pinned_reference, run


def exercise(binary, env, root, data, registry):
    cases = {}
    database = data / "graph.db"
    env = dict(env, CRG_HOME=str(registry))
    def cli(name, *argv, expected=0, **override):
        result = run([*binary, *argv], root, env | override)
        assert result["exit"] == expected, (name, result)
        cases[name] = result

    cli("registry empty", "repos")
    cli("registry explicit", "register", str(root), "--alias", "examined")
    cli("registry duplicate", "register", str(root), "--alias", "examined")
    cli("external build", "build", "--quiet", "--skip-postprocess", "--repo", str(root), "--data-dir", str(data))
    cli("registry data dir", "repos")
    assert str(data) in cases["registry data dir"]["stdout"]
    with sqlite3.connect(database) as db:
        db.execute("UPDATE metadata SET value=? WHERE key='last_updated'", ("2099-01-01T00:00:00+00:00",))
    # status returns the last real write time. Recheck it on a frozen input.
    cli("registered status", "status", "--json", "--repo", str(root))
    snapshot = database.read_bytes()
    protected = run([*binary,"update","--repo",str(root)],root,env|{"CRG_READ_ONLY":"1"})
    assert protected["exit"]==1 and "attempt to write a readonly database" in protected["stderr"]
    assert database.read_bytes()==snapshot
    cases["read-only update"] = (protected["exit"],"attempt to write a readonly database")
    cli("readonly missing", "postprocess", "--repo", str(root/"missing"), expected=1, CRG_READ_ONLY="1")
    for flags in (["--no-communities","--no-fts"], ["--no-flows"], ["--no-communities","--no-fts","--no-flows"], [], ["--embedding-model","test","--embedding-provider","local"]):
        cli("stage " + " ".join(flags), "postprocess", "--repo", str(root), *flags)
    cli("incomplete flags", "postprocess", "--embedding-provider", "local", expected=2)
    cases["graph"] = read_graph(database,root)
    for stage, args in [("create",[]),("repeat",[]),("force",["--force"])]:
        cli("wiki "+stage, "wiki", "--repo", str(root), *args)
        files=sorted((data/"wiki").glob("*.md"))
        assert len(files)>1
        cases["wiki content "+stage]={p.name:p.read_text() for p in files}
    for state in ("postprocess-pending","in-progress"):
        with sqlite3.connect(database) as db:
            db.execute("INSERT OR REPLACE INTO metadata VALUES('build_state',?)", (state,))
        cli(state, "postprocess", "--repo", str(root))
        with sqlite3.connect(database) as db:
            cases[state+" marker"] = db.execute("SELECT value FROM metadata WHERE key='build_state'").fetchone()
    with (data/"write.lock").open("w+b") as handle:
        handle.write(b"12345")
        handle.flush()
        fcntl.flock(handle.fileno(),fcntl.LOCK_EX)
        cli("overlap","postprocess","--repo",str(root),expected=75)
    wire = WireClient([*binary,"mcp","--repo",str(root)],root,env)
    try:
        wire.call("initialize",{"protocolVersion":"2025-11-25","clientInfo":{"name":"standalone-test","version":"1"},"capabilities":{}})
        wire.send({"method":"notifications/initialized"})
        for tool, args in [
            ("run_postprocess_tool", {"flows":False,"fts":False}),
            ("generate_wiki_tool", {"force":True}),
            ("cross_repo_search_tool", {"query":"target"}),
            ("semantic_search_nodes_tool", {"query":"target"}),
        ]:
            packet = wire.call("tools/call",{"name":tool,"arguments":args})["response"]["result"]
            assert "structuredContent" in packet and not packet.get("isError"), (tool,packet)
            cases[tool] = packet
    finally:
        wire.close()
    for mode, extra, flags in [("env",{"CRG_TOOLS":"list_repos_tool"},[]),
                               ("flag wins",{"CRG_TOOLS":"query_graph_tool"},["--tools"," list_repos_tool,unknown "])]:
        wire = WireClient([*binary,"serve","--repo",str(root),*flags],root,env|extra)
        try:
            wire.call("initialize",{"protocolVersion":"2025-11-25","clientInfo":{"name":"filtered","version":"1"},"capabilities":{}})
            wire.send({"method":"notifications/initialized"})
            names=[tool["name"] for tool in wire.call("tools/list",{})["response"]["result"]["tools"]]
            assert names==["list_repos_tool"],names
            cases[mode+" rejected tool"]=wire.call("tools/call",{"name":"query_graph_tool","arguments":{}})["response"]["result"]
            assert cases[mode+" rejected tool"]["isError"]
            cases[mode+" allowed tool"]=wire.call("tools/call",{"name":"list_repos_tool","arguments":{}})["response"]["result"]
            assert "structuredContent" in cases[mode+" allowed tool"]
        finally:
            wire.close()
    cli("unregister", "unregister", "examined")
    cli("unregister missing", "unregister", "examined",expected=1)
    cli("registry cleared","repos")
    return cases


def against_reference(temp, source, native):
    env={k:v for k,v in os.environ.items() if not k.startswith(("CRG_","PYTHON","FASTMCP_"))}
    env.update(PYTHONDONTWRITEBYTECODE="1",PYTHONHASHSEED="0",CRG_SERIAL_PARSE="1")
    root,data,registry=temp/"repo",temp/"external",temp/"registry"
    new_repo(root,FILES | {"pipeline.py": "def parse(): return 1\ndef read(): return parse()\ndef process(): return read()\ndef main(): return process()\n"})
    want=exercise([sys.executable,"-m","code_review_graph"],env|{"PYTHONPATH":str(source)},root,data,registry)
    shutil.rmtree(data)
    shutil.rmtree(registry)
    got=exercise([str(native)],env,root,data,registry)
    return want,got


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream",required=True,type=Path)
    parser.add_argument("--binary",required=True,type=Path)
    args=parser.parse_args()
    source,native=args.upstream.resolve(),args.binary.resolve()
    pinned_reference(source)
    with tempfile.TemporaryDirectory(prefix="crg-standalone-") as folder:
        want, got = against_reference(Path(folder).resolve(),source,native)
        for key in want:
            assert want[key]==got[key], (key,want[key],got[key])
        print(f"{len(want)} exact standalone processing, shared registry, and MCP observations")
if __name__=="__main__":
    main()
