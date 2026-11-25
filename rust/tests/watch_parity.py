"""Exercise real filesystem changes, lifecycle and MCP auto-watch against the pinned Python CLI."""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

from differential import new_repo, verify_source

INITIAL = {"service.py": "def old():\n    return 1\n", ".gitignore": ".code-review-graph/\n",
           ".code-review-graphignore": "generated/\n", "existing.py":"def baseline(): pass\n"}
CHANGED = "def leaf():\n    return 1\n\ndef middle():\n    return leaf()\n\ndef entry():\n    return middle()\n"

EXPECTED_GRAPH = {
    "calls": [("entry", "middle"), ("middle", "leaf")],
    "flows": [("entry", "entry"), ("entry", "leaf"), ("entry", "middle")],
    "fts": [("middle",)],
}


def select_rows(db, sql):
    with closing(sqlite3.connect(db.as_uri()+"?mode=ro", uri=True, timeout=1)) as conn:
        return conn.execute(sql).fetchall()


def ready(database, condition, description, process, seconds=18):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        if process.poll() is not None:
            raise AssertionError(f"{description}: early exit {process.returncode}")
        try:
            # The batch holds write.lock through all post-processing commits.
            # The new symbols alone can already be visible to a separate reader.
            if condition() and not (database.parent / "write.lock").read_text().strip():
                return
        except (sqlite3.OperationalError, FileNotFoundError, json.JSONDecodeError):
            pass
        time.sleep(0.1)
    raise AssertionError(f"timeout waiting for {description}")


def run_suite(root, engine, cmd, source):
    root.mkdir()
    env = {k:v for k,v in os.environ.items() if not k.startswith(("CRG_","PYTHON","FASTMCP_"))}
    env.update(HOME=str(root/"home"), CRG_HOME=str(root/"state"), PYTHONPATH=str(source),
               PYTHONDONTWRITEBYTECODE="1", PYTHONFAULTHANDLER="1", FASTMCP_CHECK_FOR_UPDATES="off")
    Path(env["HOME"]).mkdir()
    main, auto, foreign = (root/n for n in ("repo","auto","foreign"))
    for repo in (main, auto, foreign):
        new_repo(repo, INITIAL)
    db = main / ".code-review-graph/graph.db"
    def invoke(*args):
        return subprocess.run([*cmd, *map(str,args)], cwd=main, capture_output=True, env=env, timeout=30)
    def health(repo):
        h = hashlib.sha256(str(repo).encode()).hexdigest()[:16]
        return root/"state/watch-health"/(h+".json")
    def watch_log(repo, args):
        return (root/(repo.name+"-"+args[0]+".log")).open("w+b")
    def start(repo, *args, log):
        return subprocess.Popen([*cmd,*args,"--repo",str(repo)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=log, env=env, cwd=repo)
    def stop(p, sig=None):
        if p.poll() is None:
            if sig:
                p.send_signal(sig)
            elif p.stdin and not p.stdin.closed:
                p.stdin.close()
        try:
            code=p.wait(timeout=8)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait(timeout=5)
            raise
        return code
    def symbols(database):
        return {x for x, in select_rows(database,"SELECT name FROM nodes WHERE kind IN ('Function','Test')")}
    def summary(database):
        queries = {
            "calls":"SELECT a.name,b.name FROM edges e JOIN nodes a ON a.qualified_name=e.source_qualified JOIN nodes b ON b.qualified_name=e.target_qualified WHERE e.kind='CALLS' ORDER BY 1,2",
            "flows":"SELECT f.name, n.name FROM flows f JOIN flow_memberships m ON m.flow_id=f.id JOIN nodes n ON n.id=m.node_id ORDER BY 1,2",
            "fts":"SELECT n.name FROM nodes_fts f JOIN nodes n ON n.id=f.rowid WHERE nodes_fts MATCH 'middle' ORDER BY 1",
        }
        with closing(sqlite3.connect(database.as_uri()+"?mode=ro", uri=True, timeout=1)) as conn:
            conn.execute("BEGIN")
            return {name: conn.execute(sql).fetchall() for name, sql in queries.items()}

    # Foreground watch has no build side effects without an existing graph.
    absent=invoke("watch","--repo",main,"--data-dir",root/"absent")
    assert absent.returncode==1 and not absent.stdout and b"No graph found at " in absent.stderr
    assert not (root/"absent").exists()
    built=invoke("build","--repo",main,"--quiet")
    assert built.returncode==0,(engine,built.stderr)
    shared=invoke("watch","--repo",foreign,"--data-dir",db.parent)
    assert shared.returncode==1 and not shared.stdout and shared.stderr
    assert symbols(db)=={"old","baseline"} and not health(foreign).exists()
    (main/"existing.py").write_text("def created_offline(): pass\n")
    (main/"untouched.py").write_text("def offline_new(): pass\n")
    # Watch repairs stale rows in its initial reconciliation.
    with watch_log(main,["watch"]) as log:
        watcher=start(main,"watch",log=log)
        try:
            ready(db, lambda: json.loads(health(main).read_bytes())["phase"]=="watching" and
                  "created_offline" in symbols(db),"initial reconciliation",watcher)
            assert "offline_new" not in symbols(db)
            (main/"generated").mkdir()
            (main/"generated/ignore.py").write_text("def ignored_by_rule(): pass\n")
            (main/"node_modules").mkdir()
            (main/"node_modules/ignore.py").write_text("def ignored_dependency(): pass\n")
            (main/"service.py").write_text(CHANGED)
            ready(db, lambda: {"leaf","middle","entry"}<=symbols(db),"debounced edit",watcher)
            assert "ignored_by_rule" not in symbols(db) and "ignored_dependency" not in symbols(db)
            graph=summary(db)
            assert graph == EXPECTED_GRAPH, (engine, graph, EXPECTED_GRAPH)
            config=main/".code-review-graphignore"
            config.unlink()
            ready(db, lambda: "ignored_by_rule" in symbols(db),"removed exclusion includes source",watcher)
            config.write_text("generated/\n")
            ready(db, lambda: "ignored_by_rule" not in symbols(db),"new exclusion purges source",watcher)
            example=main/"module/target/example.py"
            example.parent.mkdir(parents=True)
            example.write_text("def nested_output(): pass\n")
            ready(db, lambda: "nested_output" in symbols(db),"ordinary nested source directory",watcher)
            (main/"module/pom.xml").write_text("<project/>")
            ready(db, lambda: "nested_output" not in symbols(db),"manifest excludes newly identified output",watcher)
            config.write_text("generated/\n!module/target\n")
            ready(db, lambda: "nested_output" in symbols(db),"explicit keep overrides inferred output",watcher)
            example.unlink()
            ready(db, lambda: "nested_output" not in symbols(db),"remove nested source",watcher)
            (main/"service.py").rename(main/"renamed.py")
            ready(db, lambda: select_rows(db,"SELECT count(*) FROM nodes WHERE file_path LIKE '%/renamed.py'")[0][0]>1
                  and not select_rows(db,"SELECT 1 FROM nodes WHERE file_path LIKE '%/service.py'"),"rename",watcher)
            (main/"renamed.py").unlink()
            ready(db, lambda: symbols(db)=={"created_offline"},"deletion",watcher)
            assert summary(db) == {"calls": [], "flows": [], "fts": []}, engine
        finally:
            code=stop(watcher,signal.SIGTERM)
            log.seek(0); stderr=log.read()
            if code: print(stderr.decode(errors="replace"),file=sys.stderr)
        assert code==0 and watcher.stdout.read()==b"" and not health(main).exists(),(engine,code,stderr)
        assert b"Watching " in stderr and b"Watch stopped" in stderr
    print(engine+": foreground startup/create/edit/rename/delete, FTS, flows, ignored trees, SIGTERM cleanup passed",flush=True)

    # A graph can be pending after its build exits before post-processing. The watcher updates the graph while leaving explicit build lifecycle state alone.
    with closing(sqlite3.connect(db)) as conn:
        conn.execute("INSERT OR REPLACE INTO metadata VALUES('build_state','postprocess-pending')")
        conn.commit()
    (main/"existing.py").write_text("def recovery_after_interruption(): pass\n")
    with watch_log(main,["recovery"]) as log:
        watcher=start(main,"watch",log=log)
        try:
            ready(db, lambda: health(main).exists() and json.loads(health(main).read_bytes())["phase"]=="watching"
                  and symbols(db)=={"recovery_after_interruption"}, "stale incomplete graph recovery",watcher)
            assert select_rows(db,"SELECT value FROM metadata WHERE key='build_state'")==[("postprocess-pending",)]
        finally:
            code=stop(watcher,signal.SIGINT)
        assert code==0 and watcher.stdout.read()==b"" and not health(main).exists()
    print(engine+": stale incomplete graph recovery and SIGINT cleanup passed",flush=True)

    outside=root/"linked sources"
    outside.mkdir()
    (outside/"mirror.py").write_text("def mirrored(): pass\n")
    (main/"linked").symlink_to(outside,target_is_directory=True)
    (outside/"loop").symlink_to(outside,target_is_directory=True)
    with watch_log(main,["links"]) as log:
        watcher=start(main,"watch","--follow-symlinks",log=log)
        try:
            ready(db, lambda: "mirrored" in symbols(db), "follow external folder",watcher)
            assert select_rows(db,"SELECT file_path FROM nodes WHERE name='mirrored'")==[(str(main/"linked/mirror.py"),)]
            (outside/"mirror.py").write_text("def outside_save(): pass\n")
            ready(db, lambda: "outside_save" in symbols(db) and "mirrored" not in symbols(db),
                  "linked file edit and completed batch",watcher)
            (main/"linked").unlink()
            ready(db, lambda: "outside_save" not in symbols(db), "detached folder cleanup",watcher)
        finally:
            code=stop(watcher,signal.SIGTERM)
        assert code==0 and not watcher.stdout.read() and not health(main).exists()
    print(engine+": opt-in followed folders, external edits, loop rejection and detach cleanup passed",flush=True)
    auto_db=auto/".code-review-graph/graph.db"
    with watch_log(auto,["mcp"]) as log:
        server=start(auto,"serve","--auto-watch",log=log)
        def rpc(request):
            server.stdin.write(json.dumps(request).encode()+b"\n"); server.stdin.flush()
        def read():
            assert select.select([server.stdout],[],[],15)[0], engine+" MCP timed out"
            return json.loads(server.stdout.readline())
        try:
            rpc({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"watch-test","version":"1"}}})
            assert read()["id"]==1
            rpc({"jsonrpc":"2.0","method":"notifications/initialized"})
            ready(auto_db, lambda: symbols(auto_db)==set() and json.loads(health(auto).read_bytes())["phase"]=="watching","auto-watch first build",server)
            (auto/"service.py").write_text(CHANGED)
            ready(auto_db, lambda: "entry" in symbols(auto_db),"auto-watch query-visible edit",server)
            rpc({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"query_graph_tool","arguments":{"pattern":"callers_of","target":"leaf"}}})
            result=read()
            assert result["id"]==2 and not result["result"].get("isError"), (engine, result)
            records = result["result"]["structuredContent"]["results"]
            assert [record["name"] for record in records] == ["middle"], (engine, result)
        finally:
            code=stop(server)
            log.seek(0); stderr=log.read()
        assert code==0 and not health(auto).exists(),(engine,code,stderr)
        leftover=server.stdout.read()
        assert not leftover and b"Traceback" not in stderr, (engine,leftover,stderr)
    print(engine+": stdio MCP startup, lazy initial graph, live callers query and EOF cleanup passed",flush=True)
    return graph


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream",type=Path,required=True)
    p.add_argument("--binary",type=Path,required=True)
    args=p.parse_args()
    source,binary=args.upstream.resolve(),args.binary.resolve()
    verify_source(source)
    with tempfile.TemporaryDirectory(prefix="crg-watch-") as tmp:
        root=Path(tmp).resolve()
        expected=run_suite(root/"python","Python",[sys.executable,"-m","code_review_graph"],source)
        actual=run_suite(root/"rust","Rust",[str(binary)],source)
        assert expected==actual,(expected,actual)
        print("Post-edit calls, flow memberships and full-text rows match pinned Python")


if __name__=="__main__":
    main()
