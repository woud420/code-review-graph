"""Compare both daemon command names with a pinned upstream and exercise isolated processes.

Run using the locked reference interpreter. The only daemon state directory passed
to any process is below a fresh temporary directory. Foreground reference startup
keeps the exact supervised process available for cleanup if an assertion fails.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time

from differential import git
from public_parity import pinned_reference, run


def wait_for(check, label, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            result = check()
        except (OSError, ValueError, sqlite3.Error):
            result = None
        if result:
            return result
        time.sleep(0.12)
    raise AssertionError(f"timed out waiting for {label}")


def isolated(root, source, python):
    state = root / "state"
    assert state.parent == root and root.is_absolute()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "GIT_"))}
    env.update(CRG_HOME=str(state), PYTHONPATH=str(source), PYTHONDONTWRITEBYTECODE="1",
               PYTHONNOUSERSITE="1", PATH=f"{Path(python).parent}:/usr/bin:/bin",
               NO_COLOR="1", TERM="dumb", COLUMNS="80",
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    return state, env


def repo(root, name):
    path = root / name
    path.mkdir(exist_ok=True)
    subprocess.run(["/usr/bin/git", "init", "-q", str(path)], check=True, capture_output=True)
    (path / "billing.py").write_text("def original():\n    return 1\n", encoding="utf-8")
    git(path, "add", "billing.py")
    git(path, "commit", "-qm", "initial state")
    return path


def clean(root, state):
    assert root == state.parent and state.name == "state"
    assert not (state / "daemon.pid").exists(), "cannot reset state of a live daemon"
    if state.exists():
        shutil.rmtree(state)


def stopped(command, root, env, state):
    cases = {}

    def probe(label, *args):
        cases[label] = run(command + [str(a) for a in args], root, env)

    for name in ("", "start", "stop", "restart", "status", "logs", "add", "remove"):
        probe("help/" + (name or "top"), *([name] if name else []), "--help")
    probe("status/empty", "status")
    probe("stop/empty", "stop")
    probe("logs/empty", "logs")
    probe("remove/missing", "remove", "absent")
    probe("add/missing", "add", "absent")
    (root / "plain").mkdir(exist_ok=True)
    probe("add/no-marker", "add", "plain")
    probe("add/relative", "add", "first", "--alias", 'x"y')
    cases["disk/config"] = (state / "watch.toml").read_text(encoding="utf-8")
    probe("add/repeated", "add", "first", "--alias", "renamed")
    probe("add/alias-used", "add", "second", "--alias", 'x"y')
    probe("add/implicit-alias", "add", "second")
    probe("status/configured", "status")
    probe("logs/repo-missing", "logs", "--repo", "second")
    (state / "logs").mkdir()
    data = "first\nline two\nlast\n".encode() + b"invalid byte \xff\n"
    (state / "logs/daemon.log").write_bytes(data)
    (state / "logs/second.log").write_text("repo output\n", encoding="utf-8")
    probe("logs/daemon-tail", "logs", "--lines", "2")
    probe("logs/zero", "logs", "--lines", "0")
    probe("logs/negative", "logs", "--lines", "-2")
    probe("logs/repo-tail", "logs", "--repo", "second")
    probe("remove/by-alias", "remove", 'x"y')
    probe("remove/by-path", "remove", "second")
    probe("status/removed", "status")
    cases["disk/removed-config"] = (state / "watch.toml").read_text(encoding="utf-8")
    return cases


def contains_symbol(path, name):
    target = path / ".code-review-graph/graph.db"
    with sqlite3.connect(target.as_uri() + "?mode=ro", uri=True) as db:
        return bool(db.execute("SELECT count(*) FROM nodes WHERE name=? AND kind='Function'", (name,)).fetchone()[0])


def process_state(state):
    return json.loads((state / "daemon-state.json").read_text(encoding="utf-8"))


def foreground(command, root, state, env, action):
    """Run the identical lifecycle for each implementation, assert outcomes individually."""
    outcomes = {}
    first, second = root / "first", root / "second"
    init = run(command + ["add", str(first), "--alias", "fee"], root, env)
    assert init["exit"] == 0, init
    conf = state / "watch.toml"
    conf.write_text(conf.read_text().replace("poll_interval = 2", "poll_interval = 1"))
    log_path = root / "foreground-capture"
    with log_path.open("w") as log, ThreadPoolExecutor(1) as pool:
        proc = subprocess.Popen(command + [action, "--foreground"], cwd=root, env=env,
                                stdout=log, stderr=log, stdin=subprocess.DEVNULL)
        exit_status = pool.submit(proc.wait)
        try:
            wait_for(lambda: int((state / "daemon.pid").read_text()) == proc.pid, "owned daemon PID")
            wait_for(lambda: "fee" in process_state(state), "initial worker")
            wait_for(lambda: contains_symbol(first, "original"), "daemon initial graph")
            outcomes["initial-graph"] = True
            status = run(command + ["status"], root, env)
            assert status["exit"] == 0 and f"running (PID {proc.pid})" in status["stdout"], status
            entry = process_state(state)["fee"]
            assert entry["path"] == str(first) and entry["pid"] != proc.pid, entry
            assert re.search(rf"\bfee\s+alive\s+\w+\s+{entry['pid']}\s+", status["stdout"]), status
            outcomes["initial-worker-and-running-status"] = True
            refused = run(command + ["start"], root, env)
            assert refused == {"exit": 1, "stdout": "Error: Daemon is already running.\n", "stderr": ""}, refused
            outcomes["duplicate-start"] = True
            digest = hashlib.sha256(str(first).encode()).hexdigest()[:16]
            health_file = state / f"watch-health/{digest}.json"
            wait_for(lambda: json.loads(health_file.read_text())["phase"] == "watching", "ready file watcher")
            (first / "billing.py").write_text("def renamed():\n    return 2\n")
            wait_for(lambda: contains_symbol(first, "renamed") and not contains_symbol(first, "original"), "source edit")
            outcomes["edited-source-graph"] = True
            added = run(command + ["add", str(second), "--alias", "added"], root, env)
            assert added["exit"] == 0 and "Daemon will pick up the change automatically." in added["stdout"], added
            wait_for(lambda: "added" in process_state(state) and contains_symbol(second, "original"), "live addition")
            registry = json.loads((state / "registry.json").read_text())["repos"]
            assert {(r["path"], r["alias"]) for r in registry} == {(str(first), "fee"), (str(second), "added")}, registry
            outcomes["live-add-and-index"] = True
            removed = run(command + ["remove", "fee"], root, env)
            assert removed["exit"] == 0 and "Daemon will pick up the change automatically." in removed["stdout"], removed
            wait_for(lambda: set(process_state(state)) == {"added"}, "live removal")
            outcomes["live-remove"] = True
            result = run(command + ["stop"], root, env)
            assert result == {"exit": 0, "stderr": "", "stdout": f"Stopping daemon (PID {proc.pid})...\nDaemon stopped.\n"}, result
            assert exit_status.result(timeout=8) == 0
            assert not (state / "daemon.pid").exists() and not (state / "daemon-state.json").exists()
            outcomes["graceful-stop-and-cleanup"] = True
        except BaseException:
            print(log_path.read_text(errors="replace")[-4000:], file=sys.stderr)
            for entry in (state / "logs").glob("*.log"):
                print(f"{entry.name}: {entry.read_text(errors='replace')[-3000:]}", file=sys.stderr)
            raise
        finally:
            if proc.poll() is None:
                proc.terminate()
                try:
                    exit_status.result(timeout=10)
                except TimeoutError:
                    proc.kill()
                    exit_status.result(timeout=5)
    return outcomes


def native_background(command, root, state, env):
    """Fixture-owned background restart plus a foreign PID in an unlocked state file."""
    state.mkdir()
    with subprocess.Popen(["/bin/sleep", "90"], cwd=root, env=env) as harmless:
        try:
            (state / "daemon.pid").write_text(str(harmless.pid))
            refused = run(command + ["stop"], root, env)
            assert refused["exit"] == 1 and harmless.poll() is None and not (state / "daemon.pid").exists(), refused
        finally:
            harmless.terminate()
    try:
        first = run(command + ["restart"], root, env)
        assert first["exit"] == 0 and first["stdout"] == "Daemon is not running, starting fresh.\n", first
        pid = int((state / "daemon.pid").read_text())
        second = run(command + ["restart"], root, env)
        assert second["exit"] == 0 and second["stdout"] == f"Stopping daemon (PID {pid})...\nDaemon stopped.\n", second
        assert int((state / "daemon.pid").read_text()) != pid
    finally:
        if (state / "daemon.pid").exists():
            done = run(command + ["stop"], root, env)
            assert done["exit"] == 0, done
    return 3


def fresh_restart(command, root, state, env):
    # A foreground restart from stopped exercises the public restart handler without
    # losing the Popen handle or waiting for an unowned daemon PID.
    capture = root / "restart-capture"
    with capture.open("w") as stream, ThreadPoolExecutor(1) as pool:
        proc = subprocess.Popen(command + ["restart", "--foreground"], cwd=root, env=env, stdin=subprocess.DEVNULL,
                                stdout=stream, stderr=stream)
        exited = pool.submit(proc.wait)
        try:
            wait_for(lambda: int((state / "daemon.pid").read_text()) == proc.pid, "restarted daemon")
            stop_result = run(command + ["stop"], root, env)
            assert stop_result == {"exit": 0, "stderr": "", "stdout": f"Stopping daemon (PID {proc.pid})...\nDaemon stopped.\n"}, stop_result
            assert exited.result(timeout=10) == 0
        finally:
            if proc.poll() is None:
                proc.terminate()
                try:
                    exited.result(timeout=10)
                except TimeoutError:
                    proc.kill()
                    exited.result(timeout=5)
    content = capture.read_text()
    assert "Daemon is not running, starting fresh.\n" in content and "Daemon started — watching 0 repo(s)" in content, content
    return True


def compare(results, source, python, public_binary, daemon_binary):
    python = str(Path(python).absolute())
    commands = [
        ("crg-daemon", [python, "-m", "code_review_graph.daemon_cli"], [str(daemon_binary)] if daemon_binary else None),
        ("code-review-graph daemon", [python, "-m", "code_review_graph", "daemon"], [str(public_binary), "daemon"]),
    ]
    covered = {name: set() for name, _, _ in commands}
    with tempfile.TemporaryDirectory(prefix="crg-daemon-") as temp:
        root = Path(temp).resolve()
        state, env = isolated(root, source, python)
        first, second = repo(root, "first"), repo(root, "second")
        for program, reference, candidate in commands:
            prefix = "daemon-process/" + program + "/"
            observed = []
            live_results = []
            for command in (reference, candidate):
                if not command:
                    observed.append({})
                    live_results.append({})
                    continue
                process_env = env.copy()
                if command == candidate:
                    process_env.pop("PYTHONPATH", None)
                    process_env["PATH"] = "/usr/bin:/bin"
                observed.append(stopped(command, root, process_env, state))
                clean(root, state)
                live = {}
                try:
                    live.update(foreground(command, root, state, process_env, "start"))
                    clean(root, state)
                    live["fresh-restart"] = fresh_restart(command, root, state, process_env)
                except (AssertionError, OSError, ValueError, TimeoutError, subprocess.SubprocessError) as error:
                    live["failure"] = str(error)
                live_results.append(live)
                clean(root, state)
                for target in (first, second):
                    graph_dir = target / ".code-review-graph"
                    if graph_dir.exists():
                        shutil.rmtree(graph_dir)
                    (target / "billing.py").write_text("def original():\n    return 1\n")
            for key, expected in observed[0].items():
                actual = observed[1].get(key)
                results.check(prefix + key, expected, actual, cli=isinstance(expected, dict) and actual is not None)
            for key in ("initial-graph", "initial-worker-and-running-status", "duplicate-start", "edited-source-graph",
                        "live-add-and-index", "live-remove", "graceful-stop-and-cleanup", "fresh-restart"):
                results.check(prefix + "live/" + key, live_results[0].get(key), live_results[1].get(key))
            for engine, evidence in zip(("reference", "candidate"), live_results):
                results.check(prefix + engine + "-lifecycle-completed", {"complete": True},
                              {"complete": len(evidence) == 8} | ({"failure": evidence["failure"]} if "failure" in evidence else {}))
            by_command = {
                "start": ["live/initial-graph", "live/duplicate-start", "live/edited-source-graph"],
                "stop": ["stop/empty", "live/graceful-stop-and-cleanup"],
                "restart": ["live/fresh-restart"], "status": ["status/configured", "live/initial-worker-and-running-status"],
                "add": ["add/relative", "add/alias-used", "disk/config", "live/live-add-and-index"],
                "remove": ["remove/missing", "remove/by-path", "disk/removed-config", "live/live-remove"],
                "logs": ["logs/daemon-tail", "logs/repo-tail", "logs/repo-missing"],
            }
            for name, witnesses in by_command.items():
                if all(results.checks[prefix + key]["match"] and results.checks[prefix + key]["reference"] is not None
                       for key in witnesses):
                    covered[program].add(name)
        if daemon_binary:
            try:
                assert native_background([str(daemon_binary)], root, state, env) == 3
                native = {"complete": True}
            except (AssertionError, OSError, ValueError, TimeoutError, subprocess.SubprocessError) as error:
                native = {"complete": False, "error": str(error)}
            results.check("daemon-process/native/background-and-unrelated-process", {"complete": True}, native)
    return covered


def main():
    from public_parity import Results
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--daemon-binary", type=Path, required=True)
    parser.add_argument("--public-binary", type=Path, required=True)
    args = parser.parse_args()
    source = args.upstream.resolve()
    pinned_reference(source)
    result = Results()
    covered = compare(result, source, sys.executable, args.public_binary.resolve(), args.daemon_binary.resolve())
    failures = [name for name, item in result.checks.items() if not item["match"]]
    for name in failures:
        print(f"{name}: {result.checks[name]}", file=sys.stderr)
    print(f"{len(result.checks) - len(failures)}/{len(result.checks)} daemon observations and lifecycle checks")
    for name, commands in covered.items():
        print(f"{name}: {', '.join(sorted(commands))}")
    return int(bool(failures))


if __name__ == "__main__":
    sys.exit(main())
