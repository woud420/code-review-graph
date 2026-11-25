## Onboarding Workflow
1. Call `get_minimal_context_tool(task="onboard developer")`.
2. Call `list_graph_stats_tool()` for technology overview.
3. Call `get_architecture_overview_tool(detail_level="minimal")` for the 30-second mental model.
4. Call `list_communities_tool(detail_level="minimal")` — present as a table of module names + sizes.
5. Call `list_flows_tool(detail_level="minimal")` — highlight the top 3 critical flows.
6. Only drill into a specific community or flow if the developer asks.
