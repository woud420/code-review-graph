## Review Workflow
1. Call `get_minimal_context_tool(task="review changes against {{argument}}", base="{{argument}}")` to get risk overview.
2. If risk is "low": call `detect_changes_tool(base="{{argument}}", detail_level="minimal")` → report summary + any test gaps.
3. If risk is "medium" or "high":
   a. Call `detect_changes_tool(base="{{argument}}", detail_level="standard")` for full change list.
   b. For each high-risk function, call `query_graph_tool(pattern="callers_of", target=<func>, detail_level="minimal")`.
   c. Call `get_affected_flows_tool(base="{{argument}}", detail_level="minimal")` only if >3 changed functions.
4. Summarize: risk level, what changed, test gaps, specific improvements needed.

Do NOT call get_review_context_tool unless you need source code snippets for a specific function.
