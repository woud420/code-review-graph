"""Exercise native stdio preview and apply with the pinned upstream rename acceptance suite."""
import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

from mcp_contract import WireClient
from public_parity import pinned_reference

SUITE_HASH = "c60da03693ef3edfbbd84c600d728e024577bc73d0909bbf73edaa61ae0e6335"


def server(command, root, env):
    client = WireClient([*command, "serve", "--repo", str(root)], root, env)
    client.call("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                               "clientInfo": {"name": "rename-oracle", "version": "1"}})
    client.send({"method": "notifications/initialized"})
    return client


def call(client, name, args):
    started = time.time()
    wire = client.call("tools/call", {"name": name, "arguments": args})
    finished = time.time()
    assert not wire["notifications"], wire
    envelope = wire["response"]["result"]
    assert envelope["isError"] is False, envelope
    value = envelope["structuredContent"]
    assert envelope["content"] == [{"type": "text", "text": json.dumps(
        value, ensure_ascii=False, separators=(",", ":"))}], envelope
    graph = value.get("_graph", {})
    assert "age_seconds" not in graph or "updated_at" in graph, value
    if "updated_at" in graph:
        built = datetime.fromisoformat(graph["updated_at"])
        # Both servers run in UTC and interpret a naive build timestamp in their local zone.
        epoch = (built if built.tzinfo else built.replace(tzinfo=timezone.utc)).timestamp()
        age = graph.get("age_seconds")
        # SQLite samples its wall clock in milliseconds. Account for that precision at the
        # boundary, while rejecting incorrect ages even when both implementations agree.
        low = max(0, int(started - epoch - 0.001))
        high = max(0, int(finished - epoch + 0.001))
        assert type(age) is int and low <= age <= high, (value, started, finished, low, high)
    if "created_at" in value:
        assert started <= value["created_at"] <= finished, (value, started, finished)
    return value


def assert_same_response(actual, expected):
    def deterministic_fields(value):
        if "age_seconds" not in value.get("_graph", {}):
            return value
        # Each request's actual age was validated against its own observation window.
        return {**value, "_graph": {**value["_graph"], "age_seconds": "<measured>"}}
    assert deterministic_fields(actual) == deterministic_fields(expected), (actual, expected)


def normalized_preview(data):
    value = dict(data)
    if value.get("status") == "ok":
        identifier = value.pop("refactor_id")
        assert re.fullmatch("[0-9a-f]{8}", identifier)
        del value["created_at"]
        assert identifier in value["summary"]
        value["summary"] = value["summary"].replace(identifier, "<preview>")
    return value


class Comparison:
    def __init__(self, reference, native):
        self.reference, self.native = reference, native
        self.pairs = {}
        self.counts = {"preview": 0, "dry_run": 0, "apply": 0}

    def preview(self, **args):
        expected = call(self.reference, "refactor_tool", args)
        actual = call(self.native, "refactor_tool", args)
        return self.check_preview(actual, expected)

    def check_preview(self, actual, expected):
        assert_same_response(normalized_preview(actual), normalized_preview(expected))
        if actual["status"] == "ok":
            assert actual["refactor_id"] not in self.pairs
            self.pairs[actual["refactor_id"]] = expected["refactor_id"]
        self.counts["preview"] += 1
        return actual

    def apply(self, refactor_id, repo_root, dry_run=False, max_diff_files=25):
        args = dict(repo_root=repo_root, dry_run=dry_run, max_diff_files=max_diff_files)
        ids = (self.pairs.get(refactor_id, refactor_id), refactor_id)
        def request(client, identifier):
            return call(client, "apply_refactor_tool", dict(args, refactor_id=identifier))
        # Replay both implementations against identical bytes at the same absolute paths.
        root = Path(repo_root)
        sources = [f for f in root.rglob("*") if f.is_file() and
                   not {".git", ".code-review-graph", "__pycache__"}.intersection(f.parts)]
        before = {f: f.read_bytes() for f in sources}
        expected = request(self.reference, ids[0])
        expected_bytes = {f: f.read_bytes() for f in sources}
        for f in sources:
            if before[f] != expected_bytes[f]:
                assert not dry_run
                f.write_bytes(before[f])
        actual = request(self.native, ids[1])
        if expected.get("status") == "error":
            expected["error"] = expected["error"].replace(ids[0], ids[1])
        assert_same_response(actual, expected)
        assert {f: f.read_bytes() for f in sources} == expected_bytes
        self.counts["dry_run" if dry_run else "apply"] += 1
        return actual

    def pytest_collection_modifyitems(self, items):
        module = items[0].module
        module.refactor_func = self.preview
        module.apply_refactor_func = self.apply
        # Four white-box cases inject a private Python dict. The stdio probes below
        # exercise a stale token and an unreadable file through genuine previews.
        items[:] = [item for item in items if item.cls and item.cls.__name__ != "TestApplyIsExact"]


def guard_cases(comp, root, build):
    root.mkdir()
    (root / "lib.py").write_text("def helper():\n    return 1\n")
    (root / "caller.py").write_text("from lib import helper\nprint(helper())\n")
    build(root)
    def preview(**opts):
        return comp.preview(mode="rename", old_name="helper", new_name="renamed",
                            repo_root=str(root), **opts)["refactor_id"]
    identifier = preview(max_results=1, detail_level="minimal")
    dry = comp.apply(identifier, str(root), dry_run=True, max_diff_files=1)
    assert dry["edits_applied"] == 3 and dry["diffs_truncated"] and len(dry["diffs"]) == 1
    (root / "caller.py").write_text("from lib import helper_two\nprint(helper_two())\n")
    result = comp.apply(identifier, str(root))
    assert result["edits_applied"] == 1 and result["edits_skipped"] == 2, result
    assert comp.apply(identifier, str(root))["status"] == "error"
    (root / "lib.py").write_text("def helper():\n    return 1\n")
    identifier = preview()
    (root / "lib.py").write_bytes(b"# \xff\n")
    assert comp.apply(identifier, str(root))["edits_skipped"] >= 1
    target = root / "lib.py"
    target.write_text("def helper():\n    return 1\n")
    identifier = preview()
    other = root.parent / "outside.py"
    other.write_bytes(target.read_bytes())
    target.unlink()
    target.symlink_to(other)
    for dry_run in (True, False):
        blocked = comp.apply(identifier, str(root), dry_run=dry_run)
        assert blocked["status"] == "error" and "outside repo root" in blocked["error"]
        assert other.read_bytes() == b"def helper():\n    return 1\n"


def star_reexport_across_seconds(comp, root, build):
    for package in ("pkg", "app"):
        (root / package).mkdir(parents=True)
    (root / "pkg/__init__.py").write_text("from .util import *\n")
    (root / "app/__init__.py").write_bytes(b"")
    home = root / "pkg/util.py"
    caller = root / "app/use.py"
    home.write_text("__all__ = ['helper']\n\ndef helper(x):\n    return x * 2\n")
    caller.write_text("from pkg import helper\n\nVALUE = helper(3)\n")
    build(root)
    with sqlite3.connect(root / ".code-review-graph/graph.db") as db:
        db.execute("UPDATE metadata SET value='2020-01-01T00:00:00Z' WHERE key='last_updated'")
    args = dict(mode="rename", old_name="helper", new_name="double", repo_root=str(root))
    expected = call(comp.reference, "refactor_tool", args)
    time.sleep(1.01)
    actual = call(comp.native, "refactor_tool", args)
    assert actual["_graph"]["age_seconds"] > expected["_graph"]["age_seconds"]
    preview = comp.check_preview(actual, expected)
    sites = [(caller, 1, 17, "import"), (caller, 3, 9, "call"),
             (home, 1, 13, "__all__"), (home, 3, 5, "definition")]
    assert preview["edits"] == [
        dict(file=str(path), line=line, column=column, old="helper", new="double",
             site=site, confidence="high") for path, line, column, site in sites]
    assert comp.apply(preview["refactor_id"], str(root))["edits_applied"] == 4
    assert home.read_bytes() == b"__all__ = ['double']\n\ndef double(x):\n    return x * 2\n"
    assert caller.read_bytes() == b"from pkg import double\n\nVALUE = double(3)\n"


def main():
    import pytest
    opts = argparse.ArgumentParser(description=__doc__)
    opts.add_argument("--upstream", type=Path, required=True)
    opts.add_argument("--binary", type=Path, required=True)
    args = opts.parse_args()
    source, binary = args.upstream.resolve(), args.binary.resolve()
    pinned_reference(source)
    suite = source / "tests/test_rename_imports.py"
    if hashlib.sha256(suite.read_bytes()).hexdigest() != SUITE_HASH:
        raise ValueError("upstream rename suite changed")
    sys.path.insert(0, str(source))
    with tempfile.TemporaryDirectory(prefix="crg-rename-oracle-") as name:
        root = Path(name).resolve()
        stripped = ("PYTHON", "CRG_", "FASTMCP_")
        env = {k: v for k, v in os.environ.items() if not k.startswith(stripped)}
        if "PYTHONHASHSEED" in os.environ:
            env["PYTHONHASHSEED"] = os.environ["PYTHONHASHSEED"]
        env.update(CRG_HOME=str(root / "registry"), TZ="UTC")
        ref = server([sys.executable, "-m", "code_review_graph"], root,
                     dict(env, PYTHONPATH=str(source), PYTHONDONTWRITEBYTECODE="1"))
        native = server([str(binary)], root, env)
        try:
            comp = Comparison(ref, native)
            result = pytest.main([str(suite), "-q", "-p", "no:cacheprovider", "--tb=short",
                                  "--basetemp", str(root / "fixtures")], plugins=[comp])
            if result:
                raise SystemExit(result)
            from code_review_graph.graph import GraphStore
            from code_review_graph.incremental import full_build
            def build(repo):
                with GraphStore(repo / ".code-review-graph" / "graph.db") as store:
                    full_build(repo, store)
            guard_cases(comp, root / "stale", build)
            star_reexport_across_seconds(comp, root / "reexport", build)
            print("Native and Python stdio responses and file bytes matched", comp.counts)
        finally:
            native.close()
            ref.close()


if __name__ == "__main__":
    main()
