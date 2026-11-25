"""Compare the port with the Python implementation on an evolving TypeScript repository.

Run with an interpreter containing upstream's dependencies:
  python tests/differential.py --upstream /path/to/pinned/source --binary /path/to/crg-rs

The input is adapted from upstream tests/test_incremental_reexport_regression.py.
The digest permits testing an unpacked source archive as well as a Git checkout.
"""

import argparse
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
from contextlib import closing
from pathlib import Path

UPSTREAM_REV = "7ba9d17ab6fd281026b914d8da4c0dc98d013474"
PYTHON_SOURCE_DIGEST = "db0b09474e3517e50d097a7d7a775348a1e7f2a1f7e517348302cc0931852f81"
SAMPLE_FILES = ["tests/fixtures/sample_python.py", "tests/fixtures/sample_rust.rs"]
FILES = {
    "fee.ts": "export function calculateFee(n: number) { return n + 2; }\n",
    "barrel.ts": 'export { calculateFee as fee } from "./fee.js";\n',
    "client.ts": 'import { calculateFee } from "./fee.js";\nexport const clientFee = calculateFee(8);\n',
    "use.ts": 'import { fee as charge } from "./barrel.js";\n'
              "export function invoice() { return charge(3); }\n"
              "export function nested() { return [1, 2].map(x => charge(x)); }\n",
    "unrelated.ts": "export function unrelated() { return 42; }\n"
                    "export function observe() { return unrelated(); }\n",
}
EXPECTED_CALLERS = ["client.ts", "use.ts::invoice", "use.ts::x"]


def equal(label, actual, expected):
    if actual != expected:
        raise AssertionError(f"{label}\n  expected {expected!r}\n  received {actual!r}")


def verify_source(source):
    package = source / "code_review_graph"
    digest = hashlib.sha256()
    files = sorted(package.rglob("*.py"))
    files.extend(source / path for path in ["tests/test_incremental_reexport_regression.py", *SAMPLE_FILES])
    for path in files:
        digest.update(path.relative_to(source).as_posix().encode() + b"\0")
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    equal(f"Python source fingerprint at {UPSTREAM_REV}", digest.hexdigest(), PYTHON_SOURCE_DIGEST)


def load_reference(source):
    verify_source(source)
    package = source / "code_review_graph"
    sys.path.insert(0, str(source))
    from code_review_graph.tools.build import build_or_update_graph
    from code_review_graph.tools.query import query_graph

    for name, module in sys.modules.items():
        if name == "code_review_graph" or name.startswith("code_review_graph."):
            if not Path(module.__file__).resolve().is_relative_to(package):
                raise AssertionError(f"oracle loaded {name} from {module.__file__}")
    return build_or_update_graph, query_graph


def git(repo, *arguments):
    subprocess.run(
        ["git", "-C", str(repo), "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false",
         "-c", "user.name=Oracle", "-c", "user.email=oracle@example.invalid", *arguments],
        check=True, capture_output=True, text=True, timeout=20,
    )


def write_tree(repo, files):
    repo.mkdir(exist_ok=True)
    for path in repo.glob("*.ts"):
        if path.name not in files:
            path.unlink()
    for name, source in files.items():
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists() or path.read_text(encoding="utf-8") != source:
            path.write_text(source, encoding="utf-8")


def new_repo(repo, files):
    write_tree(repo, files)
    git(repo, "init", "-q")
    git(repo, "add", "--", *files)
    git(repo, "commit", "-qm", "fixture")


def rust(binary, repo, *arguments):
    result = subprocess.run(
        [str(binary), *arguments, "--repo", str(repo)], capture_output=True, text=True, timeout=30,
    )
    if result.returncode:
        raise AssertionError(f"Rust {arguments} exited {result.returncode}: {result.stderr}")
    return json.loads(result.stdout)


def reference_state(repo):
    prefix = repo.as_posix() + "/"
    db = repo / ".code-review-graph" / "graph.db"
    with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)) as connection:
        files = sorted(path.removeprefix(prefix) for (path,) in connection.execute(
            "SELECT file_path FROM nodes WHERE kind='File'"
        ))
        symbols = sorted((kind, name.removeprefix(prefix), start, end)
                         for kind, name, start, end in connection.execute(
            "SELECT kind, qualified_name, line_start, line_end FROM nodes WHERE kind='Function'"
        ))
        calls = sorted((source.removeprefix(prefix), target.removeprefix(prefix), line)
                       for source, target, line in connection.execute(
            "SELECT source_qualified, target_qualified, line FROM edges "
            "WHERE kind = 'CALLS' AND target_qualified IN (SELECT qualified_name FROM nodes)"
        ))
    return files, symbols, calls


def rust_state(repo):
    def qualified(file, name):
        return file if name == "<module>" else f"{file}::{name}"

    db = repo / ".code-review-graph-rs" / "graph.db"
    with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)) as connection:
        files = sorted(path for (path,) in connection.execute("SELECT path FROM files"))
        symbols = sorted((kind, qualified(file, name), start, end)
                         for file, name, kind, start, end in connection.execute(
            "SELECT file, name, kind, start_line, end_line FROM symbols WHERE kind != 'File'"
        ))
        calls = sorted((qualified(file, name), qualified(target_file, target), line)
                       for file, name, target_file, target, line in connection.execute(
            "SELECT source_file, source, target_file, target, line FROM calls"
        ))
    return files, symbols, calls


def check_rollback(repo, binary):
    before = rust_state(repo)
    (repo / "a-valid.ts").write_text("export function added() { return 1; }\n", encoding="utf-8")
    invalid = repo / "z-invalid.ts"
    invalid.write_bytes(b"\xff")
    result = subprocess.run([str(binary), "update", "--repo", str(repo)],
                            capture_output=True, text=True, timeout=30)
    if not result.returncode or "z-invalid.ts" not in result.stderr:
        raise AssertionError(f"expected the invalid input to fail: {result}")
    equal("failed update preserved the previous graph", rust_state(repo), before)
    invalid.write_text("", encoding="utf-8")
    updated = rust(binary, repo, "update")
    equal("retry picked up both new files", updated["files_parsed"], 2)
    hits = rust(binary, repo, "query", "symbols", "a-valid.ts::added")["results"]
    equal("recovered symbol", [(row["file"], row["name"]) for row in hits], [("a-valid.ts", "added")])
    print("failure/retry: an invalid input rolled back the Rust update; correction indexed both files")


def check_build(repo, command, binary, upstream_build, postprocess="none"):
    result = upstream_build(repo_root=str(repo), full_rebuild=command == "build", postprocess=postprocess)
    equal("reference errors", result["errors"], [])
    equal("reference status", result["status"], "ok")
    equal("reference build type", result["build_type"], "full" if command == "build" else "incremental")
    return result, rust(binary, repo, command)


def check_queries(repo, symbol, binary, upstream_query):
    expected = {"calculateFee": [], "computeFee": [], "unrelated": ["unrelated.ts::observe"]}
    if symbol:
        expected[symbol] = EXPECTED_CALLERS
    for target, wanted in expected.items():
        check_callers(repo, target, wanted, binary, upstream_query)


def check_callers(repo, target, wanted, binary, upstream_query):
    upstream_target = str(repo / target) if "::" in target else target
    result = upstream_query("callers_of", upstream_target, repo_root=str(repo))
    equal(f"Python query status for {target}", result["status"], "ok" if wanted else "not_found")
    actual = sorted(row["qualified_name"].removeprefix(repo.as_posix() + "/")
                    for row in result.get("results", []))
    equal(f"Python callers of {target}", actual, wanted)
    rows = rust(binary, repo, "query", "callers-of", target)["results"]
    projected = sorted(row["file"] if row["kind"] == "File" else f"{row['file']}::{row['name']}"
                       for row in rows)
    equal(f"Rust and Python callers of {target}", projected, actual)


def report_package_overlap(repo):
    prefix = str(repo) + "/"
    package_prefix = "code_review_graph/"
    database = repo / ".code-review-graph/graph.db"
    with closing(sqlite3.connect(f"{database.as_uri()}?mode=ro", uri=True)) as connection:
        reference = {(caller.removeprefix(prefix), callee.removeprefix(prefix))
                     for caller, callee in connection.execute("""
            SELECT DISTINCT e.source_qualified, e.target_qualified FROM edges e
            JOIN nodes src ON src.qualified_name=e.source_qualified
            JOIN nodes dst ON dst.qualified_name=e.target_qualified
            WHERE e.kind='CALLS' AND src.language='python' AND dst.language='python'
        """)}
    database = repo / ".code-review-graph-rs/graph.db"
    with closing(sqlite3.connect(f"{database.as_uri()}?mode=ro", uri=True)) as connection:
        candidate = set(connection.execute("""
            SELECT DISTINCT
                CASE WHEN src.kind='File' THEN src.file ELSE src.file||'::'||src.name END,
                CASE WHEN dst.kind='File' THEN dst.file ELSE dst.file||'::'||dst.name END
            FROM calls c
            JOIN symbols src ON src.file=c.source_file AND src.name=c.source
            JOIN symbols dst ON dst.file=c.target_file AND dst.name=c.target
        """))
    reference = {(s, t) for s, t in reference if s.startswith(package_prefix) and t.startswith(package_prefix)}
    candidate = {(s, t) for s, t in candidate if s.startswith(package_prefix) and t.startswith(package_prefix)}
    shared = reference & candidate
    print(f"unique bound production caller/callee pairs: Python {len(reference)}, Rust {len(candidate)}, "
          f"shared {len(shared)} ({len(shared)/len(reference):.1%} of Python, "
          f"{len(shared)/len(candidate):.1%} of Rust)")
    for label, pairs in (("Python only", reference - candidate), ("Rust only", candidate - reference)):
        cross = sum(src.partition("::")[0] != dst.partition("::")[0] for src, dst in pairs)
        print(f"  {label}: {cross} cross-file and {len(pairs) - cross} same-file pairs")
        for src, dst in sorted(pairs)[:2]:
            print(f"    {src} -> {dst}")
    equal("all distinct directly bound production Python caller/callee pairs", candidate, reference)


def check_upstream_examples(binary, source, tmp, build, query, whole_package):
    paths = [source / path for path in SAMPLE_FILES]
    if whole_package:
        paths.extend(sorted((source / "code_review_graph").rglob("*.py")))
    else:
        paths.append(source / "code_review_graph/context_savings.py")
    files = {p.relative_to(source).as_posix(): p.read_text(encoding="utf-8") for p in paths}
    repo = tmp / "upstream-examples"
    new_repo(repo, files)
    py, rs = check_build(repo, "build", binary, build, "full" if whole_package else "none")
    equal("all upstream example files indexed by Python", py["files_parsed"], len(files))
    equal("all upstream example files indexed by Rust", rs["files"], len(files))
    queries = {
        "tests/fixtures/sample_rust.rs::create_user": ["tests/fixtures/sample_rust.rs::create_user_saves_to_repo"],
        "tests/fixtures/sample_python.py::process_request": ["tests/fixtures/sample_python.py::guarded_process"],
        "code_review_graph/context_savings.py::_savings": [
            "code_review_graph/context_savings.py::attach_context_savings",
            "code_review_graph/context_savings.py::estimate_context_savings",
        ],
    }
    for target, wanted in queries.items():
        check_callers(repo, target, wanted, binary, query)
        print(f"{target}: {len(wanted)} exact caller(s) matched across a {len(files)}-file index")
    if whole_package:
        report_package_overlap(repo)


def run(binary, upstream, temp, whole_package):
    build, query = load_reference(upstream)
    files = dict(FILES)
    repo = temp / "incremental"
    new_repo(repo, files)
    for stage in ("build", "rename", "no-op", "delete", "restore"):
        if stage == "rename":
            for name in ("fee.ts", "barrel.ts", "client.ts"):
                files[name] = files[name].replace("calculateFee", "computeFee")
        if stage == "delete":
            del files["fee.ts"]
        if stage == "restore":
            files["fee.ts"] = FILES["fee.ts"].replace("calculateFee", "computeFee")
        if stage in {"rename", "delete", "restore"}:
            write_tree(repo, files)
            git(repo, "add", "-u")
            if stage == "restore":
                git(repo, "add", "--", "fee.ts")
            git(repo, "commit", "-qm", stage)

        command = "build" if stage == "build" else "update"
        py_report, rs_report = check_build(repo, command, binary, build)
        fresh = temp / f"fresh-{stage}"
        new_repo(fresh, files)
        check_build(fresh, "build", binary, build)
        symbol = "calculateFee" if stage == "build" else "computeFee"
        if stage == "delete":
            symbol = None
        for tree in (repo, fresh):
            check_queries(tree, symbol, binary, query)
        expected = reference_state(fresh)
        reference = reference_state(repo)
        if stage == "restore":
            # Python answers from its bare-name fallback after the deleted target returns.
            equal("restored reference file and symbol inventory", reference[:2], expected[:2])
            equal("pinned reference retains one resolved call after restore", reference[2], [
                ("unrelated.ts::observe", "unrelated.ts::unrelated", 2)
            ])
        else:
            equal("Python update vs clean graph", reference, expected)
        equal("Rust clean build vs Python clean build", rust_state(fresh), expected)
        equal("Rust update vs Python clean build", rust_state(repo), expected)
        equal("tracked file count", rs_report["files"], len(files))
        if stage == "no-op":
            equal("Python duplicate notification parse count", py_report["files_updated"], 0)
            equal("Rust duplicate notification parse count", rs_report["files_parsed"], 0)
            equal("Rust duplicate notification delete count", rs_report["files_deleted"], 0)
        if stage == "delete":
            equal("Rust deleted file count", rs_report["files_deleted"], 1)
        print(f"{stage}: {len(files)} files, {len(expected[1])} functions, "
              f"{len(expected[2])} resolved call sites match a clean Python build")
        if stage == "restore":
            print("  The Python update recovered the three public callers via bare-name fallback.")
    check_rollback(repo, binary)
    check_upstream_examples(binary, upstream, temp, build, query, whole_package)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--whole-package", action="store_true",
                        help="index every upstream production Python module for the example queries")
    args = parser.parse_args()
    for name in list(os.environ):
        if name.startswith("CRG_"):
            del os.environ[name]
    os.environ["CRG_SERIAL_PARSE"] = "1"
    os.environ["GIT_CONFIG_NOSYSTEM"] = "1"
    os.environ["GIT_CONFIG_GLOBAL"] = os.devnull
    with tempfile.TemporaryDirectory(prefix="crg-oracle-") as tmp:
        run(args.binary.resolve(), args.upstream.resolve(), Path(tmp).resolve(), args.whole_package)


if __name__ == "__main__":
    main()
