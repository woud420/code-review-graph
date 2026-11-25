"""Compare installed assets and start the actual server using each generated launcher."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib

from mcp_contract import WireClient
from public_parity import pinned_reference


def inventory(root, home):
    return {
        (scope, str(f.relative_to(folder))): (
            hashlib.sha256(f.read_bytes()).digest(),
            f.stat().st_mode,
        )
        for scope, folder in (("repo", root), ("home", home))
        for f in folder.rglob("*")
        if f.is_file() and (".git" not in f.parts or "hooks" in f.parts)
    }


def call(cmd, argv, root, env, success=True):
    proc = subprocess.run([*cmd, *argv], cwd=root, env=env, capture_output=True, timeout=30)
    assert (proc.returncode == 0) == success, (argv, proc.stdout, proc.stderr)
    return proc.stdout.decode()


def install(cmd, root, home, platform, source, python):
    env = {k: v for k, v in os.environ.items() if k in ("TMPDIR", "TEMP", "TMP", "PATH")}
    env.update(
        HOME=str(home),
        CODEX_HOME=str(home / ".codex"),
        CRG_HOME=str(home / "registry"),
        PYTHONDONTWRITEBYTECODE="1",
        NO_COLOR="1",
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
    )
    if python:
        env["PYTHONPATH"] = str(source)
    subprocess.run(
        ["git", "init", "-q", "--template=", "-b", "main", str(root)], env=env, check=True
    )
    home.mkdir(parents=True, exist_ok=True)
    codex = home / ".codex"
    codex.mkdir(exist_ok=True)
    settings = codex / "config.toml" if platform == "codex" else root / ".mcp.json"
    if platform == "claude":
        settings.write_text(
            json.dumps({"mcpServers": {"untouched": {"command": "other"}}}, indent=2) + "\n"
        )
    else:
        settings.write_text('# keep this comment\n[mcp_servers.untouched]\ncommand = "other"\n')
    flags = ["--platform", platform, "--repo", str(root)]
    before = inventory(root, home)
    preview = call(cmd, ["init", *flags, "--dry-run"], root, env)
    assert "No files were modified." in preview and inventory(root, home) == before
    call(cmd, ["install", *flags, "--yes"], root, env)
    get_servers = lambda: (
        json.loads(settings.read_text())["mcpServers"]
        if platform == "claude"
        else tomllib.loads(settings.read_text())["mcp_servers"]
    )
    servers = get_servers()
    assert servers["untouched"] == {"command": "other"}
    if platform == "codex":
        assert "# keep this comment" in settings.read_text()
    entry = servers["code-review-graph"]
    assert Path(entry["cwd"]).resolve() == root
    if not python:
        assert Path(entry["command"]).resolve() == Path(cmd[0]).resolve() and entry["args"] == [
            "serve"
        ]
        just_installed = inventory(root, home)
        call(cmd, ["install", *flags, "--yes"], root, env)
        assert inventory(root, home) == just_installed
        assert not list(codex.rglob("*.py"))
    wire = WireClient([entry["command"], *entry["args"]], Path(entry["cwd"]), env)
    try:
        hello = wire.call(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "installed-launcher", "version": "1"},
            },
        )
        assert hello["response"]["result"]["protocolVersion"] == "2025-11-25"
        wire.send({"method": "notifications/initialized"})
        tools = wire.call("tools/list", {})["response"]["result"]["tools"]
        assert any(t["name"] == "query_graph_tool" for t in tools)
    finally:
        wire.close()
    docs = {p.relative_to(root).as_posix(): p.read_bytes() for p in root.rglob("*.md")}
    hooks = root / ".git/hooks"
    modes = {p.name: bool(p.stat().st_mode & 0o111) for p in hooks.iterdir() if p.is_file()}
    assert len(modes) == 4 and all(modes.values()), modes
    before = inventory(root, home)
    call(cmd, ["uninstall", *flags, "--dry-run", "--yes"], root, env)
    assert inventory(root, home) == before
    call(cmd, ["uninstall", *flags, "--yes"], root, env)
    assert get_servers() == {"untouched": {"command": "other"}}
    if platform == "claude":
        custom = {"command": sys.executable, "args": ["-m", "code_review_graph", "serve"], "env": {"MODE": "manual"}}
        settings.write_text(json.dumps({"mcpServers": {"code-review-graph": custom}}, indent=2))
        before = settings.read_bytes()
        call(cmd, ["install", *flags, "--yes"], root, env)
        assert settings.read_bytes() == before
    else:
        custom = '[mcp_servers.code-review-graph]\ncommand = "uv"\nargs = ["run","--project","other","code-review-graph","serve"]\n'
        settings.write_text(custom)
        call(cmd, ["install", *flags, "--yes"], root, env)
        assert settings.read_text() == custom
    return docs, modes


def upgrade(base, binary, source, platform):
    root, home = base / "repo", base / "home"
    home.mkdir()
    env = {k: v for k, v in os.environ.items() if k in ("PATH", "TMPDIR", "TEMP", "TMP")}
    env.update(
        HOME=str(home),
        CODEX_HOME=str(home / ".codex"),
        CRG_HOME=str(home / "registry"),
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        NO_COLOR="1",
    )
    subprocess.run(["git", "init", "-q", "--template=", str(root)], env=env, check=True)
    flags = ["install", "--platform", platform, "--repo", str(root), "--yes"]
    call(
        [sys.executable, "-m", "code_review_graph"],
        flags,
        root,
        env | {"PYTHONPATH": str(source), "PYTHONDONTWRITEBYTECODE": "1"},
    )
    call([str(binary)], flags, root, env)
    path = home / ".codex/hooks.json" if platform == "codex" else root / ".claude/settings.json"
    groups = json.loads(path.read_text())["hooks"]
    for event, items in groups.items():
        assert len(items) == 1, (event, items)
        command = items[0]["hooks"][0]["command"]
        assert " -m code_review_graph " not in command and str(binary) in command, command
    assert not list((home / ".codex").rglob("*.py"))
    before = inventory(root, home)
    call([str(binary)], flags, root, env)
    assert inventory(root, home) == before
    shutil.rmtree(root)
    shutil.rmtree(home)
    print(
        f"{platform}: adopted upstream install, migrated launcher and hooks, verified idempotence"
    )


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", required=True, type=Path)
    p.add_argument("--binary", required=True, type=Path)
    args = p.parse_args()
    binary, source = args.binary.resolve(), args.upstream.resolve()
    pinned_reference(source)
    with tempfile.TemporaryDirectory(prefix="crg-install-") as directory:
        base = Path(directory).resolve()
        for platform in ("claude", "codex"):
            observed = []
            for mode, cmd in [
                ("reference", [sys.executable, "-m", "code_review_graph"]),
                ("native", [str(binary)]),
            ]:
                root = base / "repo"
                home = base / "home"
                observed.append(install(cmd, root, home, platform, source, mode == "reference"))
                shutil.rmtree(root)
                shutil.rmtree(home)
            assert observed[0] == observed[1], platform
            upgrade(base, binary, source, platform)
            print(
                f"{platform}: dry runs, docs, executable hooks, MCP launcher, unrelated configuration and scoped removal verified"
            )
        root = base / "repo"
        home = base / "home"
        root.mkdir()
        home.mkdir()
        foreign = base / "outside"
        foreign.write_text("unchanged")
        (root / ".mcp.json").symlink_to(foreign)
        env = dict(
            os.environ,
            HOME=str(home),
            CODEX_HOME=str(home / ".codex"),
            CRG_HOME=str(home / "registry"),
        )
        call(
            [str(binary)],
            ["install", "--repo", str(root), "--platform", "claude", "--yes"],
            root,
            env,
            False,
        )
        assert not (root / ".gitignore").exists() and foreign.read_text() == "unchanged"
        call([str(binary)], ["readonly", "build", "--repo", str(root)], root, env, False)
        assert not (root / ".code-review-graph/graph.db").exists()
        print("native symlink boundaries and read-only command restrictions verified")


if __name__ == "__main__":
    main()
