"""Exercise --gitignore and Claude steering through both installed entry points."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from differential import git, verify_source


def exercise(base, command, env):
    root = base/"repo"
    root.mkdir()
    git(root, "init", "-q")
    (root/"CLAUDE.md").write_text("existing instructions\n")
    git(root, "add", "CLAUDE.md")
    (root/".gitignore").write_text("other.log\n")
    def install(*opts):
        return subprocess.run([*command, "install", "--repo", str(root), "--platform", "claude", "--yes", *opts],
                              cwd=root, env=env, capture_output=True, check=True).stdout.decode()
    state = []
    def ignore():
        return (root/".gitignore").read_bytes()
    flags = ["--gitignore","--steering"]
    initial = ignore()
    result = install(*flags, "--dry-run")
    assert "Would add the steering hook" in result and ignore() == initial and not (root/".mcp.json").exists()
    install(*flags)
    first = ignore()
    assert b"/.claude/" in first and b"/.mcp.json" in first and b"/CLAUDE.md" not in first
    state.append(first)
    settings = root/".claude/settings.json"
    def groups():
        return json.loads(settings.read_bytes())["hooks"]
    def steering():
        return [hook for group in groups().get("PreToolUse",[]) for hook in group["hooks"] if " enrich" in hook["command"]]
    assert len(steering()) == 1 and steering()[0]["timeout"] == 5
    install(*flags)
    assert ignore() == first and len(steering()) == 1
    doc = json.loads(settings.read_bytes())
    custom = {"type":"command","command":"echo project-hook"}
    doc["hooks"]["PreToolUse"][0]["hooks"].append(custom)
    settings.write_text(json.dumps(doc,indent=2)+"\n")
    install("--no-steering", "--dry-run")
    assert len(steering()) == 1
    install("--no-steering")
    assert not steering() and any(custom in group["hooks"] for group in groups()["PreToolUse"])
    install("--no-hooks", "--steering")
    assert not steering()
    (root/".gitignore").write_bytes(first.replace(b"/.mcp.json\n",b"*.manual\n"))
    before = ignore()
    install("--gitignore")
    assert ignore() == before
    state.append(before)
    return state


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--upstream", type=Path, required=True)
    args = parser.parse_args()
    source, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(source)
    with tempfile.TemporaryDirectory(prefix="crg-install-options-") as folder:
        base = Path(folder).resolve()
        results = []
        for cmd in ([sys.executable,"-m","code_review_graph"], [str(binary)]):
            home = base/"home"
            home.mkdir()
            env = {k:v for k,v in os.environ.items() if k in ("PATH","TMPDIR","TMP","TEMP")}
            env.update(HOME=str(home), CODEX_HOME=str(home/".codex"), CRG_HOME=str(home/"state"),
                       PYTHONPATH=str(source), GIT_CONFIG_GLOBAL=os.devnull,GIT_CONFIG_NOSYSTEM="1")
            results.append(exercise(base,cmd,env))
            shutil.rmtree(base/"repo")
            shutil.rmtree(home)
        assert results[0] == results[1]
    print("Install options: matching gitignore bytes, dry runs, tracked and custom entries, steering installation, reruns and scoped removal")


if __name__ == "__main__":
    main()
