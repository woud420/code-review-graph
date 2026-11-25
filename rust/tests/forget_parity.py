"""Exercise forgetting and restoring public graph data with real referrers and vector rows."""
import argparse
import fcntl
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

from canonical_parity import read_graph
from differential import new_repo
from public_parity import pinned_reference, run

FILES={"pkg/lib.ts":"export function target() { return 1; }\n",
       "client.ts":'import {target} from "./pkg/lib.js";\nexport function caller(){ return target(); }\n',
       "untouched.py":"def sentinel():\n    return 42\n"}


def exercise(exe, root, env):
    db=root/".code-review-graph/graph.db"
    outputs={}
    def invoke(name,*argv,expected=0):
        observation=run([*exe,*argv,"--repo",str(root)],root,env)
        assert observation["exit"] == expected,(name,observation)
        outputs[name]=observation
    invoke("build","build","-q")
    for name,paths in [("miss",["absent"]),("folder",["pkg"]),("patterns",["*.ts","pkg/lib.ts"]),("abs",[str(root/"untouched.py")])]:
        before=db.read_bytes()
        invoke(name,"forget","--dry-run",*paths)
        assert db.read_bytes()==before
    with db.with_name("write.lock").open("w+b") as lock:
        lock.write(b"9876")
        lock.flush()
        fcntl.flock(lock.fileno(),fcntl.LOCK_EX)
        invoke("contended","forget","pkg",expected=75)
        invoke("preview with holder","forget","--dry-run","pkg")
    with sqlite3.connect(db) as c:
        c.execute("CREATE TABLE IF NOT EXISTS embeddings(qualified_name TEXT PRIMARY KEY,vector BLOB NOT NULL,text_hash TEXT NOT NULL,provider TEXT NOT NULL)")
        c.execute("INSERT INTO embeddings SELECT qualified_name,x'0000803f','fixture','test' FROM nodes WHERE name IN ('target','sentinel')")
    invoke("forget referrer","forget","pkg/lib.ts")
    assert (root/"pkg/lib.ts").read_text()==FILES["pkg/lib.ts"]
    outputs["after forget"]=read_graph(db,root)
    with sqlite3.connect(db) as c:
        assert not c.execute("SELECT count(*) FROM nodes WHERE name='target'").fetchone()[0]
        assert [Path(row[0]).name for row in c.execute("SELECT qualified_name FROM embeddings")]==["untouched.py::sentinel"]
        assert c.execute("SELECT value FROM metadata WHERE key='intentionally_incomplete'").fetchone()[0]=="1"
    invoke("query remaining","query","callees_of","caller")
    invoke("forget again","forget","pkg/lib.ts")
    invoke("no-op after forget","update","-q")
    outputs["unchanged update"]=read_graph(db,root)
    invoke("rebuild","build","-q")
    outputs["full restored"]=read_graph(db,root)
    assert outputs["full restored"]["symbols"].total()>outputs["after forget"]["symbols"].total()
    return outputs


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary",type=Path,required=True)
    p.add_argument("--upstream",type=Path,required=True)
    args=p.parse_args(); source=args.upstream.resolve()
    pinned_reference(source)
    env={k:v for k,v in os.environ.items() if not k.startswith(("CRG_","PYTHON","FASTMCP_"))}
    with tempfile.TemporaryDirectory(prefix="crg-forget-") as temp:
        temp=Path(temp).resolve(); repo=temp/"source"
        new_repo(repo,FILES)
        shutil.copytree(repo,temp/"original")
        opts=dict(env,CRG_HOME=str(temp/"registry"),CRG_SERIAL_PARSE="1",PYTHONDONTWRITEBYTECODE="1")
        python=exercise([sys.executable,"-m","code_review_graph"],repo,opts|{"PYTHONPATH":str(source)})
        shutil.rmtree(repo)
        shutil.copytree(temp/"original",repo)
        native=exercise([str(args.binary.resolve())],repo,opts)
        for key,left in python.items():
            right=native[key]
            if key in ("after forget","full restored","unchanged update"):
                for group in left:
                    assert left[group]==right[group],(key,group,(left[group]-right[group]).most_common(2),(right[group]-left[group]).most_common(2))
            else:
                assert left==right,(key,left,right)
    print(f"{len(python)} public forget lifecycle observations matched, including repaired graphs and read-only previews")


if __name__=="__main__":
    main()
