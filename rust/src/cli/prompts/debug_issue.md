## Debug Workflow
1. Call `get_minimal_context_tool(task="debug: {{argument}}")`.
2. Call `semantic_search_nodes_tool(query=<keywords from description>, detail_level="minimal", limit=5)`.
3. For the top 1-2 results, call `query_graph_tool(pattern="callers_of", target=<name>, detail_level="minimal")`.
4. If the issue involves execution flow: call `get_flow_tool(name=<relevant flow>)` for the single most relevant flow.
5. Only call `get_review_context_tool` or `get_impact_radius_tool` if you need to trace the blast radius of a specific change.
