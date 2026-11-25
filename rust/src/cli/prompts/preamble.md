## Rules for Token-Efficient Graph Usage
1. ALWAYS call `get_minimal_context_tool` first with a task description.
2. Use `detail_level="minimal"` on all tool calls unless the minimal output is insufficient.
3. Only escalate to `detail_level="standard"` or `"verbose"` for the specific entities that need deeper inspection.
4. Never request more than 3 tool calls per turn unless absolutely necessary.
5. Prefer targeted queries (query_graph_tool with a specific symbol) over broad scans (list_communities_tool with full members).
6. When reviewing changes: detect_changes_tool(detail_level="minimal") → only expand on high-risk items.
