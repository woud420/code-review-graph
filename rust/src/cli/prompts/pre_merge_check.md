## Pre-Merge Check Workflow
1. Call `get_minimal_context_tool(task="pre-merge check", base="{{argument}}")`.
2. Call `detect_changes_tool(base="{{argument}}", detail_level="minimal")` for risk score and test gaps.
3. If risk > 0.4: call `get_affected_flows_tool(base="{{argument}}", detail_level="minimal")`.
4. If test_gap_count > 0: call `query_graph_tool(pattern="tests_for", target=<each untested function>, detail_level="minimal")` for up to 3 functions.
5. Call `refactor_tool(mode="dead_code", detail_level="minimal")` to check for newly dead code.
6. Only call `find_large_functions_tool` or `get_impact_radius_tool` if risk > 0.7.
7. Output: GO/NO-GO recommendation with 1-sentence justification + list of required follow-ups.
