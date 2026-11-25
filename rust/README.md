# Native code-review-graph

Build at the repository root with the pinned Rust toolchain. Cargo compiles SQLite and the Tree-sitter parsers into the executable. The native build, formatter, linter, and process tests run without a Python installation.

```sh
make check
make build
target/release/code-review-graph build --repo /absolute/path/to/project
target/release/code-review-graph status --json --repo /absolute/path/to/project
target/release/code-review-graph query callers_of my_function --repo /absolute/path/to/project
target/release/code-review-graph serve --repo /absolute/path/to/project
```

The public binary reads the upstream v15 SQLite format. To try a fresh native index alongside an existing one, set `CRG_DATA_DIR` to a separate directory for every command in the session. The public writer uses a second file in that directory to cache parsed facts. Writes use the upstream `write.lock`, and a failed update rolls back its changes to the public graph.

Use `--skip-flows` for a build with signatures and text search. Use `--skip-postprocess` for the raw graph. For MCP over stdio, configure the executable as the command with arguments `["serve", "--repo", "/absolute/path/to/project"]`. Its tool list reports the operations currently implemented.

The earlier `code-review-graph-rs` binary exposes the original proof interface. It stores a separate index in `.code-review-graph-rs/graph.db`. Its `query callers-of` and `query callees-of` commands report distinct symbols. The public binary's `query callers_of` and `query callees_of` also expose the individual call sites.

## Verification

The comparison uses upstream commit `7ba9d17ab6fd281026b914d8da4c0dc98d013474`. From the repository root, create the reference environment and run the comparisons. An existing external checkout of that exact revision also works.

```sh
git worktree add --detach ../crg-reference 7ba9d17ab6fd281026b914d8da4c0dc98d013474
uv sync --project ../crg-reference --python 3.12 --frozen --group dev
make cutover REFERENCE=../crg-reference PYTHON=../crg-reference/.venv/bin/python
```

- `make check` runs rustfmt, Clippy with warnings denied, type checks, and native tests. Process tests launch the public binary with an empty executable search path.
- `make parity` compares the proof binary with pinned Python on rebuild, incremental rename, duplicate notification, deletion, restoration, and direct caller queries. It also checks the official MCP client, public build tool, language-specific fixtures, graph queries, search ranking, and the context history writer and reader. It exercises actual MCP rename previews and edits, adopted multilingual databases, visualization file bytes and evaluation reports from upstream CSVs. The whole-package check requires exact equality of the distinct bound caller–callee pairs across all 95 upstream Python modules.
- `make graph-parity` builds two independent public v15 databases from those 95 modules. It compares file hashes, symbol spans, declarations, duplicate edges, certainty attributes, flow members and scores, and community assignments. Timestamps and storage-specific row IDs are excluded from this graph comparison.
- `make public-parity` runs both public command lines in separate processes on an identical fixture path. It compares stdout, stderr, exit codes, reads from a Python-built database, MCP protocol responses, prompts and tool descriptions. Its operation inventory reports entry points that still need behavioral fixtures.
- `make upstream-surface` exercises the pinned upstream release’s command and flag scenarios through both installed executables in an isolated home and repository. It records stdout, stderr and exit codes.
- `make fixture-parity` compares independently built graphs across the full upstream multilingual fixture directory, including languages that Rust cannot index yet.
- `make cutover` runs every comparison and returns a failing status while any comparison or coverage requirement fails. The public and database reports are written to `target/parity/` by default. GitHub Actions runs the same replacement gate independently of its native build job.

Native indexing includes Python, Rust, JavaScript, TypeScript, TSX, shell, JSON, Go, C, C++, Java, C#, SQL, YAML, Terraform, Solidity, Lua, Luau, PHP and Ruby. The strict graph comparison covers every file in the pinned upstream Python package. The wider fixture and public behavior reports track open differences, including unsupported languages, installers and optional integrations. Keep Python available for those workflows until the replacement gate passes.

For an architecture check, run `deslop-please` at the workspace root with `rust-analyzer` on the executable search path.
