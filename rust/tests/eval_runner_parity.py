"""Compare measured benchmark rows against the release using an offline Git repository."""
import argparse
import csv
from io import StringIO
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from differential import git, new_repo, verify_source


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream",type=Path,required=True)
    parser.add_argument("--binary",type=Path,required=True)
    args=parser.parse_args()
    source,binary=args.upstream.resolve(),args.binary.resolve()
    verify_source(source)
    with tempfile.TemporaryDirectory(prefix="crg-eval-run-") as folder:
        temp=Path(folder).resolve()
        mirror=temp/"mirror"
        new_repo(mirror,{"lib.py":"def double(n):\n    return n * 2\n\ndef main():\n    return double(2)\n"})
        (mirror/"lib.py").write_text((mirror/"lib.py").read_text().replace("double(2)","double(4)"))
        git(mirror,"commit","-qam","change")
        sha=subprocess.check_output(["git","rev-parse","HEAD"],cwd=mirror,text=True).strip()
        config=temp/"sample.yaml"
        config.write_text(json.dumps(dict(name="sample",url=str(mirror),commit=sha,entry_points=["lib.py::main"],
                                          test_commits=[{"sha":sha,"description":"change"}],
                                          search_queries=[{"query":"double","expected":"lib.py::double"}])))
        cwd=temp/"runner"
        env={k:v for k,v in os.environ.items() if k in ("PATH","TMPDIR","TMP","TEMP")}
        env.update(HOME=str(temp), CRG_HOME=str(temp/"registry"), GIT_CONFIG_NOSYSTEM="1",
                   GIT_CONFIG_GLOBAL=os.devnull, PYTHONPATH=str(source), NO_COLOR="1")
        outcomes=[]
        for cli in ([sys.executable,"-m","code_review_graph"],[str(binary)]):
            cwd.mkdir()
            f=["eval","--repo",str(config.with_suffix("")),"--benchmark","flow_completeness,search_quality,token_efficiency,impact_accuracy","--output-dir","rows"]
            result=subprocess.run([*cli,*f],cwd=cwd,env=env,capture_output=True,check=True)
            tables={p.name:p.read_bytes() for p in (cwd/"rows").glob("*.csv")}
            assert len(tables)==4 and b"Completed 4 benchmark(s)" in result.stdout
            parsed=[row for raw in tables.values() for row in csv.DictReader(StringIO(raw.decode()))]
            assert any(row.get("rank")=="1" for row in parsed)
            again=subprocess.run([*cli,*f],cwd=cwd,env=env,capture_output=True,check=True)
            assert result.stdout==again.stdout
            assert tables=={p.name:p.read_bytes() for p in (cwd/"rows").glob("*.csv")}
            outcomes.append((result.stdout,tables))
            shutil.rmtree(cwd)
        assert outcomes[0]==outcomes[1],outcomes
    print("Evaluation runner: local clone and pinned checkout, cached rerun, search ranks and flow recall CSVs matched")


if __name__=="__main__":
    main()
