"""Exercise native stdio preview and apply with the pinned upstream rename acceptance suite."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time

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
    wire = client.call("tools/call", {"name": name, "arguments": args})
    assert not wire["notifications"], wire
    envelope = wire["response"]["result"]
    assert envelope["isError"] is False, envelope
    value = envelope["structuredContent"]
    assert envelope["content"] == [{"type": "text", "text": json.dumps(
        value, ensure_ascii=False, separators=(",", ":"))}], envelope
    return value


def normalized_preview(data):
    value = dict(data)
    if value.get("status") == "ok":
        identifier = value.pop("refactor_id")
        assert re.fullmatch("[0-9a-f]{8}", identifier)
        assert 0 <= time.time() - value.pop("created_at") < 60
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
        assert normalized_preview(actual) == normalized_preview(expected)
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
        # Replay both implementations against identical source bytes at the identical absolute paths.
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
        assert actual == expected
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
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PYTHON", "CRG_", "FASTMCP_"))}
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
            print("Native and Python stdio responses and file bytes matched", comp.counts)
        finally:
            native.close()
            ref.close()


if __name__ == "__main__":
    main()
