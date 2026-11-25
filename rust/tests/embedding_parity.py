"""Compare native and pinned Python embeddings, hybrid search and stored vectors on loopback."""
import argparse
import fcntl
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import threading

from mcp_contract import WireClient
from public_parity import pinned_reference, run


class Fixture(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.headers.get("Authorization") == "Bearer fixture-key"
        assert self.path == "/fake/v1/embeddings"
        self.server.requests.append(body)
        data = []
        for index, item in enumerate(body["input"]):
            digest = hashlib.sha256(item.encode()).digest()
            data.append({"index": index, "embedding": [byte / 16 for byte in digest[:4]]})
        data.reverse()
        if body["model"] == "malformed-permutation":
            for item in data:
                item["index"] = 50
        elif body["model"] == "mixed-index":
            del data[0]["index"]
        elif body["model"] == "without-index":
            for item in data:
                del item["index"]

        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"data": data}).encode())

    def log_message(self, *args):
        pass


def snapshot(path):
    with sqlite3.connect(path) as db:
        return db.execute("SELECT qualified_name,hex(vector),text_hash,provider FROM embeddings ORDER BY 1").fetchall()


def scenario(command, root, env, http, pristine):
    db = root / ".code-review-graph/graph.db"
    with sqlite3.connect(db) as dest:
        pristine.backup(dest)
    facts = {}
    def cli(name, args, config=None):
        facts[name] = run([*command, *args, "--repo", str(root)], root, {**env, **(config or {})})
    def protocol(config, cases):
        client = WireClient([*command, "serve", "--repo", str(root)], root, dict(env, **config))
        try:
            client.call("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                       "clientInfo": {"name": "embedding-oracle", "version": "1"}})
            client.send({"method": "notifications/initialized"})
            for label, tool, args in cases:
                facts[label] = client.call("tools/call", {"name":tool, "arguments":args})
        finally:
            client.close()
    url = f"http://127.0.0.1:{http.server_port}/fake/v1"
    ai = dict(CRG_OPENAI_API_KEY="fixture-key", CRG_OPENAI_BASE_URL=url, CRG_OPENAI_MODEL="fixture",
              CRG_OPENAI_BATCH_SIZE="2", CRG_OPENAI_DIMENSION="4")
    voyage = dict(VOYAGE_API_KEY="fixture-key", CRG_VOYAGE_BASE_URL=url, CRG_VOYAGE_MODEL="fixture-v",
                  CRG_VOYAGE_BATCH_SIZE="2", CRG_VOYAGE_OUTPUT_DIMENSION="4")
    with (db.parent / "write.lock").open("w") as lock:
        lock.write("2468")
        lock.flush()
        fcntl.flock(lock, fcntl.LOCK_EX)
        cli("contended", ["embed"], ai)
        protocol(ai, [("contended-wire","embed_graph_tool",{"provider":"openai"})])
    cli("local", ["embed"])
    for provider in ("openai", "google", "minimax", "voyage"):
        cli("missing-" + provider, ["embed","--provider",provider])
    cli("google-without-optional-sdk", ["embed","--provider","google"],
        {"GOOGLE_API_KEY":"fixture-key","GOOGLE_GEMINI_BASE_URL":url})
    assert not http.requests
    protocol({}, [("invalid-provider","embed_graph_tool",{"provider":"not-real"})])
    cli("index", ["embed"], ai)
    facts["rows"] = snapshot(db)
    assert len(facts["rows"]) >= 3
    assert len(http.requests) >= 2
    cli("again", ["embed","--provider","openai"], ai)
    protocol(ai, [("cached","embed_graph_tool",{}), ("configured-stats","list_graph_stats_tool",{}), ("fts-and-vector","semantic_search_nodes_tool",{"query":"ChargeInvoice"}),
                  ("vector-only","semantic_search_nodes_tool",{"query":"unrelated query","limit":2}),
                  ("vector-class-filter","semantic_search_nodes_tool",{"query":"unrelated query","kind":"Class"})])
    for case,mode in (("fts-and-vector","hybrid"),("vector-only","semantic"),("vector-class-filter","semantic")):
        assert facts[case]["response"]["result"]["structuredContent"]["search_mode"] == mode, facts[case]
    with sqlite3.connect(db) as conn:
        conn.execute("INSERT INTO embeddings SELECT 'deleted',vector,text_hash,provider FROM embeddings LIMIT 1")
        conn.execute("UPDATE nodes SET extra=json_set(extra,'$.docstring','Updated documentation') WHERE name='ChargeInvoice'")
    cli("one-changed-plus-orphan", ["embed"], ai)
    facts["updated"] = snapshot(db)
    for model in ("malformed-permutation", "mixed-index"):
        protocol(ai, [(model,"embed_graph_tool",{"provider":"openai","model":model})])
        error = facts[model]["response"]["result"]
        assert error["isError"] and "refusing to misalign vectors" in error["content"][0]["text"], error
        assert snapshot(db) == facts["updated"]

    with sqlite3.connect(db) as conn:
        conn.execute("INSERT INTO embeddings SELECT 'orphan',vector,text_hash,provider FROM embeddings LIMIT 1")
        conn.execute("UPDATE nodes SET extra=json_set(extra,'$.docstring','New signature description') WHERE name='ChargeInvoice'")
    post = dict(flows=False,communities=False,fts=False,embedding_provider="openai",embedding_model="fixture")
    protocol(ai,[("refresh","run_postprocess_tool",post),
                 ("refresh-model-mismatch","run_postprocess_tool",dict(post,embedding_model="wrong-model"))])
    refreshed = facts["refresh"]["response"]["result"]["structuredContent"]
    assert refreshed["embeddings_purged"] == 1 and refreshed["embeddings_refreshed"] == 1, refreshed
    assert "Embedding refresh refused" in facts["refresh-model-mismatch"]["response"]["result"]["structuredContent"]["warnings"][0]
    facts["refreshed-rows"] = snapshot(db)

    protocol(voyage, [("voyage","embed_graph_tool",{"provider":"voyage"}),
                      ("voyage-query","semantic_search_nodes_tool",{"query":"total","provider":"voyage"})])
    facts["voyage-rows"] = snapshot(db)
    cli("server-order-without-index", ["embed","--provider","openai","--model","without-index"], ai)
    facts["unindexed-rows"] = snapshot(db)

    with sqlite3.connect(db) as conn:
        conn.execute("ALTER TABLE embeddings DROP COLUMN provider")
    cli("legacy-schema-migration", ["embed","--provider","openai"], ai)
    facts["legacy-rows"] = snapshot(db)
    code = root / "core.py"
    original = code.read_bytes()
    flags = ["--skip-postprocess","--embedding-provider","openai","--embedding-model","fixture"]
    try:
        code.write_bytes(original + b"\ndef addedEntry():\n    return 123\n")
        cli("full-refresh", ["build","--quiet", *flags], ai)
        facts["full-refresh-rows"] = snapshot(db)
        assert facts["full-refresh"]["exit"] == 0 and len(facts["full-refresh-rows"]) == 4, facts["full-refresh"]
        code.write_bytes(original + b"\ndef anotherEntry():\n    return 456\n")
        cli("updated-refresh", ["update","--quiet", *flags], ai)
        facts["updated-refresh-rows"] = snapshot(db)
        assert facts["updated-refresh"]["exit"] == 0 and len(facts["updated-refresh-rows"]) == 4, facts["updated-refresh"]
    finally:
        code.write_bytes(original)


    facts["requests"] = list(http.requests)
    http.requests.clear()
    return facts


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary",type=Path,required=True)
    p.add_argument("--upstream",type=Path,required=True)
    args = p.parse_args()
    binary,upstream = args.binary.resolve(),args.upstream.resolve()
    pinned_reference(upstream)
    env = {k:v for k,v in os.environ.items() if not k.startswith(("PYTHON","CRG_","FASTMCP_","VOYAGE_","MINIMAX_","GOOGLE_","OPENAI_"))}
    with tempfile.TemporaryDirectory(prefix="crg-embedding-") as folder, ThreadingHTTPServer(("127.0.0.1",0),Fixture) as http:
        root = Path(folder).resolve() / "repo"
        root.mkdir()
        (root / "core.py").write_text("class ChargeInvoice:\n    def invoice_total(self):\n        \"\"\"Return remaining total.\"\"\"\n        return 1\n\ndef itemPrice():\n    return 2\n")
        env.update(CRG_HOME=str(root.parent/"registry"), NO_COLOR="1", TZ="UTC", NO_PROXY="127.0.0.1,localhost")
        refenv = dict(env,PYTHONPATH=str(upstream),PYTHONDONTWRITEBYTECODE="1")
        py = [sys.executable,"-m","code_review_graph"]
        seeded = run([*py,"build","--repo",str(root)],root,refenv)
        assert seeded["exit"] == 0, seeded
        http.requests = []
        worker = threading.Thread(target=http.serve_forever,daemon=True)
        worker.start()
        try:
            with sqlite3.connect(":memory:") as copy, sqlite3.connect(root/".code-review-graph/graph.db") as conn:
                conn.execute("UPDATE metadata SET value='2099-01-01T00:00:00+00:00' WHERE key='last_updated'")
                conn.commit()
                conn.backup(copy)
                expected = scenario(py,root,refenv,http,copy)
                actual = scenario([str(binary)],root,env,http,copy)
            for k in expected:
                assert actual[k] == expected[k], (k,expected[k],actual[k])
            print(f"{len(expected)} Python/native embedding observations matched, including {len(actual['requests'])} provider requests")
        finally:
            http.shutdown()
            worker.join()


if __name__ == "__main__":
    main()
