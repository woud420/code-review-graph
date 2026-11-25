.PHONY: install dev build test typecheck lint fmt format-check check clean

install:
	cargo fetch --locked

dev:
	cargo run --locked --bin code-review-graph -- --help

build:
	cargo build --workspace --release --locked --bins

test:
	cargo test --workspace --locked

typecheck:
	cargo check --workspace --locked --all-targets

fmt:
	cargo fmt --all

format-check:
	cargo fmt --all -- --check

lint: format-check
	cargo clippy --workspace --locked --all-targets --all-features -- -D warnings

check: lint typecheck test

clean:
	cargo clean

REFERENCE ?= .
PYTHON ?= python3
TARGET_DIR := $(abspath $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target))
PARITY_REPORT_DIR ?= $(TARGET_DIR)/parity

.PHONY: parity graph-parity public-parity fixture-parity upstream-surface cutover

parity:
	cargo build --workspace --bins --locked
	"$(PYTHON)" -B rust/tests/cfamily_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/java_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/php_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/lua_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/extended_languages_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/solidity_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/csharp_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/go_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --proof-binary "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/differential.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph-rs" --whole-package
	"$(PYTHON)" -B rust/tests/mcp_stdio.py "$(TARGET_DIR)/debug/code-review-graph-rs"
	"$(PYTHON)" -B rust/tests/build_tool_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/source_fixture_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/sql_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/hcl_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/standalone_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/forget_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/ruby_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/rename_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/eval_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/embedding_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/installer_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/hooks_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/visualize_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/history_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/config_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/query_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/detect_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/postprocess_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"
	"$(PYTHON)" -B rust/tests/search_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph"

upstream-surface public-parity graph-parity fixture-parity: parity-binary

.PHONY: parity-binary
parity-binary:
	cargo build --locked --bin code-review-graph --bin crg-daemon
	mkdir -p "$(PARITY_REPORT_DIR)"

public-parity:
	"$(PYTHON)" -B rust/tests/public_parity.py --cutover --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --report "$(PARITY_REPORT_DIR)/public.json"

graph-parity:
	"$(PYTHON)" -B rust/tests/canonical_parity.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --report "$(PARITY_REPORT_DIR)/graph.json"

fixture-parity:
	"$(PYTHON)" -B rust/tests/canonical_parity.py --fixtures --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --report "$(PARITY_REPORT_DIR)/fixtures.json"

upstream-surface:
	"$(PYTHON)" -B rust/tests/upstream_surface.py --upstream "$(REFERENCE)" --binary "$(TARGET_DIR)/debug/code-review-graph" --report "$(PARITY_REPORT_DIR)/upstream-surface.json"

cutover:
	$(MAKE) -k parity public-parity graph-parity fixture-parity upstream-surface
