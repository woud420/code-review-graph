"""Compare the locked Python MiniMax client and production Rust transport against loopback."""

import argparse
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading

from public_parity import pinned_reference


class MiniMax(BaseHTTPRequestHandler):
    def do_POST(self):
        assert (
            self.path == "/v1/embeddings"
            and self.headers["Authorization"] == "Bearer local-fixture"
        )
        assert self.headers["Accept"] == "application/json"
        data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.requests.append(data)
        code = 200
        if "service-error" in data["texts"]:
            body = {"base_resp": {"status_code": 9, "status_msg": "fixture rejection"}}
        elif "unauthorized" in data["texts"]:
            code, body = 401, {"error": "unauthorized"}
        elif "retry" in data["texts"] and self.server.retry:
            self.server.retry = False
            code, body = 503, {"error": "busy"}
        else:
            body = {
                "base_resp": {"status_code": 0},
                "vectors": [[1.0, len(s) * 0.1, 0.5] for s in data["texts"]],
            }
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body).encode())

    def log_message(self, *args):
        pass


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    args = p.parse_args()
    upstream = args.upstream.resolve()
    pinned_reference(upstream)
    sys.path.insert(0, str(upstream))
    from code_review_graph.embeddings import MiniMaxEmbeddingProvider

    with (
        ThreadingHTTPServer(("127.0.0.1", 0), MiniMax) as server,
        tempfile.TemporaryDirectory(prefix="crg-minimax-") as d,
    ):
        tmp = Path(d)
        server.requests = []
        server.retry = True
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            py = MiniMaxEmbeddingProvider("local-fixture")
            py._ENDPOINT = f"http://127.0.0.1:{server.server_port}/v1/embeddings"
            observed = {
                "identity": py.name,
                "documents": py.embed([f"doc-{i}" for i in range(103)]),
                "query": py.embed_query("search"),
            }
            errors = []
            for text in ("service-error", "unauthorized"):
                try:
                    py.embed([text])
                except (RuntimeError, OSError) as exc:
                    errors.append(str(exc))
            observed.update(errors=errors, retry=py.embed(["retry"]))
            assert len(observed["documents"]) == 103 and errors == [
                "MiniMax API error: fixture rejection",
                "HTTP Error 401: Unauthorized",
            ]
            calls = server.requests.copy()
            server.requests.clear()
            server.retry = True
            assert [len(r["texts"]) for r in calls[:2]] == [100, 3] and calls[2]["type"] == "query"
            assert len(calls) == 7 and all(r["model"] == "embo-01" for r in calls)
            env = {
                k: v
                for k, v in os.environ.items()
                if not k.startswith(("CRG_", "GOOGLE_", "MINIMAX_", "OPENAI_", "VOYAGE_"))
            }
            dest = tmp / "actual.json"
            env.update(
                CRG_TEST_MINIMAX_PORT=str(server.server_port),
                CRG_TEST_MINIMAX_RESULT=str(dest),
                MINIMAX_API_KEY="local-fixture",
                CRG_ACCEPT_CLOUD_EMBEDDINGS="1",
                NO_PROXY="*",
            )
            crate = Path(__file__).resolve().parents[1] / "Cargo.toml"
            subprocess.run(
                [
                    "cargo",
                    "test",
                    "--manifest-path",
                    str(crate),
                    "--locked",
                    "--lib",
                    "paired_minimax_protocol",
                    "--",
                    "--ignored",
                ],
                env=env,
                check=True,
            )
            actual = json.loads(dest.read_bytes())
            assert actual == observed, (observed, actual)
            assert calls == server.requests, (calls, server.requests)
            print(
                "MiniMax: 7 matching loopback requests; 103 document vectors, query, service error, HTTP failure and retry."
            )
        finally:
            server.shutdown()
            worker.join()


if __name__ == "__main__":
    main()
