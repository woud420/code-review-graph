"""Run with a Python environment containing the MCP SDK: python tests/mcp_stdio.py BINARY."""

import asyncio
import json
import subprocess
import sys
import tempfile
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


async def check(binary: Path, repo: Path) -> None:
    server = StdioServerParameters(command=str(binary), args=["--repo", str(repo), "serve"])
    async with stdio_client(server) as (reader, writer), ClientSession(reader, writer) as client:
        info = await client.initialize()
        assert info.capabilities.tools is not None
        tools = {tool.name: tool for tool in (await client.list_tools()).tools}
        assert set(tools) == {"list_graph_stats_tool", "query_graph_tool"}
        assert all(tool.annotations.model_dump(by_alias=True)["readOnlyHint"] for tool in tools.values())

        missing = await client.call_tool("list_graph_stats_tool", {})
        assert missing.is_error
        assert not (repo / ".code-review-graph-rs").exists()

        (repo / "lib.ts").write_text("export function target() {}\n")
        (repo / "app.ts").write_text(
            "import {target as go} from './lib.js';\n"
            "export function outer() { go(); go(); }\n"
        )
        subprocess.run([binary, "--repo", repo, "build"], capture_output=True, check=True)
        stats = await client.call_tool("list_graph_stats_tool", {})
        assert not stats.is_error and stats.structured_content["files"] == 2

        for pattern, target, expected in [
            ("callers_of", "lib.ts::target", "outer"),
            ("callees_of", "app.ts::outer", "target"),
        ]:
            args = {"pattern": pattern, "target": target}
            answer = await client.call_tool("query_graph_tool", args)
            assert not answer.is_error
            assert json.loads(answer.content[0].text) == answer.structured_content
            assert [hit["name"] for hit in answer.structured_content["results"]] == [expected]


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as temp:
        asyncio.run(check(Path(sys.argv[1]).resolve(), Path(temp)))
    print("MCP stdio handshake, tools, missing-index handling, stats and graph queries passed")
