"""Drive the release's parallel multi-worktree test and compare public routing errors and state."""
import argparse
import asyncio
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import sqlite3
import sys
import tempfile

from mcp_contract import WireClient
from public_parity import pinned_reference, run


def load(path, digest):
    assert hashlib.sha256(path.read_bytes()).hexdigest() == digest
    spec = importlib.util.spec_from_file_location("tests." + path.stem, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def probe(command, root, sibling, foreign, home, env):
    register = home / "registry.json"
    register.write_text('{"repos":[]}')
    client = WireClient([*command, "serve", "--multi-worktree", "--repo", str(root),
                         "--tools", "get_docs_section_tool"], root, env)
    observed = {}
    def docs(where):
        params = {"section_name": "usage", "repo_root": where}
        return client.call("tools/call", {"name": "get_docs_section_tool","arguments": params})
    def message(reply):
        return reply["response"]["result"]["structuredContent"]
    try:
        observed["init"] = client.call("initialize", {
            "protocolVersion":"2025-11-25", "capabilities":{},
            "clientInfo":{"name":"worktree-test","version":"1"}})
        client.send({"method":"notifications/initialized"})
        for key,line in [("malformed", "{invalid"), ("non-object", "[]"), ("oversized", "x"*(4*1024*1024+1))]:
            client.child.stdin.write(line + "\n")
            client.child.stdin.flush()
            observed[key]=json.loads(client.messages.get(timeout=15))
            assert observed[key]["id"] is None and observed[key]["error"]["code"] in (-32700,-32600)
        observed["tools"] = client.call("tools/list",{})
        assert [v["name"] for v in observed["tools"]["response"]["result"]["tools"]] == ["get_docs_section_tool"]
        for route, value in (("relative","sub"),("missing",str(foreign/"missing")),
                             ("foreign",str(foreign)),("empty",""),("number",7),("nul","/tmp/\0test")):
            observed[route] = docs(value)
            assert observed[route]["response"]["error"]["code"] == -32602
        for route, value in (("default",None), ("primary",str(root/"docs")),
                             ("sibling",str(sibling)), ("subdir",str(sibling/"docs"))):
            observed[route] = docs(value)
            assert message(observed[route])["content"] == ("branch local" if route in {"sibling","subdir"} else "primary local")
        register.write_text(json.dumps({"repos":[{"path":str(sibling),"data_dir":str(home/"moved")}]}))
        observed["moved"] = docs(str(sibling))
        assert observed["moved"]["response"]["error"]["code"] == -32001
        observed["survives"] = docs(str(root))
    finally:
        client.close()
    register.write_text(json.dumps({"repos":[{"path":str(p),"data_dir":str(home/"shared")} for p in (root,sibling)]}))
    client = WireClient([*command,"mcp","--multi-worktree","--repo",str(root)],root,env)
    try:
        observed["alias"] = client.call("initialize",{
            "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"worktree-test","version":"1"}})
        client.send({"method":"notifications/initialized"})
        observed["collision"] = docs(str(sibling))
        assert "worktrees must not share" in observed["collision"]["response"]["error"]["message"]
    finally:
        client.close()
    return observed


def cancellation(command, root, sibling, env):
    (Path(env["CRG_HOME"])/"registry.json").write_text('{"repos":[]}')
    session=WireClient([*command,"serve","--multi-worktree","--repo",str(root)],root,env)
    db=sibling/".code-review-graph/graph.db"
    db.parent.mkdir(exist_ok=True)
    try:
        session.call("initialize",{"protocolVersion":"2025-11-25","capabilities":{},
                     "clientInfo":{"name":"concurrent-test","version":"1"}})
        session.send({"method":"notifications/initialized"})
        def request(tree):
            return {"name":"get_docs_section_tool","arguments":{"repo_root":str(tree),"section_name":"usage"}}
        assert session.call("tools/call",request(sibling))["response"]["result"]["structuredContent"]["content"]=="branch local"
        with sqlite3.connect(db) as guard:
            guard.execute("pragma journal_mode=delete")
            guard.execute("create table if not exists parity_lock(x)")
            guard.execute("begin exclusive")
            blocked={"method":"tools/call","id":751,"params":{"name":"list_graph_stats_tool",
                "arguments":{"repo_root":str(sibling)}}}
            session.send(blocked)
            session.send({"method":"tools/call","id":751,"params":request(root)})
            duplicate=json.loads(session.messages.get(timeout=15))
            assert duplicate["id"]==751 and duplicate.get("error",{}).get("message")=="duplicate in-flight JSON-RPC request id",duplicate
            session.send({"method":"notifications/cancelled","params":{"requestId":751}})
            # Cancellation releases the original id even while the child waits on another repository's lock.
            session.send({"method":"tools/call","id":751,"params":request(root)})
            reused=json.loads(session.messages.get(timeout=15))
            assert reused["id"]==751 and reused["result"]["structuredContent"]["content"]=="primary local",reused
        return duplicate, reused
    finally:
        session.close()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream",type=Path,required=True)
    parser.add_argument("--binary",type=Path,required=True)
    args=parser.parse_args()
    src,binary=args.upstream.resolve(),args.binary.resolve()
    pinned_reference(src)
    sys.path.insert(0,str(src))
    upstream = load(src/"tests/test_e2e_mcp_client.py","22c3cd400f8e1523f5227f6b01127ef9517f4d60b815dc645b19f658eb3c67e4")
    cases = load(src/"tests/test_multi_worktree.py","c9ccadabc3ac231d262133810f0fa1be2a47cd2a67355d321845cff27b272e65")
    ref = [sys.executable,"-B","-m","code_review_graph"]
    with tempfile.TemporaryDirectory(prefix="crg-worktree-") as name:
        base=Path(name).resolve()
        home=base/"home"
        home.mkdir()
        env={k:v for k,v in os.environ.items() if not k.startswith(("GIT_","PYTHON","CRG_","FASTMCP_"))}
        env.update(CRG_HOME=str(home),HOME=str(home),CODEX_HOME=str(home/".codex"),
                   GIT_CONFIG_GLOBAL=os.devnull,GIT_CONFIG_NOSYSTEM="1",PYTHONDONTWRITEBYTECODE="1",FASTMCP_LOG_LEVEL="ERROR")
        os.environ.update({k:env[k] for k in ("HOME","CODEX_HOME","CRG_HOME","GIT_CONFIG_GLOBAL","GIT_CONFIG_NOSYSTEM")})
        official=upstream._server_params
        for runtime in ("python","rust"):
            if runtime=="rust":
                def params(*args, **kwargs):
                    original=official(*args,**kwargs)
                    return original.model_copy(update={"command":str(binary),"args":original.args[2:]})
                upstream._server_params=params
            asyncio.run(upstream.test_mcp_multi_worktree_routes_parallel_graphs(base/runtime))
        repo=cases._repository(base/"smoke")
        sibling=base/"secondary"
        cases._git(repo,"worktree","add","-q","-b","doctor-review",str(sibling))
        foreign=cases._repository(base/"elsewhere")
        for directory, section in [(repo,"primary local"),(sibling,"branch local")]:
            (directory/"docs").mkdir()
            (directory/"docs/LLM-OPTIMIZED-REFERENCE.md").write_text('<section name="usage">'+section+'</section>')
        ref_env=dict(env,PYTHONPATH=str(src))
        expected=probe(ref,repo,sibling,foreign,home,ref_env)
        got=probe([str(binary)],repo,sibling,foreign,home,env)
        differences = [(k,expected[k],got[k]) for k in expected if expected[k] != got[k]]
        assert not differences, differences
        assert cancellation(ref,repo,sibling,ref_env)==cancellation([str(binary)],repo,sibling,env)
        for command in ("serve","mcp"):
            cases_argv=[([command],{}),([command,"--multi-worktree","--repo","relative"],{}),
                        ([command,"--multi-worktree"],{"CRG_DATA_DIR":str(home)})]
            if command=="serve":
                cases_argv.extend([(["serve","--http","--multi-worktree"],{}),
                                   (["serve","--http","--multi-worktree","--multi-repo"],{})])
            for argv, override in cases_argv:
                actual=run([str(binary),*argv],repo,dict(env,**override),stdin="")
                expected_cli=run([*ref,*argv],repo,dict(ref_env,**override),stdin="")
                if argv[0]=="mcp" and "--multi-worktree" in argv:
                    # The pinned Python CLI's missing alias in _SUBCOMMANDS masks its
                    # original diagnostic with KeyError. Check both exits and the honest native cause.
                    assert expected_cli["exit"]==actual["exit"]==1
                    assert expected_cli["stderr"].rstrip().endswith("KeyError: 'mcp'")
                    assert ("CRG_DATA_DIR" if override else "not a directory") in actual["stderr"]
                    assert not actual["stdout"] and not expected_cli["stdout"]
                else:
                    assert actual==expected_cli, (argv,expected_cli,actual)
            bad=run([str(binary),command],repo,env,stdin="invalid transport input\n")
            assert bad["exit"] != 0, bad
        for cli,environment in [(ref,ref_env),([str(binary)],env)]:
            session=WireClient([*cli,"serve","--multi-worktree","--repo",str(repo)],repo,environment)
            try:
                session.call("initialize",{"protocolVersion":"2025-11-25","capabilities":{},
                             "clientInfo":{"name":"signal-test","version":"1"}})
                session.child.send_signal(signal.SIGTERM)
                assert session.child.wait(timeout=15) == 128 + signal.SIGTERM
            finally:
                session.close()
        print(f"Both released parallel-worktree E2Es passed; {len(expected)} wire comparisons, six exact CLI outputs, two alias error exits, cancellation during a live write lock, malformed handshakes and SIGTERM verified.")


if __name__=="__main__":
    main()
