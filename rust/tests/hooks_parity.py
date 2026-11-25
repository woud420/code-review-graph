"""Compare native hook output and repository selection with the pinned Python CLI."""
import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import shlex
import sqlite3
import subprocess
import sys
import tempfile

from differential import git, new_repo, verify_source

SERVICE = """def charge():
    return 2

def compute_invoice():
    return charge()

def café_entry():
    return compute_invoice()
"""
FIXTURE = {
    ".gitignore": ".code-review-graph/\n.code-review-graph.db\n",
    "service.py": SERVICE,
    "tests/test_service.py": "from service import compute_invoice\n\ndef test_invoice():\n    assert compute_invoice() == 2\n",
}


def db(repo):
    return repo / ".code-review-graph/graph.db"


def symbols(database):
    assert database.is_file(), database
    # immutable reads make omitted writes and any stray -wal/-shm observable.
    with closing(sqlite3.connect(database.as_uri() + "?immutable=1", uri=True)) as conn:
        return {row[0] for row in conn.execute("SELECT name FROM nodes")}


def run_suite(scratch, name, command, upstream, shims):
    scratch.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "FASTMCP_"))}
    env.update(CRG_HOME=str(scratch / "state"), PYTHONDONTWRITEBYTECODE="1", PYTHONPATH=str(upstream),
               PATH=str(shims) + os.pathsep + env["PATH"], HOME=str(scratch / "home"))
    Path(env["HOME"]).mkdir()
    session, sibling, blank, linked = (scratch / x for x in ("main", "sibling", "blank", "linked"))
    for repo in (session, sibling, blank):
        new_repo(repo, FIXTURE)

    def invoke(*args, event=None, cwd=sibling, extra=None):
        data = event if isinstance(event, bytes) else json.dumps(event, ensure_ascii=False).encode()
        return subprocess.run([*command, *map(str, args)], input=data, cwd=cwd, env=env | (extra or {}),
                              capture_output=True, timeout=45)

    def hook(verb, event, *args, cwd=sibling, extra=None):
        result = invoke(verb, *args, event=event, cwd=cwd, extra=extra)
        assert result.returncode == 0, (name, verb, event, result.stderr)
        return result

    def update(cwd, path=None, *, existing=False, notebook=False, extra=None):
        event = {"cwd": str(cwd)} if cwd is not None else {}
        if path is not None:
            event["tool_input"] = {"notebook_path" if notebook else "file_path": str(path)}
        result = hook("hook-update", event, *(["--existing-only"] if existing else []), extra=extra)
        assert result.stdout == b"", (name, result.stdout)

    def append(repo, symbol):
        with (repo / "service.py").open("a", encoding="utf-8") as f:
            f.write(f"\ndef {symbol}():\n    return 123\n")

    help_pages = {}
    for verb in ("hook-update", "hook-status", "enrich"):
        result = invoke(verb, "--help")
        assert result.returncode == 0 and not result.stderr and b"event read from stdin" in b" ".join(result.stdout.split())
        help_pages[verb + " --help"] = result.stdout

    for event in (b"", b"{", b"[]", b"null", b'{"cwd": 42}', {}, {"cwd": "relative"},
                  {"cwd": str(scratch / "missing")}, {"cwd": str(scratch)}):
        for verb in ("hook-status", "hook-update", "enrich"):
            if verb == "enrich" and event == b'{"cwd": 42}':
                invalid = invoke(verb, event=event)
                assert invalid.returncode != 0 and invalid.stderr and not invalid.stdout
                continue
            result = hook(verb, event)
            assert not result.stdout and not result.stderr, (name, verb, event, result.stderr)
    update(session, existing=True)
    update(None, "service.py")
    update(session, sibling / "service.py")
    assert all(not db(r).exists() for r in (session, sibling, blank))
    status = hook("hook-status", {"cwd": str(session)})
    direct = invoke("status", "--repo", session)
    assert (status.stdout, status.stderr) == (direct.stdout, direct.stderr)
    assert b"No graph found" in status.stdout + status.stderr

    update(session / "tests", "../service.py")
    assert "compute_invoice" in symbols(db(session)) and not db(sibling).exists()
    update(sibling)
    append(session, "pending_main")
    append(sibling, "only_sibling")
    update(session, sibling / "service.py", existing=True)
    assert "only_sibling" in symbols(db(sibling)) and "pending_main" not in symbols(db(session))
    append(sibling, "absolute_without_cwd")
    update(None, sibling / "disappeared/folder/deleted.py")
    assert "absolute_without_cwd" in symbols(db(sibling))
    update(session, scratch / "unowned/file.py")
    assert "pending_main" not in symbols(db(session))
    status = hook("hook-status", {"cwd": str(sibling / "tests")})
    direct = invoke("status", "--repo", sibling)
    assert status.stderr == direct.stderr and status.stdout == direct.stdout and b"Nodes" in status.stdout, (name, status.stdout, direct.stdout, status.stderr, direct.stderr)

    git(session, "worktree", "add", "-q", "-b", "linked", str(linked))
    update(session, linked / "service.py")
    assert not db(linked).exists()
    update(linked)
    append(linked, "linked_worktree_only")
    update(session, linked / "service.py", notebook=True)
    assert "linked_worktree_only" in symbols(db(linked)) and "pending_main" not in symbols(db(session))
    git(session, "-c", "protocol.file.allow=always", "submodule", "add", "-q", str(blank), "vendor/inner")
    update(session, session / "vendor/inner/service.py")
    assert "pending_main" in symbols(db(session)) and not db(session / "vendor/inner").exists()
    print(name + ": isolated worktrees, cross-repository edits, file removal, submodule fallback and hook status passed", flush=True)

    shared, owner = scratch / "shared ?#% données", scratch / "owner"
    new_repo(owner, FIXTURE)
    options = {"CRG_DATA_DIR": str(shared)}
    update(owner, extra=options)
    shared_db = shared / "graph.db"
    assert "compute_invoice" in symbols(shared_db)
    before = sorted(p.name for p in shared.iterdir())
    original = shared_db.read_bytes()
    update(blank, existing=True, extra=options)
    assert before == sorted(p.name for p in shared.iterdir()) and original == shared_db.read_bytes()
    assert not db(blank).exists()
    append(owner, "shared_owner_symbol")
    update(owner, existing=True, extra=options)
    assert "shared_owner_symbol" in symbols(shared_db)
    legacy = scratch / "legacy"
    new_repo(legacy, FIXTURE)
    update(legacy)
    old = legacy / ".code-review-graph.db"
    db(legacy).rename(old)
    append(legacy, "legacy_migrated")
    update(legacy, existing=True)
    assert not old.exists() and "legacy_migrated" in symbols(db(legacy))
    print(name + ": shared graph owner guard, pristine foreign WAL directory and legacy migration passed", flush=True)

    # Both engines enrich independently built full graphs.
    full = invoke("build", "--repo", sibling, "--quiet")
    assert full.returncode == 0, (name, full.stderr)
    outcomes = help_pages
    def enrich(label, tool, data, *, match=False, cwd=sibling, repo=sibling, extra=None):
        event = {"cwd": str(repo / "tests"), "tool_name": tool, "tool_input": data}
        result = hook("enrich", event, cwd=cwd, extra=extra)
        assert not result.stderr and bool(result.stdout) == match, (name, label, result.stderr, result.stdout)
        if match:
            record = json.loads(result.stdout)["hookSpecificOutput"]
            assert record["hookEventName"] == "PreToolUse"
            assert "[code-review-graph]" in record["additionalContext"]
        outcomes[label] = result.stdout.replace(os.fsencode(scratch), b"<root>")
    for label, tool, data in (("grep", "Grep", {"pattern":"compute_invoice"}), ("glob", "Glob", {"pattern":"**/compute_invoice*.py"}),
                              ("bash", "Bash", {"command":"rg --glob '*.py' -n 'compute_invoice' ."}),
                              ("absolute read", "Read", {"file_path":str(sibling / "service.py")}),
                              ("relative read", "Read", {"file_path":"service.py"}), ("test read", "Read", {"file_path":"tests/test_service.py"})):
        enrich(label, tool, data, match=True)
    assert b"Called by:" in outcomes["grep"] and b"Calls:" in outcomes["grep"] and b"Flows:" in outcomes["grep"]
    assert b"Tests: test_invoice" in outcomes["grep"] and b"\\u00e9" in outcomes["absolute read"]
    for label, tool, data in (("foreign read", "Read", {"file_path":str(session / "service.py")}),
                              ("unknown", "Write", {"file_path":str(sibling / "service.py")}),
                              ("short", "Grep", {"pattern":"ab"}), ("no result", "Grep", {"pattern":"totallymissing"}),
                              ("simple glob", "Glob", {"pattern":"**/*.ts"}), ("no grep", "Bash", {"command":"echo compute_invoice"}),
                              ("bad input", "Grep", [])):
        enrich(label, tool, data)
    enrich("external-only", "Grep", {"pattern":"compute_invoice"}, repo=owner, extra=options)
    print(name + ": Grep/Glob/Bash/Read, real call/coverage/flow metadata and graph boundaries passed", flush=True)
    return outcomes


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    args = p.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-hooks-") as temp:
        scratch = Path(temp).resolve()
        shims = scratch / "shims"
        shims.mkdir()
        shim = shims / "code-review-graph"
        shim.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + ' -m code_review_graph "$@"\n')
        shim.chmod(0o755)
        expected = run_suite(scratch / "python", "Python", [sys.executable, "-m", "code_review_graph"], upstream, shims)
        actual = run_suite(scratch / "rust", "Rust", [str(binary)], upstream, shims)
        for query in expected:
            assert expected[query] == actual[query], (query, expected[query], actual[query])
        print(f"{len(expected)} help and enrichment responses matched byte for byte")


if __name__ == "__main__":
    main()
