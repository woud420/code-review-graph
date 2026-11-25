"""Check actual incremental benchmark measurements on disposable, pinned, offline repositories."""

import argparse
import csv
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from differential import git, new_repo, verify_source


def call(command, cwd, env, *args):
    result = subprocess.run([*command, *args], cwd=cwd, env=env, capture_output=True, text=True, timeout=150)
    assert result.returncode == 0, (args, result.stdout, result.stderr)
    return result


def rows(cwd, name):
    (file,) = (cwd / "results").glob(f"*_{name}_*.csv")
    with file.open(newline="") as f:
        return list(csv.DictReader(f))


def source_state(path):
    return call(["git"], path, os.environ, "status", "--porcelain", "--untracked-files=no").stdout, call(["git"], path, os.environ, "rev-parse", "HEAD").stdout


def native_projection_oracle(source, fixture, kinds, native, env, area):
    # Drive the public Rust build and update paths from the reference's own
    # edit generator, then use its SQLite comparator to audit every Rust row.
    sys.path.insert(0, str(source))
    from code_review_graph.eval.benchmarks import incremental_fidelity as oracle
    tree = oracle._materialise_tree(fixture, area / "tree")
    base = oracle._head(tree)
    baseline = area / "baseline"
    call(native, tree, env, "build", "--data-dir", str(baseline), "-q")
    targets = oracle._pick_targets(baseline / "graph.db", tree)
    out = []
    for kind in kinds:
        row = oracle._blank_row("sample", kind)
        applier = oracle._APPLIERS[kind] if kind != "revert" else oracle._edit_trailing_comment
        edit = applier(tree, targets)
        if edit.skipped:
            row["note"] = edit.skipped
        else:
            incremental, rebuild = area / (kind + "-updated"), area / (kind + "-rebuild")
            shutil.copytree(baseline, incremental)
            edited = oracle._commit_all(tree, f"benchmark: {kind}")
            call(native, tree, env, "update", "--base", base, "--data-dir", str(incremental), "-q")
            if kind == "revert":
                oracle._reset(tree, base)
                oracle._commit_all(tree, "benchmark: revert")
                call(native, tree, env, "update", "--base", edited, "--data-dir", str(incremental), "-q")
            call(native, tree, env, "build", "--data-dir", str(rebuild), "-q")
            row.update(target=edit.target, note="appended then removed one comment line" if kind=="revert" else edit.note)
            diff = oracle.compare_graphs(incremental/"graph.db", rebuild/"graph.db", path_prefix=str(tree)+"/")
            oracle._record(row, diff)
            row["status"] = "known_failure" if diff["total_differing_rows"] else "ok"
        out.append({k:str(v) for k,v in row.items() if k not in ("seconds","benchmark_seconds")})
        oracle._reset(tree, base)
    return out


def exercise(source, native, temp, files, edits=None):
    fixture = temp/"mirror"
    new_repo(fixture, files)
    before = source_state(fixture)
    config = dict(name="sample",url=str(fixture),commit=before[1].strip(),
                  search_queries=[{"query":"read", "expected":"read"},{"query":"not found","expected":"missing"}])
    if edits is not None:
        config["fidelity_edits"] = edits
    config_path = temp/"config.yaml"
    config_path.write_text(json.dumps(config))
    pyenv = {k:v for k,v in os.environ.items() if k in ("PATH","TMPDIR","TMP","TEMP")}
    pyenv.update(HOME=str(temp), CRG_HOME=str(temp/"registry"), GIT_CONFIG_GLOBAL=os.devnull,
                 GIT_CONFIG_NOSYSTEM="1", PYTHONPATH=str(source), NO_COLOR="1", PYTHONHASHSEED="0",
                 PYTHONDONTWRITEBYTECODE="1", CRG_PARSE_EXECUTOR="serial")
    env={k:v for k,v in pyenv.items() if k!="PYTHONPATH"}
    structural=[]; counts=[]
    for label,command,current in (("reference",[sys.executable,"-m","code_review_graph"],pyenv),("rust",native,env)):
        cwd=temp/label;cwd.mkdir()
        result = call(command, cwd, current, "eval", "--repo", str(config_path.with_suffix("")),
                      "--benchmark", "build_performance,incremental_fidelity", "--output-dir", "results")
        assert "Completed 2 benchmark(s)." in result.stdout and "detection failed:" not in result.stderr, result
        result_rows = rows(cwd,"incremental_fidelity")
        assert len(result_rows)==(len(edits) if edits is not None else 7)
        for row in result_rows:
            assert row["status"] in ("ok","skipped","known_failure"), row
            assert row["known_failure"]=="1" and not row["unchecked_tables"], row
            assert float(row["benchmark_seconds"]) >= float(row["seconds"]) >= 0, row
            assert sum(int(v) for k,v in row.items() if k.endswith("_diff"))==int(row["total_differing_rows"]), row
        assert len({r["benchmark_seconds"] for r in result_rows})==1
        structural.append([{k:v for k,v in row.items() if k not in ("seconds","benchmark_seconds")} for row in result_rows])
        (perf,) = rows(cwd,"build_performance")
        assert set(perf)=={"repo","file_count","node_count","edge_count","flow_detection_seconds","community_detection_seconds","search_avg_ms","nodes_per_second"},perf
        counts.append({k:perf[k] for k in ("repo","file_count","node_count","edge_count")})
        for key in ("flow_detection_seconds","community_detection_seconds","search_avg_ms","nodes_per_second"):
            n=float(perf[key]);assert math.isfinite(n) and n>=0,(key,n)
        assert int(perf["nodes_per_second"])>0
        assert source_state(cwd/"evaluate/test_repos/sample")==before
    assert counts[0] == counts[1], counts
    assert source_state(fixture)==before
    expected,observed=structural
    for ref,rs in zip(expected,observed,strict=True):
        for k in ("repo","edit_kind","target","note","known_failure"):
            assert ref[k]==rs[k],(rs["edit_kind"],k,ref[k],rs[k],ref["note"],rs["note"])
        assert (ref["status"]=="skipped") == (rs["status"]=="skipped"),(ref,rs)
    oracle = native_projection_oracle(source,fixture,[r["edit_kind"] for r in observed],native,env,temp/"oracle")
    for wanted,actual in zip(oracle,observed,strict=True):
        assert actual==wanted, {k:(wanted[k],actual[k]) for k in wanted if actual[k]!=wanted[k]}
    print(f"Performance counts {counts[0]}. Independent SQLite projections verify every native fidelity field.")
    for ref,rs in zip(expected,observed):
        print(f"  {rs['edit_kind']}: upstream {ref['status']} {ref['total_differing_rows']} {ref['example']}; Rust {rs['status']} {rs['total_differing_rows']} {rs['example']}")


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument("--upstream",type=Path,required=True);p.add_argument("--binary",type=Path,required=True)
    args=p.parse_args();source=args.upstream.resolve();verify_source(source);native=[str(args.binary.resolve())]
    with tempfile.TemporaryDirectory(prefix="crg-fidelity-parity-") as folder:
        temp=Path(folder).resolve(); rich=temp/"multi";rich.mkdir()
        exercise(source,native,rich,{
            "app.py":"from core import read\n\ndef main():\n    return read()\n",
            "core.py": "import math\n\ndef number():\n    return 4\n\ndef read():\n    return math.sqrt(number())\n",
            "tests/test_app.py":"from app import main\n\ndef test_run():\n    assert main() == 2\n",
        })
        sparse=temp/"sparse";sparse.mkdir()
        exercise(source,native,sparse,{"data.json":'{"answer": 42}'},["add_file","move_function","change_import","revert"])


if __name__=="__main__":
    main()
