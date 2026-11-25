"""Compare build/update symlink inventories, root boundaries and optional enrichment behavior."""
import argparse
from contextlib import closing
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from differential import git, verify_source


def observe(root, command, env):
    outside = root.parent / "outside"
    outside.mkdir()
    for name in ("app", "shared", "other"):
        (root / name).mkdir(parents=True)
    def source(path, name):
        path.write_text(f"def {name}(): return 42\n")
    source(root / "app/main.py", "main")
    source(root / "shared/util.py", "util")
    source(root / "other/extra.py", "extra")
    source(outside / "secret.py", "secret")
    (root / ".gitignore").write_text("/shared/\n/other/\n.code-review-graph/\n")
    (root / "app/shared").symlink_to("../shared",target_is_directory=True)
    (root / "app/up").symlink_to("..",target_is_directory=True)
    (root / "app/outside").symlink_to(outside,target_is_directory=True)
    (root / "app/home").symlink_to(root.parent,target_is_directory=True)
    (root / "shared/loop").symlink_to(".",target_is_directory=True)
    (root / "shared/to_other").symlink_to("../other",target_is_directory=True)
    (root / "other/back").symlink_to("../shared",target_is_directory=True)
    (root / "shared/alias.py").symlink_to("util.py")
    git(root,"init","-q")
    git(root,"add",".")
    git(root,"commit","-qm","links")
    db=root/".code-review-graph/graph.db"
    results={}
    def check(label, op, flags, expected, *, extra=None, enrichment=None):
        p=subprocess.run([*command, op, "--repo",str(root),"--skip-postprocess",*flags],
                         env=env|dict(extra or {}), cwd=root, capture_output=True, text=True, timeout=30)
        assert p.returncode==0,(label,p.stdout,p.stderr)
        with closing(sqlite3.connect(db)) as conn:
            files={Path(p).relative_to(root).as_posix() for p, in conn.execute("SELECT file_path FROM nodes WHERE kind='File'")}
            assert files==expected,(label,files,expected,p.stdout,p.stderr)
            assert all(str(outside) not in v for row in conn.execute("SELECT source_qualified,target_qualified FROM edges") for v in row)
            metadata=dict(conn.execute("SELECT key,value FROM metadata WHERE key LIKE 'symlink_followed_%'"))
            followed={Path(p).relative_to(root).as_posix() for p in json.loads(metadata.get("symlink_followed_files","[]"))}
            assert followed==files-{"app/main.py"},(label,followed,files)
            dirs={Path(p).relative_to(root.parent).as_posix():v for p,v in json.loads(metadata.get("symlink_followed_dirs","{}")).items()}
            names=sorted(x for x, in conn.execute("SELECT name FROM nodes WHERE kind='Function'"))
        skip="Jedi not installed, skipping Python enrichment" in p.stderr
        if enrichment is not None:
            assert skip==enrichment,(label,p.stderr)
        # Progress timings and one-time loop diagnostics vary. The public summary and graph do not.
        results[label]=(p.stdout,sorted(files),sorted(followed),dirs,names)
    off={"app/main.py"}
    on=off|{"shared/util.py","other/extra.py","app/outside/secret.py"}
    check("default","build",[],off)
    check("enabled","build",["--follow-symlinks"],on)
    check("disabled","update",["--no-follow-symlinks"],off,extra={"CRG_FOLLOW_SYMLINKS":"1"})
    check("env enabled","update",[],on,extra={"CRG_FOLLOW_SYMLINKS":"yes"})
    source(outside/"secret.py","replacement")
    check("external edit","update",["--follow-symlinks"],on)
    (root/"app/outside").unlink()
    check("detached","update",["--follow-symlinks"],on-{"app/outside/secret.py"})
    (root/"app/outside").symlink_to(outside,target_is_directory=True)
    check("capped","build",["--follow-symlinks"],off|{"app/outside/secret.py"},extra={"CRG_MAX_FOLLOWED_FILES":"1"})
    missing_jedi=importlib.util.find_spec("jedi") is None
    check("enrich disabled","build",["--enrich"],off,extra={"CRG_DISABLE_ENRICHMENT":"1"},enrichment=False)
    check("enrich requested","build",["--enrich"],off,enrichment=True if missing_jedi else None)
    check("enrich env","build",[],off,extra={"CRG_ENABLE_ENRICHMENT":"1"},enrichment=True if missing_jedi else None)
    check("enrich no edits","update",["--enrich"],off,enrichment=False)
    return results


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream",type=Path,required=True);p.add_argument("--binary",type=Path,required=True)
    args=p.parse_args()
    upstream,binary=args.upstream.resolve(),args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-links-") as tmp:
        results=[]
        for name,command in (("python",[sys.executable,"-m","code_review_graph"]),("rust",[str(binary)])):
            cwd=Path(tmp).resolve()/name
            cwd.mkdir()
            env={k:v for k,v in os.environ.items() if not k.startswith(("CRG_","PYTHON","FASTMCP_"))}
            env.update(CRG_HOME=str(cwd/"state"),PYTHONPATH=str(upstream),PYTHONDONTWRITEBYTECODE="1")
            results.append(observe(cwd/"repo",command,env))
        for label,expected in results[0].items():
            assert expected==results[1][label],(label,expected,results[1][label])
        print(f"{len(results[0])} build/update scenarios matched, including CLI summaries, followed-path metadata, loop boundaries, caps and enrichment controls")


if __name__=="__main__":
    main()
