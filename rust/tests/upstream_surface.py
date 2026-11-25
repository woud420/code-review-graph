"""Run the pinned release's own command and flag cases through both installed programs."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

from differential import git
from public_parity import pinned_reference, run

SUITE_HASH = "801e7da0015e85b797927d57d301bafcfc3a828bc6bf3acabb1d396ec2e53103"


def suite(source):
    path = source / "tests/test_cli_surface.py"
    if hashlib.sha256(path.read_bytes()).hexdigest() != SUITE_HASH:
        raise ValueError("upstream CLI suite changed")
    spec = importlib.util.spec_from_file_location("upstream_surface_fixture", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def equal_impact_files(first, second):
    if (first["stderr"],first["exit"]) != (second["stderr"],second["exit"]):
        return False
    a,b = (json.loads(observation["stdout"]) for observation in (first,second))
    files_a, files_b = a.pop("impacted_files"), b.pop("impacted_files")
    return a == b and sorted(files_a) == sorted(files_b)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    source = args.upstream.resolve()
    pinned_reference(source)
    sys.path.insert(0,str(source))
    upstream = suite(source)
    reference = Path(sys.executable).parent / "code-review-graph"
    if not reference.is_file():
        raise RuntimeError("install the pinned Python console script in the oracle's environment")
    commands = dict(upstream.INVOCATIONS)
    for case in upstream.FLAG_BEHAVIOUR:
        baseline, flagged, _ = case.values
        commands[f"flag/{case.id}/baseline"] = baseline
        commands[f"flag/{case.id}/flagged"] = flagged
    observations = {}
    with tempfile.TemporaryDirectory(prefix="crg-upstream-suite-") as name:
        temp = Path(name).resolve()
        repo, home = temp / "repo", temp / "home"
        home.mkdir()
        upstream.init_repo(repo)
        upstream.write_sample_project(repo)
        git(repo,"add","-A")
        git(repo,"commit","-qm","initial")
        with (repo/"pkg/util.py").open("a") as output:
            output.write("\n\ndef triple_all(items):\n    return [helper(item) * 3 for item in items]\n")
        git(repo,"add","-A")
        git(repo,"commit","-qm","second")
        env = {k:v for k,v in os.environ.items() if k in ("PATH","SystemRoot","COMSPEC","TMP","TEMP","TMPDIR")}
        env.update(HOME=str(home),USERPROFILE=str(home),CRG_HOME=str(home/".code-review-graph"),
                   HERMES_HOME=str(home/".hermes"),XDG_CONFIG_HOME=str(home/".config"),
                   XDG_CACHE_HOME=str(home/".cache"),XDG_DATA_HOME=str(home/".local/share"),
                   NO_COLOR="1",CRG_PARSE_EXECUTOR="serial",PYTHONHASHSEED="0",
                   PYTHONDONTWRITEBYTECODE="1",GIT_CONFIG_NOSYSTEM="1",TZ="UTC")
        native, py = [str(args.binary.resolve())], [str(reference)]
        result=run([*py,"build","-q"],repo,env|{"PYTHONPATH":str(source)})
        if result["exit"]: raise RuntimeError(result)
        with sqlite3.connect(repo/".code-review-graph/graph.db") as db:
            db.execute("UPDATE metadata SET value='2099-01-01T00:00:00+00:00' WHERE key='last_updated'")
        pristine=temp/"seed"
        shutil.copytree(repo,pristine)
        # Each launch sees the same paths, index, and registry, including commands that modify them.
        def observed(exe,argv,seed="0"):
            shutil.rmtree(repo)
            shutil.copytree(pristine,repo)
            shutil.rmtree(home)
            home.mkdir()
            if argv[0] == "unregister":
                registration=run([*py,"register","."],repo,env|{"PYTHONPATH":str(source)})
                assert registration["exit"]==0, registration
            if argv[:2] == ["daemon","remove"]:
                registration=run([*py,"daemon","add",".","--alias","surface"],repo,env|{"PYTHONPATH":str(source)})
                assert registration["exit"]==0, registration
            return run([*exe,*argv],repo,env|({"PYTHONPATH":str(source),"PYTHONHASHSEED":seed} if exe==py else {}))
        for label,argv in commands.items():
            wanted=observed(py,argv)
            native_result=observed(native,argv)
            if wanted == native_result:
                outcome = "match"
            elif wanted != observed(py,argv):
                outcome = "unstable reference"
            else:
                outcome = "different"
                # Upstream returns list(set(...)) for this field. Accept another ordering only
                # when the same Python release actually returns that order on this fixture.
                if argv[0] == "impact":
                    for seed in range(1,9):
                        other = observed(py,argv,str(seed))
                        if other == native_result and equal_impact_files(wanted,other):
                            outcome = "matched observed reference ordering"
                            break
            observations[label]={"outcome":outcome,"argv":argv,"reference":wanted,"native":native_result}
    args.report.parent.mkdir(parents=True,exist_ok=True)
    args.report.write_text(json.dumps(observations,indent=2,ensure_ascii=False)+"\n")
    for outcome in ("match","matched observed reference ordering","different","unstable reference"):
        names=[name for name,r in observations.items() if r["outcome"]==outcome]
        print(f"{outcome}: {len(names)}"+("" if outcome=="match" else " "+", ".join(names)))
    return int(any(r["outcome"] in ("different","unstable reference") for r in observations.values()))


if __name__=="__main__":
    sys.exit(main())
