"""Open and update databases created by released Python wheels, across both executables."""
import argparse
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import new_repo, verify_source

RELEASES = {"2.3.7": 9, "2.3.8": 9, "2.3.9": 13}


def observe(command, root, env, *args):
    proc = subprocess.run([*command, *args], cwd=root, env=env, capture_output=True, timeout=90)
    assert proc.returncode == 0, (args, proc.stderr.decode())
    return proc.stdout, proc.stderr


def schema(db):
    with sqlite3.connect(db) as conn:
        version = conn.execute("SELECT value FROM metadata WHERE key='schema_version'").fetchone()[0]
        ids = dict(conn.execute("SELECT qualified_name,id FROM nodes"))
        return int(version), ids


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wheels", type=Path, required=True, help="unpacked release wheels, one directory per version")
    args = parser.parse_args()
    source, binary, wheels = args.upstream.resolve(), args.binary.resolve(), args.wheels.resolve()
    verify_source(source)
    py = [sys.executable, "-m", "code_review_graph"]
    for release, old_version in RELEASES.items():
        legacy = wheels / release
        assert (legacy / f"code_review_graph-{release}.dist-info/METADATA").is_file(), legacy
        with tempfile.TemporaryDirectory(prefix="crg-upgrade-") as temp:
            parent = Path(temp).resolve()
            root = parent / "repo"
            new_repo(root, {"main.py": "def double(n):\n    return n * 2\n\ndef main():\n    return double(4)\n",
                            "lib.rb": "def twice(n)\n  n * 2\nend\n"})
            env = {k: v for k, v in os.environ.items() if k in ("PATH", "TMPDIR", "TMP", "TEMP")}
            env.update(HOME=str(parent), PYTHONDONTWRITEBYTECODE="1", PYTHONHASHSEED="0", CRG_SERIAL_PARSE="1",
                       CRG_HOME=str(parent/"registry"), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
            observe(py, root, env | {"PYTHONPATH":str(legacy)}, "build", "--repo", str(root))
            data = root / ".code-review-graph"
            db = data / "graph.db"
            assert schema(db)[0] == old_version
            original = schema(db)[1]
            assert original
            seed = parent/"old"
            data.rename(seed)
            results = []
            for command, runtime in [(py, env | {"PYTHONPATH":str(source)}), ([str(binary)], env)]:
                shutil.copytree(seed, data)
                status = observe(command, root, runtime, "status", "--repo", str(root), "--json")
                assert schema(db) == (15, original), (release,command,status,schema(db),original)
                before = read_graph(db,root)
                search = observe(command, root, runtime, "search", "double", "--repo", str(root))
                update = observe(command, root, runtime, "update", "--repo", str(root), "--quiet")
                observations = dict(status=status, opened=before, search=search, update=update, updated=read_graph(db,root))
                shutil.rmtree(data)
                for verb, options in [("postprocess", []), ("update", ["--quiet"]), ("forget", ["lib.rb"])]:
                    shutil.copytree(seed, data)
                    observations[verb + " first"] = observe(command, root, runtime, verb, "--repo", str(root), *options)
                    assert schema(db)[0] == 15
                    observations[verb + " first graph"] = read_graph(db, root)
                    shutil.rmtree(data)
                shutil.copytree(seed, data)
                snapshot = db.read_bytes()
                protected = subprocess.run([*command, "status", "--repo", str(root)], cwd=root,
                                           env=runtime | {"CRG_READ_ONLY": "true"}, capture_output=True, timeout=30)
                assert protected.returncode and b"a read-only open cannot upgrade it" in protected.stderr, protected
                assert db.read_bytes() == snapshot
                shutil.rmtree(data)
                results.append(observations)
            for label, a in results[0].items():
                b = results[1][label]
                if isinstance(a, dict):
                    for key in a:
                        assert a[key] == b[key], (release,label,key,list((a[key]-b[key]).items())[:2],list((b[key]-a[key]).items())[:2])
                else:
                    assert a == b, (release,label,a,b)
            print(f"{release} schema {old_version} → 15: read-only protection, preserved IDs, first-op upgrades and graph snapshots matched")


if __name__ == "__main__":
    main()
