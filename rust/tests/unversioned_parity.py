"""Check filesystem ordering when building a Rust project outside version control."""
import argparse
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import verify_source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,required=True)
    parser.add_argument("--upstream",type=Path,required=True)
    args=parser.parse_args()
    source,binary=args.upstream.resolve(),args.binary.resolve()
    verify_source(source)
    with tempfile.TemporaryDirectory(prefix="crg-unversioned-") as folder:
        base=Path(folder).resolve()
        root=base/"corpus"
        (root/".code-review-graph").mkdir(parents=True)
        for rel in ("lib.rs","core/z.rs","core/d.rs","app/b.rs","core/a.rs",
                    "app/sub/z.rs","core/sub/m.rs","app/sub/a.rs","core/sub/j.rs"):
            path=root/rel
            path.parent.mkdir(exist_ok=True,parents=True)
            path.write_text(f"pub fn name_{path.stem}() {{}}\n")
        (root/"Cargo.toml").write_text('[package]\nname="graph-check"\nversion="0.1.0"\n')
        obs=[]
        for name,command in [("reference",[sys.executable,"-m","code_review_graph"]),("native",[str(binary)])]:
            db=base/name/"graph.db"
            env={k:v for k,v in os.environ.items() if k in ("PATH","TMP","TEMP","TMPDIR")}
            env.update(HOME=str(base),CRG_DATA_DIR=str(db.parent),CRG_HOME=str(base/"registry"),
                       CRG_SERIAL_PARSE="1",PYTHONDONTWRITEBYTECODE="1",PYTHONHASHSEED="0")
            if name=="reference": env["PYTHONPATH"]=str(source)
            subprocess.run([*command,"build","--quiet","--repo",str(root)],cwd=root,env=env,check=True)
            with sqlite3.connect(db) as conn:
                order=conn.execute("SELECT qualified_name FROM nodes WHERE kind='File' ORDER BY id").fetchall()
            assert len(order)==9,order
            obs.append((order,read_graph(db,root)))
        assert obs[0][0]==obs[1][0],obs
        for k,wanted in obs[0][1].items(): assert wanted==obs[1][1][k],k
    print("Unversioned Rust tree: file insertion order, all nine graph projections and community names matched")


if __name__=="__main__":
    main()
