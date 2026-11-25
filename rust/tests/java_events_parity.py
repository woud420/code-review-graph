"""Compare Spring event graph storage, package-scoped queries and resolver counts on real stdio."""

import argparse
import ast
import hashlib
import importlib.util
import json
import os
import sqlite3
import sys
import tempfile
from pathlib import Path

from canonical_parity import read_graph
from differential import git, new_repo
from mcp_contract import WireClient
from public_parity import pinned_reference


def invoke(client, name, **args):
    message = client.call("tools/call", {"name": name, "arguments": args})["response"]["result"]
    assert message.get("isError") is False, message
    value = message["structuredContent"]
    assert json.loads(message["content"][0]["text"]) == value
    return value


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    opts = p.parse_args()
    source = opts.upstream.resolve()
    binary = opts.binary.resolve()
    pinned_reference(source)
    sys.path.insert(0, str(source))
    suite = source / "tests/test_spring_events.py"
    assert hashlib.sha256(suite.read_bytes()).hexdigest() == (
        "8285225e29c6e838b6f08739a36bfadc9640e1cfae8d5bd243433fbf2307705f"
    )
    spec = importlib.util.spec_from_file_location("pinned_spring_events", suite)
    tests = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(tests)
    tree = ast.parse(suite.read_text())
    with tempfile.TemporaryDirectory(prefix="crg-events-") as scratch:
        temp = Path(scratch).resolve()
        repo = temp / "repo"
        new_repo(repo, {"README.txt": "Spring event acceptance tests\n"})
        for pkg in ("alpha", "beta"):
            tests._write_event_package(repo, pkg)
        extra = repo / "extra"
        extra.mkdir()
        for test, filename in [
            ("test_event_edges_use_package_and_import_qualified_identity", "Listeners.java"),
            ("test_publish_event_new_expression_uses_qualified_identity", "ExternalPublisher.java"),
        ]:
            case = next(n for n in tree.body if getattr(n, "name", None) == test)
            fixture = next(
                n.value
                for n in ast.walk(case)
                if isinstance(n, ast.Constant)
                and isinstance(n.value, str)
                and "package " in n.value
            )
            (extra / filename).write_text(fixture)
        (extra / "Plain.java").write_text("""package omega;
class Plain {
    @org.springframework.context.event.EventListener(
        classes={Alpha.class, Alpha.class}, id="notice", condition="#a.ok", defaultExecution=false)
    void on(Alpha e) {}
    void send() { this.bus.publishEvent(new Alpha()); publishEvent(new Beta()); }
}
""")
        git(repo, "add", ".")
        git(repo, "commit", "-qm", "record publisher and listener sources")
        env = {
            k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "FASTMCP_"))
        }
        clients = []
        data = []
        try:
            for name, command in [
                ("python", [sys.executable, "-m", "code_review_graph"]),
                ("rust", [str(binary)]),
            ]:
                cache = temp / name
                data.append(cache / "graph.db")
                settings = dict(
                    env,
                    CRG_DATA_DIR=str(cache),
                    CRG_HOME=str(temp / (name + "home")),
                    PYTHONHASHSEED="0",
                    CRG_SERIAL_PARSE="1",
                )
                if name == "python":
                    settings.update(PYTHONPATH=str(source), PYTHONDONTWRITEBYTECODE="1")
                client = WireClient(command + ["serve", "--repo", str(repo)], repo, settings)
                clients.append(client)
                client.call(
                    "initialize",
                    {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "spring", "version": "1"},
                    },
                )
                client.send({"method": "notifications/initialized"})
            for stage in ("raw", "full", "retarget", "delete", "prune"):
                if stage == "retarget":
                    f = repo / "alpha/Listener.java"
                    f.write_text(f.read_text().replace("SharedEvent event", "OtherEvent event"))
                    git(repo, "commit", "-qam", "retarget alpha listener only")
                elif stage == "delete":
                    (repo / "beta/Listener.java").unlink()
                    git(repo, "commit", "-qam", "remove beta listener")
                elif stage == "prune":
                    (extra / "Plain.java").unlink()
                    git(repo, "commit", "-qam", "remove the last publisher and listener for omega")
                reports = [
                    invoke(
                        c,
                        "build_or_update_graph_tool",
                        repo_root=str(repo),
                        full_rebuild=stage in ("raw", "full"),
                        postprocess="none" if stage == "raw" else "full",
                    )
                    for c in clients
                ]
                assert reports[0]["event_resolution"] == reports[1]["event_resolution"], (
                    stage,
                    reports,
                )
                graphs = [read_graph(db, repo) for db in data]
                for key, expected in graphs[0].items():
                    got = graphs[1][key]
                    assert expected == got, (
                        stage,
                        key,
                        (expected - got).most_common(5),
                        (got - expected).most_common(5),
                    )
                with sqlite3.connect(data[0]) as db:
                    amount = db.execute(
                        "select count(*) from edges "
                        "where json_extract(extra,'$.spring_event_resolved')=1"
                    ).fetchone()[0]
                assert (
                    amount == {"raw": 4, "full": 4, "retarget": 3, "delete": 2, "prune": 1}[stage]
                ), amount
                for pattern in ("publishers_of", "listeners_of"):
                    pair = [
                        invoke(
                            c,
                            "query_graph_tool",
                            pattern=pattern,
                            target="event::alpha.SharedEvent",
                            repo_root=str(repo),
                            max_results=1,
                        )
                        for c in clients
                    ]
                    # Compare projected event records below the MCP envelope.
                    assert pair[0]["results"] == pair[1]["results"], (stage, pattern, pair)
                    assert bool(pair[0]["results"]) == (
                        pattern == "publishers_of" or stage in ("raw", "full")
                    )
                print(
                    stage,
                    reports[0]["event_resolution"],
                    f"{len(graphs[0])}/9 graph projections; publisher/listener queries matched",
                )
        finally:
            for c in clients:
                c.close()


if __name__ == "__main__":
    main()
