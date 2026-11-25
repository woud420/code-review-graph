## Architecture Mapping Workflow
1. Call `get_minimal_context_tool(task="map architecture")`.
2. Call `get_architecture_overview_tool(detail_level="minimal")` for community coupling summary.
3. Call `list_flows_tool(detail_level="minimal")` for critical flow names + criticality scores.
4. Only call `get_community_tool(name=<X>, detail_level="standard")` for the 1-2 communities the user is most interested in.
5. Produce a concise Mermaid diagram showing communities as boxes and key flows as arrows.
