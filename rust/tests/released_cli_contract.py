"""Run the released CLI's public pytest assertions through each implementation."""
import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

from upstream_surface import suite
from public_parity import pinned_reference

# These enumerate Python source or deliberately crash its interpreter.
SOURCE_ONLY = {
    "test_canary_parser_enumeration_found_the_real_tree",
    "test_canary_env_scan_found_the_real_variables",
    "test_canary_harness_detects_a_traceback",
    "test_invocation_table_covers_every_enumerated_command",
    "test_numeric_env_var_table_covers_every_numeric_site",
    "test_no_new_unguarded_numeric_env_parse",
}


class Commands:
    def __init__(self, command, upstream):
        self.command = command
        self.upstream = upstream

    def __getattr__(self, name):
        return getattr(subprocess, name)

    def Popen(self, argv, *args, **kwargs):
        if argv[:3] == [sys.executable, "-m", "code_review_graph"]:
            argv = [*self.command, *argv[3:]]
            if self.upstream is not None and kwargs.get("env"):
                env = kwargs["env"].copy()
                # Some fixtures prepend Python import shims. The reference package
                # still has to be present after the shim for those tests to run.
                env["PYTHONPATH"] = os.pathsep.join(filter(None, [env.get("PYTHONPATH"), str(self.upstream)]))
                kwargs["env"] = env
        return subprocess.Popen(argv, *args, **kwargs)


def worker(upstream, command, root):
    import pytest
    sys.path.insert(0, str(upstream))
    # Pin the fixture file as well as the Python package.
    suite(upstream)
    class Route:
        def pytest_collection_modifyitems(self, items):
            for item in items:
                item.module.subprocess = Commands(command, upstream if Path(command[0]).parent == Path(sys.executable).parent else None)
    return pytest.main([str(upstream/"tests/test_cli_surface.py"), "-q", "--tb=line",
                        "-m", "cli_surface", "--basetemp", str(root/"repos"), "-p", "no:cacheprovider",
                        "--junitxml", str(root/"result.xml"), "-k",
                        "not (" + " or ".join(sorted(SOURCE_ONLY)) + ")"], plugins=[Route()])


def results(file):
    outcomes = {}
    for row in ET.parse(file).iter("testcase"):
        state = "pass"
        detail = ""
        for tag in ("error", "failure", "skipped"):
            element = row.find(tag)
            if element is not None:
                state, detail = tag, element.get("message", "")
                break
        outcomes[row.get("name")] = {"state": state, "detail": detail}
    return outcomes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--worker", type=Path)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    if args.worker:
        return worker(upstream, [str(binary)], args.worker)
    pinned_reference(upstream)
    reference = Path(sys.executable).parent / "code-review-graph"
    with tempfile.TemporaryDirectory(prefix="crg-released-cli-") as tmp:
        root = Path(tmp).resolve()
        def run(name, exe):
            folder = root/name
            folder.mkdir()
            cli_bin = folder/"bin"
            cli_bin.mkdir()
            (cli_bin/"code-review-graph").symlink_to(exe)
            env = {k: v for k, v in os.environ.items() if k in ("PATH","TMPDIR","TMP","TEMP","SystemRoot")}
            env.update(HOME=str(folder), USERPROFILE=str(folder), PYTHONPATH=str(upstream),
                       PATH=os.pathsep.join([str(cli_bin),env["PATH"]]), PYTHONDONTWRITEBYTECODE="1",
                       GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, NO_COLOR="1",
                       CRG_PARSE_EXECUTOR="serial", PYTHONHASHSEED="0")
            cmd = [sys.executable, "-B", str(Path(__file__).resolve()), "--upstream", str(upstream),
                   "--binary", str(exe), "--worker", str(folder)]
            proc = subprocess.run(cmd, cwd=upstream, env=env, capture_output=True, timeout=650)
            if proc.returncode not in (0, 1) or not (folder/"result.xml").is_file():
                raise RuntimeError(proc.stdout.decode(errors="replace")[-4000:]+proc.stderr.decode(errors="replace")[-2000:])
            return results(folder/"result.xml")
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            py, native = executor.map(lambda pair: run(*pair), (("python", reference), ("rust", binary)))
    if py.keys() != native.keys() or not py:
        raise ValueError("released test inventory changed between processes")
    failed = [case for case in py if py[case]["state"] == "pass" and native[case]["state"] != "pass"]
    baseline = [case for case in py if py[case]["state"] in ("error","failure")]
    report = {"reference":py, "rust":native, "regressions":failed, "reference_failures":baseline,
              "python_source_only":sorted(SOURCE_ONLY)}
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2)+"\n")
    passed = sum(row["state"] == "pass" for row in py.values())
    if passed == 0:
        raise RuntimeError("the reference ran zero passing CLI assertions")
    print(f"Released CLI contract: {passed-len(failed)}/{passed} reference-passing assertions pass in Rust, "
          f"{len(baseline)} reference failures, {len(py)-passed-len(baseline)} reference skips")
    for name in failed:
        print(f"  {name}")
    return bool(failed or baseline)


if __name__ == "__main__":
    sys.exit(main())
