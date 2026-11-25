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


def isolated(root, source):
    state = root / "state"
    assert state.parent == root and root.is_absolute()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "GIT_"))}
    env.update(CRG_HOME=str(state), PYTHONPATH=str(source), PYTHONDONTWRITEBYTECODE="1",
               PYTHONNOUSERSITE="1", PATH=f"{Path(sys.executable).parent}:/usr/bin:/bin",
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


def foreground(command, root, state, env):
    """Run the identical lifecycle for each implementation, assert outcomes individually."""
    outcomes = {}
    first, second = root / "first", root / "second"
    init = run(command + ["add", str(first), "--alias", "fee"], root, env)
    assert init["exit"] == 0, init
    conf = state / "watch.toml"
    conf.write_text(conf.read_text().replace("poll_interval = 2", "poll_interval = 1"))
    log_path = root / "foreground-capture"
    with log_path.open("w") as log, ThreadPoolExecutor(1) as pool:
        proc = subprocess.Popen(command + ["start", "--foreground"], cwd=root, env=env,
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--daemon-binary", type=Path, required=True)
    parser.add_argument("--public-binary", type=Path, required=True)
    args = parser.parse_args()
    source = args.upstream.resolve()
    pinned_reference(source)
    commands = [
        ([sys.executable, "-m", "code_review_graph.daemon_cli"], [str(args.daemon_binary.resolve())]),
        ([sys.executable, "-m", "code_review_graph", "daemon"], [str(args.public_binary.resolve()), "daemon"]),
    ]
    with tempfile.TemporaryDirectory(prefix="crg-daemon-") as temp:
        root = Path(temp).resolve()
        state, env = isolated(root, source)
        first, second = repo(root, "first"), repo(root, "second")
        for reference, candidate in commands:
            results = []
            for command in (reference, candidate):
                results.append(stopped(command, root, env, state))
                clean(root, state)
            failures = [k for k in results[0] if results[0][k] != results[1][k]]
            for key in failures:
                print(f"{key}: Python={results[0][key]!r} Rust={results[1][key]!r}", file=sys.stderr)
            assert not failures, f"{len(failures)} mismatches for {candidate}"
            print(f"{candidate[-1]}: {len(results[0])} exact CLI responses/config comparisons")
        for command in commands[0]:
            checks = foreground(command, root, state, env)
            assert len(checks) == 7
            print(f"{command[-1]}: {len(checks)} live lifecycle assertions")
            clean(root, state)
            for target in (first, second):
                shutil.rmtree(target / ".code-review-graph")
                (target / "billing.py").write_text("def original():\n    return 1\n")
        print(f"Native: {native_background(commands[0][1], root, state, env)} additional background/PID checks")


if __name__ == "__main__":
    main()
