"""Run the pinned doctor's released setup and read-only fixtures against both public binaries."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import sysconfig
import venv

from public_parity import pinned_reference, run


class DoctorOracle:
    def __init__(self, source, binary, parent):
        from code_review_graph import doctor
        self.source, self.binary, self.parent = source, binary, parent
        self.reference = [sys.executable, "-B", "-m", "code_review_graph"]
        self.original = doctor.diagnose
        self.count = 0

    def compare(self, repo, cwd=None, env=None, explicit=True):
        env = dict(os.environ if env is None else env)
        env.update(PYTHONPATH=str(self.source), PYTHONDONTWRITEBYTECODE="1",
                   CODEX_HOME=str(Path(env["HOME"]) / ".codex"))
        env.pop("CRG_REPO_ROOT", None)
        cwd = cwd or self.parent
        args = ["doctor", *(["--repo", str(repo)] if explicit else [])]
        ref = run([*self.reference, *args, "--json"], cwd, env)
        native = run([self.binary, *args, "--json"], cwd, env)
        assert ref["exit"] == native["exit"], (ref, native)
        assert ref["stderr"] == native["stderr"] == ""
        expected, observed = json.loads(ref["stdout"]), json.loads(native["stdout"])
        # Binaries live at different paths. Validate that each reports its actual runtime.
        expected_copy = expected["this_copy"]
        assert Path(expected_copy["package"]) == self.source / "code_review_graph"
        assert Path(expected_copy["python"]).samefile(sys.executable)
        assert observed["this_copy"] == {"version": expected_copy["version"], "executable": str(self.binary)}
        py_copy = f"This copy: {expected_copy['version']} at {expected_copy['package']}."
        rust_copy = f"This copy: {expected_copy['version']} at {self.binary}."
        assert observed["checks"][0]["message"] == rust_copy and expected["checks"][0]["message"] == py_copy
        expected["this_copy"] = observed["this_copy"]
        expected["checks"][0]["message"] = rust_copy
        assert native["stdout"] == json.dumps(expected, indent=2) + "\n", (expected, observed)
        py_text = run([*self.reference, *args], cwd, env)
        rs_text = run([self.binary, *args], cwd, env)
        assert py_text["exit"] == rs_text["exit"] == int(not observed["healthy"])
        assert py_text["stderr"] == rs_text["stderr"] == ""
        assert py_text["stdout"].replace(py_copy, rust_copy, 1) == rs_text["stdout"]
        self.count += 1
        return observed, rs_text

    def diagnose(self, repo, **kwargs):
        from code_review_graph import doctor
        value, _ = self.compare(repo)
        # Five release cases also inspect an injected Python argv recorder. Run it
        # against the reference, then use the native report for their public assertions.
        if "run" in kwargs:
            self.original(repo, **kwargs)
        return doctor.Report(value["repo_root"], value["this_copy"],
                             [doctor.Copy(**v) for v in value["path_copies"]],
                             [doctor.McpEntry(**v) for v in value["mcp_entries"]],
                             value["graph"], value["hooks"], [doctor.Finding(**v) for v in value["checks"]])

    def pytest_collection_modifyitems(self, items):
        from code_review_graph import doctor
        doctor.diagnose = self.diagnose
        surface = next(item.module for item in items if item.module.__name__.endswith("test_cli_surface"))
        official_run = surface.run_cli
        def run_cli(*args, cwd, env, **kwargs):
            if args[0] != "doctor":
                return official_run(*args, cwd=cwd, env=dict(env, PYTHONPATH=str(self.source)), **kwargs)
            env = dict(env, PATH=str(self.parent / "tools"))
            value, output = self.compare(cwd, cwd=cwd, env=env, explicit=False)
            text = json.dumps(value, indent=2) if "--json" in args else output["stdout"]
            return surface.CliResult(tuple(args), output["exit"], text)
        surface.run_cli = run_cli


def extra_cases(oracle, folder):
    repo = folder / "diagnostic-repo"
    repo.mkdir()
    data = repo / ".code-review-graph"
    data.mkdir()
    db = data / "graph.db"
    config = repo / "opencode.jsonc"
    config.write_text('{"mcp":{/* comment */"code-review-graph":{"command":["uv","tool","run","--from=code-review-graph[full]@2.9.0","serve",],},},}')
    env = dict(os.environ, PATH=str(folder / "tools"), HOME=str(folder), CRG_HOME=str(folder / "registry"))
    repo_before = lambda: {str(p.relative_to(repo)): (p.stat().st_size, p.stat().st_mtime_ns)
                           for p in repo.rglob("*") if p.is_file()}
    for stage in ("missing", "corrupt", "older", "postprocess-pending", "no-sha"):
        if stage == "corrupt":
            db.write_bytes(b"this is not a SQLite database\n")
        if stage == "older":
            db.unlink()
            with sqlite3.connect(db) as conn:
                conn.executescript("create table nodes(kind text);create table metadata(key text primary key,value text);"
                                   "insert into metadata values ('schema_version','14');")
        if stage in {"postprocess-pending", "no-sha"}:
            with sqlite3.connect(db) as conn:
                conn.execute("insert or replace into metadata values ('build_state', ?)",
                             (stage if stage == "postprocess-pending" else "complete",))
        before = repo_before()
        oracle.compare(repo, env=env)
        assert before == repo_before()
    # A repository-owned PATH entry must not get a version query, including via a symlink.
    inside = repo / "bin"
    inside.mkdir()
    marker = folder / "inside-program-ran"
    for name in ("code-review-graph", "uv", "git"):
        stub = inside / name
        stub.write_text(f"#!/bin/sh\n: > '{marker}'\n")
        stub.chmod(0o755)
    (repo / ".git").mkdir()
    config.write_text('{"mcp":{"code-review-graph":{"command":["uvx","code-review-graph","serve"]}}}')
    alias = folder / "alias"
    alias.symlink_to(inside, target_is_directory=True)
    result = run([oracle.binary,"doctor","--repo",str(repo),"--json"], folder,
                 dict(env, PATH=str(alias)))
    assert result["exit"] == 1 and not marker.exists()
    assert json.loads(result["stdout"])["path_copies"][0]["version"] is None
    outside = folder / "python3"
    outside.write_text(f"#!/bin/sh\n: > '{marker}'\n")
    outside.chmod(0o755)
    interpreter = inside / "python3"
    interpreter.symlink_to(outside)
    config.write_text(json.dumps({"mcp":{"code-review-graph":{
        "command":[str(interpreter),"-m","code_review_graph","serve"]}}}))
    result = run([oracle.binary,"doctor","--repo",str(repo),"--json"], folder, env)
    assert not marker.exists()
    assert "does not run a program inside" in json.loads(result["stdout"])["mcp_entries"][0]["source"]


def main():
    import pytest
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--reference-installed", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    source, binary = args.upstream.resolve(), args.binary.resolve()
    if not args.reference_installed:
        # -I deliberately ignores PYTHONPATH. Give the reference a real, disposable
        # installation that it can inspect without modifying the supplied interpreter.
        with tempfile.TemporaryDirectory(prefix="crg-doctor-python-") as name:
            root = Path(name)
            venv.EnvBuilder(with_pip=False, symlinks=True).create(root)
            python = root / "bin/python"
            site = Path(subprocess.check_output([python, "-c",
                "import sysconfig;print(sysconfig.get_paths()['purelib'])"],text=True).strip())
            (site / "pinned-reference.pth").write_text(
                f"{source}\n{sysconfig.get_paths()['purelib']}\n")
            assert subprocess.check_output([python, "-I", "-m", "code_review_graph", "--version"],
                                           cwd=root,text=True).strip() == "code-review-graph 2.4.0"
            subprocess.run([python,"-B",Path(__file__).resolve(),"--upstream",source,
                            "--binary",binary,"--reference-installed"],check=True)
        return
    pinned_reference(source)
    suites = {"test_doctor.py": "9f1fdd3bdc3177689d372f3c68abc4f30eaa6438c4d5e7241343397e7e075f4f",
              "test_cli_surface.py": "801e7da0015e85b797927d57d301bafcfc3a828bc6bf3acabb1d396ec2e53103"}
    for name, digest in suites.items():
        assert hashlib.sha256((source / "tests" / name).read_bytes()).hexdigest() == digest
    sys.path.insert(0, str(source))
    with tempfile.TemporaryDirectory(prefix="crg-doctor-oracle-") as name:
        parent = Path(name).resolve()
        tools = parent / "tools"
        tools.mkdir()
        git = shutil.which("git")
        assert git
        (tools / "git").symlink_to(git)
        oracle = DoctorOracle(source, binary, parent)
        extra_cases(oracle, parent)
        result = pytest.main([str(source/"tests/test_doctor.py"),
                              str(source/"tests/test_cli_surface.py")+"::test_doctor_reports_cleanly_whatever_the_machine_holds",
                              "-q", "-p", "no:cacheprovider", "--tb=short", "-m", "cli_surface or not cli_surface",
                              "--basetemp", str(parent/"fixtures")],plugins=[oracle])
        if result:
            raise SystemExit(result)
        print(f"Compared {oracle.count} doctor setups in JSON and text, plus the repository-owned PATH guard.")


if __name__ == "__main__":
    main()
