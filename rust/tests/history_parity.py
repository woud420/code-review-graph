"""Exercise the public history reader and an actual opt-in writer against the locked CLI."""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from differential import new_repo, verify_source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    upstream, native = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-history-") as temp:
        folder = Path(temp).resolve()
        repo, data = folder / "project", folder / "data"
        new_repo(repo, {"main.py": "def hello():\n    return 'Hello!'\n"})
        options = {k: v for k, v in os.environ.items() if not k.startswith(("PYTHON", "CRG_"))}
        options.update(CRG_HOME=str(folder/"registry"), PYTHONDONTWRITEBYTECODE="1")
        def invoke(label, cmd, **env):
            binary = [str(native)] if label == "Rust" else [sys.executable, "-m", "code_review_graph"]
            result = subprocess.run([*binary, *cmd], cwd=repo, env=dict(options, PYTHONPATH=str(upstream), **env),
                                    capture_output=True, timeout=30)
            return result.returncode, result.stdout, result.stderr
        count = 0
        def compare(*cmd, **env):
            nonlocal count
            reference, actual = (invoke(label, list(cmd), **env) for label in ("Python", "Rust"))
            assert reference[0] == 0 and reference == actual, (cmd, reference, actual)
            count += 1
        def history(*cmd, **env):
            compare("context-history", "--repo", str(repo), "--data-dir", str(data), *cmd, **env)
        history()
        history("--json", CRG_CONTEXT_HISTORY=" yes ")
        assert not data.exists()
        stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT12:00:00Z")
        def row(tool, ts, baseline, returned):
            return json.dumps(dict(v=1, tool=tool, ts=ts, baseline_tokens=baseline,
                                   returned_tokens=returned, saved_tokens=max(0, baseline-returned)))
        data.mkdir()
        log = data/"context-history.jsonl"
        old = data/"context-history.jsonl.1"
        old.write_text(row("older", "2018-01-01T16:45:59Z", 100, 60)+"\n")
        log.write_text("\n".join([row("impact", stamp, 10, 2), row("impact", stamp, 40, 80),
                                  row("调用", stamp, 15000, 4000), "", "corrupt", "{}", row("bad", stamp, False, 1)]))
        for by in ("tool", "day"):
            for flags in ([], ["--json"], ["--days","1"], ["--days","1","--json"]):
                history("--by", by, *flags)
        # Use identical absolute paths in each executable's output, rebuilding state between writers.
        probes = []
        for label in ("Python", "Rust"):
            args = ["--repo", str(repo)]
            env = dict(CRG_DATA_DIR=str(data), CRG_CONTEXT_HISTORY="on")
            assert invoke(label, ["build", "-q", *args], **env)[0] == 0
            log.unlink(missing_ok=True)
            old.unlink(missing_ok=True)
            assert invoke(label, ["impact", "--files", "main.py", *args], **env)[0] == 0
            recorded = json.loads(log.read_text())
            dt.datetime.strptime(recorded.pop("ts"), "%Y-%m-%dT%H:%M:%SZ")
            before = log.read_bytes()
            assert invoke(label, ["impact", "--files", "main.py", *args], **(env | {"CRG_READ_ONLY":"1"}))[0] == 0
            assert log.read_bytes() == before
            assert log.stat().st_mode & 0o777 == 0o600
            log.write_bytes(b"a" * 1048576)
            assert invoke(label, ["impact", "--files", "main.py", *args], **env)[0] == 0
            assert old.stat().st_size == 1048576
            probes.append(recorded)
        assert probes[0] == probes[1], probes
        print(f"Context history: {count} exact CLI observations plus matching recorded counts, rotation, permissions and read-only behavior")


if __name__ == "__main__":
    main()
