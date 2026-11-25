"""Compare native derived tables and full-text queries to a pinned base Python install."""

import argparse
import os
import sqlite3
import subprocess
import tempfile
from contextlib import closing
from pathlib import Path

from differential import equal, load_reference, new_repo


FILES = {
    "api/entry.py": (
        "from inventory.logic import price_total\n"
        "\ndef handle_order():\n    return invoice_amount()\n"
        "\ndef invoice_amount():\n    return price_total()\n"
    ),
    "inventory/logic.py": (
        "def price_total():\n    return price_units()\n"
        "\ndef price_units():\n    return sum([2, 3])\n"
    ),
}


def snapshot(db):
    with closing(sqlite3.connect(db)) as conn:
        def rows(sql):
            return list(conn.execute(sql))

        return {
            "signatures": rows("SELECT qualified_name,signature FROM nodes ORDER BY qualified_name"),
            "index": rows("SELECT n.qualified_name FROM nodes_fts JOIN nodes n ON n.id=nodes_fts.rowid "
                          "WHERE nodes_fts MATCH 'invoice' ORDER BY n.qualified_name"),
            "fts_state": rows("SELECT qualified_name,signature FROM nodes_fts_state ORDER BY qualified_name"),
            "flows": rows("SELECT f.name,n.qualified_name,f.depth,f.node_count,f.file_count,f.criticality "
                          "FROM flows f JOIN nodes n ON n.id=f.entry_point_id ORDER BY n.qualified_name"),
            "steps": rows("SELECT ep.qualified_name,m.position,n.qualified_name FROM flow_memberships m "
                          "JOIN flows f ON f.id=m.flow_id JOIN nodes n ON n.id=m.node_id "
                          "JOIN nodes ep ON ep.id=f.entry_point_id ORDER BY ep.qualified_name,m.position"),
            "communities": rows("SELECT name,cohesion,size,dominant_language,description FROM communities ORDER BY name"),
            "members": rows("SELECT c.name,n.qualified_name FROM nodes n JOIN communities c ON c.id=n.community_id "
                            "ORDER BY c.name,n.qualified_name"),
            "metadata": rows("SELECT key,value FROM metadata WHERE key IN ('build_state','community_detector',"
                             "'fts_state_synced','postprocess_level') ORDER BY key"),
        }


def run(source, binary, root):
    build, _ = load_reference(source)
    repo = root / "fixture"
    new_repo(repo, FILES)
    db = repo / ".code-review-graph" / "graph.db"
    for level, flags in [("minimal", ["--skip-flows"]), ("full", [])]:
        reference = build(repo_root=str(repo), full_rebuild=True, postprocess=level)
        equal(f"Python {level} status", reference["status"], "ok")
        expected = snapshot(db)
        if not expected["index"]:
            raise AssertionError(f"{level} produced no searchable invoice term")
        if level == "full" and (len(expected["communities"]) < 2 or len(expected["steps"]) < 3):
            raise AssertionError("fixture must span multiple nontrivial communities and flow steps")
        db.parent.rename(root / f"python-{level}")
        subprocess.run([str(binary), "build", "--repo", str(repo), *flags], check=True, capture_output=True, text=True)
        actual = snapshot(db)
        for key, rows in expected.items():
            equal(f"{level} {key}", actual[key], rows)
        print(f"{level}: {len(actual['signatures'])} signatures, {len(actual['steps'])} flow memberships, "
              f"{len(actual['members'])} community memberships; exact derived rows and FTS query")
        db.parent.rename(root / f"rust-{level}")


if __name__ == "__main__":
    args = argparse.ArgumentParser(description=__doc__)
    args.add_argument("--upstream", type=Path, required=True)
    args.add_argument("--binary", type=Path, required=True)
    config = args.parse_args()
    os.environ["CRG_SERIAL_PARSE"] = "1"
    with tempfile.TemporaryDirectory(prefix="crg-postprocess-") as scratch:
        run(config.upstream.resolve(), config.binary.resolve(), Path(scratch).resolve())
