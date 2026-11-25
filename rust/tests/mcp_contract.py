"""Raw stdio JSON-RPC probes. Preserve the response envelope and MCP content blocks."""

import json
import queue
import subprocess
import tempfile
import threading

QUERY_CASES = {
    "stats": ("list_graph_stats_tool", {}),
    "callers": ("query_graph_tool", {"pattern": "callers_of", "target": "target"}),
    "limit": ("query_graph_tool", {"pattern": "callers_of", "target": "target", "max_results": 1}),
    "minimal": ("query_graph_tool", {"pattern": "callers_of", "target": "target", "detail_level": "minimal"}),
    "direct": ("query_graph_tool", {"pattern": "callees_of", "target": "caller", "resolution": "direct"}),
    "missing-target": ("query_graph_tool", {"pattern": "callers_of", "target": "absentSymbol"}),
    "missing-arg": ("query_graph_tool", {"pattern": "callers_of"}),
    "invalid-pattern": ("query_graph_tool", {"pattern": "nonexistent", "target": "target"}),
    "docs": ("get_docs_section_tool", {"section_name": "legal"}),
    "missing-docs": ("get_docs_section_tool", {"section_name": "no-such-section"}),
    "unknown-tool": ("no_such_parity_tool", {}),
}
REVIEW_CASES = {
    "impact": ("get_impact_radius_tool", {"changed_files": ["lib.ts"], "max_results": 1}),
    "review": ("get_review_context_tool", {"changed_files": ["lib.ts"], "include_source": True}),
    "minimal-context": ("get_minimal_context_tool", {"task": "review", "changed_files": ["lib.ts"]}),
    "changes": ("detect_changes_tool", {"changed_files": ["lib.ts"], "max_results": 1}),
    "large-functions": ("find_large_functions_tool", {"min_lines": 1, "limit": 1}),
    "hubs": ("get_hub_nodes_tool", {"top_n": 1}),
    "bridges": ("get_bridge_nodes_tool", {"top_n": 1}),
    "gaps": ("get_knowledge_gaps_tool", {"max_per_category": 1}),
    "questions": ("get_suggested_questions_tool", {}),
    "architecture": ("get_architecture_overview_tool", {"max_results": 1}),
    "no-flow": ("get_flow_tool", {"flow_name": "absent"}),
    "flows": ("list_flows_tool", {"limit": 1}),
    "affected-flows": ("get_affected_flows_tool", {"changed_files": ["lib.ts"]}),
    "communities": ("list_communities_tool", {"min_size": 1}),
    "no-community": ("get_community_tool", {"community_name": "absent"}),
    "no-wiki-page": ("get_wiki_page_tool", {"community_name": "absent"}),
    "connections": ("get_surprising_connections_tool", {"top_n": 1}),
    "isolated-registry": ("list_repos_tool", {}),
}


class WireClient:
    def __init__(self, command, cwd, env):
        self.stderr = tempfile.TemporaryFile(mode="w+t")
        self.child = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=self.stderr, text=True,
                                      encoding="utf-8", bufsize=1)
        self.messages = queue.Queue()
        self.sequence = 0
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()

    def _read(self):
        for line in self.child.stdout:
            self.messages.put(line)
        self.messages.put(None)

    def send(self, message):
        self.child.stdin.write(json.dumps({"jsonrpc": "2.0", **message}) + "\n")
        self.child.stdin.flush()

    def call(self, method, params):
        self.sequence += 1
        self.send({"id": self.sequence, "method": method, "params": params})
        notices = []
        for _ in range(100):
            try:
                line = self.messages.get(timeout=25)
            except queue.Empty as exc:
                raise TimeoutError(f"{method}: no MCP response within 25 seconds") from exc
            if line is None:
                self.stderr.seek(0)
                raise RuntimeError(f"{method}: server EOF: {self.stderr.read()[-1200:]}")
            message = json.loads(line)
            if "id" not in message:
                notices.append(message)
                continue
            if message["id"] != self.sequence:
                raise ValueError(f"unexpected JSON-RPC response to {method}: {message}")
            return {"response": message, "notifications": notices}
        raise RuntimeError(f"{method}: more than 100 messages before reply")

    def close(self):
        try:
            self.child.stdin.close()
        except BrokenPipeError:
            pass
        try:
            self.child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.child.terminate()
            try:
                self.child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=2)
        self.thread.join(timeout=1)
        self.child.stdout.close()
        self.stderr.close()


def catalog(client, category):
    pages, entries, cursors = [], {}, set()
    cursor = None
    for _ in range(100):
        answer = client.call(category + "/list", {"cursor": cursor} if cursor else {})
        pages.append(answer)
        body = answer["response"]
        if "error" in body:
            break
        result = body["result"]
        for item in result[category]:
            name = item["name"]
            if name in entries:
                raise RuntimeError(f"duplicate {category} entry {name!r}")
            entries[name] = item
        cursor = result.get("nextCursor")
        if not cursor:
            break
        if cursor in cursors:
            raise RuntimeError(f"repeated {category} cursor")
        cursors.add(cursor)
    else:
        raise RuntimeError(f"too many {category} pages")
    return pages, entries


def inspect(command, repo, env, prompt_specs=None, extended=False):
    client = WireClient(command + ["serve", "--repo", str(repo)], repo, env)
    probes, inventory = {}, {}
    try:
        probes["initialize"] = client.call("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "parity-oracle", "version": "1"},
        })
        client.send({"method": "notifications/initialized"})
        for category in ("tools", "prompts"):
            pages, inventory[category] = catalog(client, category)
            probes[category + "/list"] = pages
        queries = QUERY_CASES | REVIEW_CASES if extended else QUERY_CASES
        for label, (name, arguments) in queries.items():
            probes["tools/call/" + label] = client.call("tools/call", {"name": name, "arguments": arguments})
        if prompt_specs is None:
            prompt_specs = inventory["prompts"]
        for name, prompt in sorted(prompt_specs.items()):
            arguments = {}
            for argument in prompt.get("arguments", []):
                if argument.get("required"):
                    arguments[argument["name"]] = str(repo) if argument["name"] == "repo_root" else "target"
            probes["prompts/get/" + name] = client.call("prompts/get", {"name": name, "arguments": arguments})
        probes["invalid-method"] = client.call("parity/does-not-exist", {})
    finally:
        client.close()
    return probes, inventory
