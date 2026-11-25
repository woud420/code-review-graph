# Rust code-review graph

A local Rust CLI and read-only MCP server for finding symbols, callers, callees, and the transitive callers of a changed symbol. The Python implementation alongside this directory is the behavioral reference, pinned to upstream `main` at `7ba9d17ab6fd281026b914d8da4c0dc98d013474`.

## Run it

From this `rust/` directory, build the binary and index the parent repository.

```sh
cargo build --release --locked
target/release/code-review-graph-rs build --repo ..
target/release/code-review-graph-rs query callers-of 'code_review_graph/context_savings.py::_savings' --repo ..
target/release/code-review-graph-rs update --repo ..
```

Use `query callees-of` for outgoing calls, `query impact --depth 3` for transitive callers, and `query symbols` for a name search. A qualified target takes the form `relative/path::symbol`. Unqualified names combine matches from all files. All CLI results are JSON.

Point an MCP client's command at the binary with arguments `["serve", "--repo", "/absolute/path/to/repo"]`. The server exposes `query_graph_tool` with `pattern` (`callers_of` or `callees_of`) and `target`, plus `list_graph_stats_tool`. The results give distinct, directly resolved symbols and their source spans. Build the index with the CLI first. A running MCP session can read subsequent updates.

## Current scope

- JavaScript, TypeScript and TSX support includes relative imports, aliases, re-exports and callback calls. Python resolves explicit module-level relative and repository-root imports, aliases and re-exports. Rust supports same-file function and method definitions and common direct calls. Rust module and trait resolution, dynamic dispatch, TypeScript configuration aliases, and framework-specific inference remain open.
- The port stores its index in `.code-review-graph-rs/graph.db` with its own schema. An update scans supported files, hashes them and parses changed files. After a change it resolves calls across the stored facts in one SQLite transaction. On a large graph this trades update speed for simpler invalidation.
- Review risk scoring, test-coverage inference, watch hooks, embeddings, and the rest of upstream’s MCP tools are outside this milestone. Graph agreement measures differences from the pinned Python implementation. Both implementations use static heuristics.

## Measured agreement

Across the 95 production Python files at the reference commit, the first implementation reproduced 3,248 of Python’s 4,756 distinct, directly bound caller–callee pairs (68.3%). Adding Python import resolution raised this to 3,881 (81.6%). Python contained 3,881 of the 3,883 pairs emitted by Rust. Of the 875 pairs only Python reported, 590 crossed a file boundary. These numbers measure agreement on one corpus. The checker below can reproduce the current result.

## Reproduce the comparison

Run these commands from `rust/`. Use a Python interpreter with the upstream project's dependencies installed. The test checks a fingerprint of the Python source and builds all test indexes in temporary directories.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
python tests/differential.py --upstream .. --binary target/release/code-review-graph-rs --whole-package
python tests/mcp_stdio.py target/release/code-review-graph-rs
```

The differential test checks TypeScript symbol ranges, direct call sites and caller queries across a clean build, rename, repeated update, deletion, and restoration. It also checks rollback after an unreadable-as-UTF-8 source file and compares selected queries on upstream Rust and Python examples. The whole-package option reports overlap across directly bound production Python caller–callee pairs, counting each pair once. The MCP test uses the official Python client over stdio.

To repeat the architecture check, run `deslop-please` in this directory with `rust-analyzer` on your PATH.
