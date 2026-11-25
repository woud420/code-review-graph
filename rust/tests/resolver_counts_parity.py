"""Compare actual resolver work, including already-resolved edges, over both real MCP servers."""

import argparse
import os
from pathlib import Path
import sys
import tempfile

from canonical_parity import read_graph
from differential import git, new_repo, verify_source
from mcp_contract import WireClient


def counts(client, arguments):
    response = client.call(
        "tools/call", {"name": "build_or_update_graph_tool", "arguments": arguments}
    )["response"]["result"]
    assert not response.get("isError"), response
    return {
        k: response["structuredContent"][k]
        for k in ("spring_resolution", "temporal_resolution", "hcl_resolution")
        if k in response["structuredContent"]
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-resolver-counts-") as tmp:
        base = Path(tmp).resolve()
        repo = base / "repo"
        fixtures = upstream / "tests/fixtures"
        new_repo(
            repo,
            {
                "SpringDI.java": (fixtures / "SpringDI.java").read_text(),
                "TemporalWorkflow.java": (fixtures / "TemporalWorkflow.java").read_text(),
                "isolated/sample.tf": (fixtures / "sample.tf").read_text(),
                "infra/app.tf": 'output "region" { value = var.region }\nmodule "net" { source = "./net" }\n',
                "infra/vars.tf": 'variable "region" {}\n',
                "infra/net/main.tf": 'variable "name" {}\n',
            },
        )
        env = os.environ | {
            "PYTHONPATH": str(upstream),
            "CRG_HOME": str(base / "registry"),
            "PYTHONDONTWRITEBYTECODE": "1",
            "CRG_PARSE_EXECUTOR": "serial",
        }
        clients = []
        try:
            for i, cmd in enumerate(([sys.executable, "-m", "code_review_graph"], [str(binary)])):
                client = WireClient(
                    [*cmd, "serve", "--repo", str(repo)],
                    repo,
                    env | {"CRG_DATA_DIR": str(base / str(i))},
                )
                clients.append(client)
                client.call(
                    "initialize",
                    {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "resolver-counts", "version": "1"},
                    },
                )
                client.send({"method": "notifications/initialized"})
            for phase in ("raw", "full", "new-files", "spring-and-hcl", "temporal", "no-op"):
                if phase == "new-files":
                    (repo / "Unrelated.java").write_text("class Unrelated {}\n")
                    (repo / "infra/unrelated.tf").write_text('variable "unused" {}\n')
                elif phase == "spring-and-hcl":
                    for name in ("SpringDI.java", "infra/app.tf"):
                        p = repo / name
                        p.write_text(p.read_text() + "\n")
                elif phase == "temporal":
                    p = repo / "TemporalWorkflow.java"
                    p.write_text(
                        p.read_text().replace("shippingActivity.shipOrder(orderId)", '"removed"')
                    )
                if phase in ("new-files", "spring-and-hcl", "temporal"):
                    git(repo, "add", "-A")
                    git(repo, "commit", "-qm", phase)
                request = {"full_rebuild": True} if phase in ("raw", "full") else {}
                if phase == "raw":
                    request["postprocess"] = "none"
                expected, actual = (counts(c, request) for c in clients)
                assert expected == actual, (phase, expected, actual)
                spring, temporal, hcl = (
                    expected.get(key)
                    for key in ("spring_resolution", "temporal_resolution", "hcl_resolution")
                )
                python_edges, native_edges = (
                    read_graph(base / str(i) / "graph.db", repo)["edge_attributes"]
                    for i in range(2)
                )
                assert python_edges == native_edges, (
                    phase,
                    (python_edges - native_edges).most_common(3),
                    (native_edges - python_edges).most_common(3),
                )
                if phase in ("raw", "full"):
                    assert spring["calls_resolved"] > 0 and temporal["calls_resolved"] > 0
                    assert hcl["references_resolved"] > 0 and hcl["imports_resolved"] > 0
                elif phase == "new-files":
                    assert (
                        spring["calls_resolved"]
                        == temporal["calls_resolved"]
                        == hcl["references_resolved"]
                        == hcl["imports_resolved"]
                        == 0
                    )
                elif phase == "spring-and-hcl":
                    assert (
                        spring["calls_resolved"] > 0
                        and temporal["calls_resolved"] == 0
                        and hcl["imports_resolved"] > 0
                    )
                elif phase == "temporal":
                    assert (
                        spring["calls_resolved"] == 0
                        and temporal["calls_resolved"] > 0
                        and hcl is None
                    )
                elif phase == "no-op":
                    assert spring is None and temporal is None and hcl is None
                print(phase, expected)
        finally:
            for client in clients:
                client.close()


if __name__ == "__main__":
    main()
