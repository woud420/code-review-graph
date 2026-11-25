"""Compare C/C++ persisted graph rows after build/update against the pinned Python CLI."""
import argparse
import ast
from hashlib import sha256
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import git, new_repo, verify_source


def corpus(upstream):
    fixtures = upstream / "tests/fixtures"
    files = {str(p.relative_to(fixtures)): p.read_text() for p in sorted(fixtures.rglob("*"))
             if p.suffix in {".c", ".h", ".cpp", ".hh", ".hpp", ".cc", ".cxx"}}
    digest = sha256()
    for name, source in files.items():
        digest.update(name.encode() + b"\0" + sha256(source.encode()).digest())
    assert digest.hexdigest() == "a9e65a6e327bbe3bb65f11ed25eaefe8feb87d1f8357d9e7b94314a648ffabe6", "C family fixtures differ from the pinned sources"
    tests = ast.parse((upstream / "tests/test_cpp_overload_identity.py").read_text())
    for fn in tests.body:
        if not isinstance(fn, ast.FunctionDef):
            continue
        for call in ast.walk(fn):
            if (isinstance(call, ast.Call) and isinstance(call.func, ast.Name)
                    and call.func.id == "_index_source" and len(call.args) == 2
                    and isinstance(call.args[1], ast.Constant) and isinstance(call.args[1].value, str)):
                files["overloads/" + fn.name.removeprefix("test_cpp_").removeprefix("test_") + ".cpp"] = call.args[1].value
    assert len(files) == 24, list(files)
    files["include/types.h"] = "typedef struct record { int id; } Record;\n"
    files["external/library.cpp"] = "void Billing::Apply(int fee) {}\nvoid Billing::Tax(int fee) {}\nvoid Billing::Tax(double fee) {}\n"
    files["external/client.cpp"] = "void test_billing() {\n Billing::Apply(5);\n Billing::Tax(4);\n Billing::Missing();\n}\n"
    return files


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as conn:
        def targets(filename, caller):
            return {r[0].removeprefix(str(root) + "/") for r in conn.execute(
                "SELECT target_qualified FROM edges WHERE kind='CALLS' AND source_qualified=?",
                (str(root / filename) + "::" + caller,))}
        assert targets("sample.c", "main") == {"sample.c::create_user", "sample.c::print_user", "free"}
        assert "live_in_else" in targets("sample_dead_guard.c", "caller")
        assert not any(t.startswith("dead_") for t in targets("sample_dead_guard.c", "caller"))
        assert targets("sample.cpp", "main()") == {"sample.cpp::greet(const Animal&)", "speak"}
        assert conn.execute("SELECT language FROM nodes WHERE file_path=? AND kind='File'",
                            (str(root / "cpp_qt_headers/MyWidget.h"),)).fetchone() == ("cpp",)
        assert conn.execute("SELECT count(*) FROM nodes WHERE name='calculateValue'").fetchone()[0] >= 4
        tagged = str(root / "include/types.h")
        assert conn.execute("SELECT source_qualified FROM edges WHERE kind='CONTAINS' AND target_qualified=?",
                            (tagged+"::Record.record",)).fetchone() == (tagged + "::Record",)
        assert conn.execute("SELECT language FROM nodes WHERE qualified_name=?",(tagged,)).fetchone() == ("c",)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--proof-binary", type=Path, required=True)
    args = p.parse_args()
    source = args.upstream.resolve()
    verify_source(source)
    files = corpus(source)
    native, proof = str(args.binary.resolve()), str(args.proof_binary.resolve())
    failures = []
    with tempfile.TemporaryDirectory(prefix="crg-cfamily-") as name:
        temp = Path(name).resolve()
        root = temp / "repo"
        new_repo(root, files)
        env = {k:v for k,v in os.environ.items() if not k.startswith(("CRG_", "FASTMCP_", "PYTHON"))}
        env.update(CRG_SERIAL_PARSE="1", CRG_HOME=str(temp/"home"), PYTHONDONTWRITEBYTECODE="1")
        for command in ("build", "update"):
            if command == "update":
                (root/"sample.hh").unlink()
                (root/"sample.c").write_text(files["sample.c"].replace("print_user", "print_profile"))
                git(root, "commit", "-qam", "rename C function and remove C++ header")
            for post in ("none", "full"):
                graphs = []
                for language, executable in (("python", [sys.executable, "-m", "code_review_graph"]),
                                             ("rust", [native])):
                    directory = temp / (language + "-" + post)
                    opts = dict(env, CRG_DATA_DIR=str(directory))
                    if language == "python":
                        opts["PYTHONPATH"] = str(source)
                    subprocess.run([*executable,command,"--repo",str(root),"-q",
                                    *(["--skip-postprocess"] if post == "none" else [])],
                                   env=opts,cwd=root,check=True,timeout=120)
                    if command == "build" and post == "none":
                        assertions(directory/"graph.db",root)
                    graphs.append(read_graph(directory/"graph.db",root))
                for group, expected in graphs[0].items():
                    actual=graphs[1][group]
                    missing, extra=expected-actual,actual-expected
                    name=f"{command}/{post}/{group}"
                    print(f"{name}: {expected.total()} upstream, {missing.total()} missing, {extra.total()} extra")
                    if missing or extra:
                        failures.append(name)
                        print(f"{name}: missing {missing.most_common(4)}; extra {extra.most_common(4)}", file=sys.stderr)
            subprocess.run([proof,command,"--repo",str(root)],env=env,check=True,capture_output=True)
            def callers(target):
                out=subprocess.check_output([proof,"query","callers-of",target,"--repo",str(root)],env=env)
                return {row["name"] for row in json.loads(out)["results"]}
            assert callers("sample.cpp::greet(const Animal&)") == {"main()"}
            assert callers("sample.c::" + ("print_user" if command=="build" else "print_profile")) == {"main"}
    return int(bool(failures))

if __name__ == "__main__":
    sys.exit(main())
