"""Compare complete JSON path metadata, spans and containment through both public builds."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import new_repo, verify_source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args=parser.parse_args()
    source=args.upstream.resolve()
    verify_source(source)
    with tempfile.TemporaryDirectory(prefix="crg-json-") as temp:
        folder=Path(temp).resolve()
        root=folder/"source"
        new_repo(root, {"config.json": (source/"tests/fixtures/sample_config.json").read_text(),
                        "config/wide.json": "{"+",".join(f'\"key{i}\": {i}' for i in range(201))+"}",
                        "config/dupes.json": '{"a":42,"a":false,"items":[{"unicode 💡":true},{"unicode 💡":"hidden"}]}',
                        "locales/pt-BR.json": '{"translated":"value"}',
                        "docs/sample.json": '{"secret":"content"}',
                        "package-lock.json": '{"dependencies":{"ignored":"value"}}'})
        graphs=[]
        for label,command in [("reference",[sys.executable,"-m","code_review_graph"]),("rust",[str(args.binary.resolve())])]:
            env={k:v for k,v in os.environ.items() if not k.startswith(("CRG_","PYTHON"))}
            if label=="reference":env["PYTHONPATH"]=str(source)
            env.update(CRG_HOME=str(folder/"registry"),CRG_DATA_DIR=str(folder/label),CRG_SERIAL_PARSE="1",PYTHONDONTWRITEBYTECODE="1")
            subprocess.run([*command,"build","--quiet","--repo",str(root)],cwd=root,env=env,check=True,timeout=45)
            graphs.append(read_graph(folder/label/"graph.db",root))
        if graphs[0]["symbols"].total()<20:
            raise RuntimeError("missing reference path nodes")
        for category in graphs[0]:
            left,right=graphs[0][category],graphs[1][category]
            if left!=right:
                raise AssertionError(f"{category}: missing {list((left-right).items())[:1]}, extra {list((right-left).items())[:1]}")
        print(f"JSON config: {graphs[0]['symbols'].total()} nodes with identical path metadata, spans, containment and derived graph")
if __name__=="__main__":
    main()
