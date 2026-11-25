"""Compare the optional native Gemini protocol and results with the pinned SDK against loopback."""

import argparse
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import threading

from embedding_parity import snapshot
from public_parity import pinned_reference, run
from mcp_contract import WireClient


class Gemini(BaseHTTPRequestHandler):
    def do_POST(self):
        assert (
            self.headers.get("x-goog-api-key") == "test-local-key"
            and self.headers.get("authorization") is None
        )
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.requests.append([self.path, body])
        assert self.path.startswith("/fixture/v1beta/models/") and self.path.endswith(
            ":batchEmbedContents"
        )
        data = body["requests"]
        if "bad-request" in self.path:
            code, reply = (
                400,
                {
                    "error": {
                        "code": 400,
                        "message": "Unexpected fixture model.",
                        "status": "INVALID_ARGUMENT",
                    }
                },
            )
        elif "retry-once" in self.path and self.server.retry:
            self.server.retry = False
            code, reply = (
                503,
                {
                    "error": {
                        "code": 503,
                        "message": "Fixture busy.",
                        "status": "UNAVAILABLE",
                    }
                },
            )
        else:
            code, reply = (
                200,
                {
                    "embeddings": [
                        {
                            "values": [
                                1.0,
                                len(r["content"]["parts"][0]["text"]) * 0.01,
                                0.5,
                            ]
                        }
                        for r in data
                    ]
                },
            )
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(reply).encode())

    def log_message(self, *args):
        pass


def scenario(command, root, env, server, seed):
    db = root / ".code-review-graph/graph.db"
    with sqlite3.connect(db) as dest:
        seed.backup(dest)
    outcomes = {}

    def cli(args):
        return run([*command, *args, "--repo", str(root)], root, env)

    outcomes["local"] = cli(["embed"])
    assert not server.requests
    client = WireClient([*command, "serve", "--repo", str(root)], root, env)
    try:
        client.call(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "google-embedding-parity", "version": "1"},
            },
        )
        client.send({"method": "notifications/initialized"})

        def tool(label, name, **args):
            outcomes[label] = client.call(
                "tools/call", {"name": name, "arguments": args}
            )
            return outcomes[label]["response"]["result"]

        def embed(label, **opts):
            return tool(label, "embed_graph_tool", provider="google", **opts)

        res = embed("index")["structuredContent"]
        assert res["newly_embedded"] == 3, res
        assert len(server.requests) == 1 and len(server.requests[0][1]["requests"]) == 3
        outcomes["vectors"] = snapshot(db)
        assert all(
            row[-1] == "google:gemini-embedding-001" for row in outcomes["vectors"]
        )
        assert embed("cache")["structuredContent"]["newly_embedded"] == 0
        assert len(server.requests) == 1
        res = tool(
            "search",
            "semantic_search_nodes_tool",
            provider="google",
            query="unrelated",
            limit=2,
        )["structuredContent"]
        assert (
            res["search_mode"] == "semantic"
            and server.requests[-1][1]["requests"][0]["taskType"] == "RETRIEVAL_QUERY"
        )
        with sqlite3.connect(db) as dest:
            dest.execute(
                "UPDATE nodes SET extra=json_set(extra,'$.docstring','Changed description') WHERE name='Widget'"
            )
            dest.execute(
                "INSERT INTO embeddings SELECT 'orphan',vector,text_hash,provider FROM embeddings LIMIT 1"
            )
        refresh = dict(
            flows=False,
            communities=False,
            fts=False,
            embedding_provider="google",
            embedding_model="gemini-embedding-001",
        )
        refresh_response = tool("refresh", "run_postprocess_tool", **refresh)
        assert "structuredContent" in refresh_response, refresh_response
        res = refresh_response["structuredContent"]
        assert res["embeddings_purged"] == 1 and res["embeddings_refreshed"] == 1, res
        outcomes["refreshed"] = snapshot(db)
        res = embed("bad", model="bad-request")
        assert res["isError"] and "INVALID_ARGUMENT" in res["content"][0]["text"], res
        assert snapshot(db) == outcomes["refreshed"]
        assert (
            embed("retry", model="retry-once")["structuredContent"]["newly_embedded"]
            == 3
        )
        assert sum("/retry-once:" in url for url, _ in server.requests) == 2
    finally:
        client.close()
    outcomes["requests"] = server.requests.copy()
    server.requests.clear()
    server.retry = True
    return outcomes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--sdk-path", type=Path)
    args = parser.parse_args()
    source, binary = args.upstream.resolve(), args.binary.resolve()
    pinned_reference(source)
    py = [sys.executable, "-B", "-m", "code_review_graph"]
    env = {
        key: v
        for key, v in os.environ.items()
        if key in ("PATH", "TMPDIR", "TMP", "TEMP", "LANG")
    }
    with (
        tempfile.TemporaryDirectory(prefix="crg-google-") as d,
        ThreadingHTTPServer(("127.0.0.1", 0), Gemini) as server,
    ):
        tmp = Path(d).resolve()
        root = tmp / "repo"
        root.mkdir()
        (root / "lib.py").write_text(
            "class Widget:\n    def total(self):\n        return 1\n\ndef compute():\n    return 2\n"
        )
        env.update(
            HOME=str(tmp / "home"),
            CRG_HOME=str(tmp / "registry"),
            NO_PROXY="*",
            NO_COLOR="1",
            TZ="UTC",
            PYTHONDONTWRITEBYTECODE="1",
            GOOGLE_API_KEY="test-local-key",
            CRG_ACCEPT_CLOUD_EMBEDDINGS="1",
            GOOGLE_GEMINI_BASE_URL=f"http://127.0.0.1:{server.server_port}/fixture",
        )
        refenv = dict(
            env,
            PYTHONPATH=os.pathsep.join(
                [
                    str(source),
                    *([str(args.sdk_path.resolve())] if args.sdk_path else []),
                ]
            ),
        )
        server.requests = []
        server.retry = True
        t = threading.Thread(target=server.serve_forever, daemon=True)
        t.start()
        try:
            result = run([*py, "build", "-q", "--repo", str(root)], root, refenv)
            assert result["exit"] == 0, result
            with (
                sqlite3.connect(":memory:") as saved,
                sqlite3.connect(root / ".code-review-graph/graph.db") as db,
            ):
                db.execute(
                    "UPDATE metadata SET value='2099-01-01T00:00:00+00:00' WHERE key='last_updated'"
                )
                db.commit()
                db.backup(saved)
                expected = scenario(py, root, refenv, server, saved)
                actual = scenario([str(binary)], root, env, server, saved)
            for key in expected:
                assert expected[key] == actual[key], (key, expected[key], actual[key])
            print(
                f"Google embedding parity: {len(expected)} exact observations including {len(expected['requests'])} loopback requests, vector rows, opt-in, caching, query, retry and rejected model."
            )
        finally:
            server.shutdown()
            t.join()


if __name__ == "__main__":
    main()
