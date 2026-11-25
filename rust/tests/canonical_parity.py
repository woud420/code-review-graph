"""Compare the graphs the two public build commands actually write.

Nodes and edges are compared as multisets, retaining duplicate call sites. Surrogate
IDs are replaced by qualified names for flow/community membership. Timestamps and
storage-specific indexes are excluded from this comparison. The public CLI oracle
separately checks values (including IDs) visible in command and MCP responses.
"""
import argparse
import hashlib
from collections import Counter
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from differential import new_repo, verify_source


QUERIES = {
    "files": "SELECT file_path, file_hash, language FROM nodes WHERE kind='File'",
    "symbols": "SELECT kind, qualified_name, line_start, line_end FROM nodes",
    "declarations": """SELECT kind, name, qualified_name, parent_name, params, return_type,
        modifiers, is_test, symbol, docstring, name_tokens, signature FROM nodes""",
    "node_attributes": "SELECT qualified_name,extra FROM nodes",
    "edges": "SELECT kind, source_qualified, target_qualified, file_path, line FROM edges",
    "resolved_calls": """SELECT DISTINCT e.source_qualified,e.target_qualified FROM edges e
        JOIN nodes s ON s.qualified_name=e.source_qualified
        JOIN nodes t ON t.qualified_name=e.target_qualified WHERE e.kind='CALLS'""",
    "edge_attributes": """SELECT kind, source_qualified, target_qualified, file_path, line,
        confidence, confidence_tier, target_resolution, extra FROM edges""",
    "flows": """SELECT f.name, e.qualified_name, f.depth, f.node_count, f.file_count,
        f.criticality, m.position, n.qualified_name FROM flows f
        JOIN nodes e ON e.id=f.entry_point_id LEFT JOIN flow_memberships m ON m.flow_id=f.id
        LEFT JOIN nodes n ON n.id=m.node_id""",
    "communities": """SELECT c.name,c.level,c.cohesion,c.size,c.dominant_language,c.description,
        n.qualified_name FROM communities c LEFT JOIN nodes n ON c.id=n.community_id""",
}


def read_graph(path, root):
    result = {}
    with closing(sqlite3.connect(f"{path.as_uri()}?mode=ro", uri=True)) as db:
        for group, sql in QUERIES.items():
            rows = Counter()
            for record in db.execute(sql):
                values = list(record)
                if group in ("edge_attributes", "node_attributes"):
                    values[-1] = json.loads(values[-1] or "{}")
                key = json.dumps(values, sort_keys=True, ensure_ascii=False).replace(str(root) + "/", "")
                rows[key] += 1
            result[group] = rows
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    sample = parser.add_mutually_exclusive_group()
    sample.add_argument("--rust-tree", type=Path, help="compare every Rust source below a directory")
    sample.add_argument("--fixtures", action="store_true", help="compare the complete upstream multilingual fixture tree")
    args = parser.parse_args()
    source = args.upstream.resolve()
    verify_source(source)
    corpus = source
    if args.rust_tree:
        corpus = args.rust_tree.resolve(strict=True)
        paths = sorted((corpus / "src").rglob("*.rs")) if (corpus / "Cargo.toml").is_file() else sorted(corpus.rglob("*.rs"))
        if (corpus / "Cargo.toml").is_file():
            paths.append(corpus / "Cargo.toml")
        if not paths:
            raise ValueError(f"no Rust source in {corpus}")
    elif args.fixtures:
        paths = sorted(p for p in (source / "tests/fixtures").rglob("*") if p.is_file() and "__pycache__" not in p.parts)
        digest = hashlib.sha256()
        for p in paths:
            digest.update(p.relative_to(source).as_posix().encode() + b"\0" + hashlib.sha256(p.read_bytes()).digest())
        if digest.hexdigest() != "ed6b062abe5c3c1ac4e2ef2b632b2d04887001c1b9d7bc9b291123381b47af3b":
            raise ValueError("upstream fixture files differ from the pinned revision")
    else:
        paths = sorted((source / "code_review_graph").rglob("*.py"))
    files = {p.relative_to(corpus).as_posix(): p.read_text(encoding="utf-8") for p in paths}
    with tempfile.TemporaryDirectory(prefix="crg-canonical-") as tmp:
        directory = Path(tmp).resolve()
        root = directory / "source"
        new_repo(root, files)
        graphs = []
        env = {key: val for key, val in os.environ.items() if not key.startswith(("CRG_", "PYTHON", "FASTMCP_"))}
        env.update(CRG_SERIAL_PARSE="1", PYTHONDONTWRITEBYTECODE="1", PYTHONHASHSEED="0",
                   CRG_HOME=str(directory / "registry"))
        for name, command in [("python", [sys.executable, "-m", "code_review_graph"]),
                              ("rust", [str(args.binary.resolve())])]:
            data = directory / name
            options = dict(env, CRG_DATA_DIR=str(data))
            if name == "python":
                options["PYTHONPATH"] = str(source)
            subprocess.run([*command, "build", "--quiet", "--repo", str(root)], cwd=root,
                           env=options, check=True, timeout=600)
            graphs.append(read_graph(data / "graph.db", root))
    reference, candidate = graphs
    report = {}
    for group in QUERIES:
        missing = reference[group] - candidate[group]
        extra = candidate[group] - reference[group]
        report[group] = {"reference": reference[group].total(), "candidate": candidate[group].total(),
                         "missing": missing.total(), "extra": extra.total(),
                         "missing_examples": sorted(missing)[:5], "extra_examples": sorted(extra)[:5]}
        print(f"{group}: {reference[group].total()} reference, {candidate[group].total()} Rust,"
              f" {missing.total()} missing, {extra.total()} extra")
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return int(any(row["missing"] or row["extra"] for row in report.values()))


if __name__ == "__main__":
    sys.exit(main())
