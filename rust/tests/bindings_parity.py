"""Compare every installed MCP binding with the pinned platform registry and launch it."""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

import tomllib
import yaml
from mcp_contract import WireClient
from public_parity import pinned_reference
from uninstall_parity import snapshot, write


def location(platform, root, env, skills):
    with patch.dict(os.environ, env, clear=True):
        return skills.PLATFORMS[platform]["config_path"](root)


def file_doc(platform, path, skills, dsh):
    raw = path.read_text()
    if platform == "codex":
        return tomllib.loads(raw)
    if platform == "hermes":
        return yaml.safe_load(raw)
    if platform == "dsh":
        return dsh.load(raw)
    return json.loads(skills._strip_jsonc(raw))


def server(platform, doc, key):
    if platform == "dsh":
        return next(
            r["config"]
            for op in doc
            for r in op.get("insert", [])
            if r["id"] == "mcp-code-review-graph"
        )
    if platform == "continue":
        return next(e for e in doc[key] if e.get("name") == "code-review-graph")
    return doc[key]["code-review-graph"]


def setup(base, platform, scenario, env, skills, dsh):
    root = base / "repo"
    root.mkdir(parents=True)
    (base / "home").mkdir()
    subprocess.run(["git", "init", "--template=", "-q", str(root)], check=True, env=env)
    location0 = location(platform, root, env, skills)
    if scenario == "fresh":
        return location0
    manual = scenario == "handwritten"
    entry = {"command": "uvx", "args": ["code-review-graph", "serve"]}
    if manual:
        entry.update(command="third-party", args=["serve", "--feature"])
    if platform in ("opencode", "mimo"):
        entry = {"type": "local", "command": [entry["command"], *entry["args"]]}
    spec = skills.PLATFORMS[platform]
    key = spec["key"]
    if platform == "dsh":
        row = dsh.server_row(entry)
        text = "# keep this setting\n- other: !!js foo\n" + dsh.render_operation(row)
    elif platform == "hermes":
        text = "# keep this setting\ntheme: purple\n" + yaml.safe_dump(
            {key: {"mine": {"command": "other"}, "code-review-graph": entry}}, sort_keys=False
        )
    elif platform == "codex":
        text = (
            "# keep this setting\n[mcp_servers.mine]\n"
            'command = "other"\n\n[mcp_servers.code-review-graph]\ncommand = '
            + json.dumps(entry["command"])
            + "\nargs = "
            + json.dumps(entry["args"])
            + "\n"
        )
    elif platform == "continue":
        row = {"name": "code-review-graph", **entry}
        text = json.dumps(
            {"theme": "purple", key: [{"name": "mine"}, row, *([row] if not manual else [])]}
        )
    else:
        text = json.dumps(
            {
                "theme": "purple",
                key: {"mine": {"url": "https://example.test/v1"}, "code-review-graph": entry},
            },
            indent=2,
        )
        text = text.replace('  "theme"', '  // keep this setting\n  "theme"') + "\n"
        if platform == "copilot-cli" and not manual:
            text = text.replace(
                '  "theme"', '  "servers":{"keep":{},"code-review-graph":{}},\n  "theme"'
            )
        if platform == "mimo":
            location0 = location0.with_suffix(".jsonc")
        elif platform == "opencode":
            location0 = location0.with_suffix(".json")
    write(location0, text)
    return location0


def compare_run(base, platform, scenario, env, skills, dsh, cmd, binary, is_python):
    root = base / "repo"
    file = setup(base, platform, scenario, env, skills, dsh)
    args = [
        "install",
        "--platform",
        platform,
        "--repo",
        str(root),
        "--no-skills",
        "--no-hooks",
        "--no-instructions",
        "--yes",
    ]

    def run(args):
        res = subprocess.run(
            [*cmd, *args], input="", cwd=root, env=env, capture_output=True, text=True, timeout=45
        )
        assert res.returncode == 0, (res.stdout, res.stderr)
        return res.stdout

    before = snapshot(base)
    old = file.read_bytes() if file.exists() else None
    run([*args, "--dry-run"])
    assert snapshot(base) == before
    run(args)
    assert file.is_file(), file
    data = file_doc(platform, file, skills, dsh)
    baseline = file.read_bytes()
    if scenario == "handwritten":
        assert old == baseline, (platform, old, baseline)
    if old and b"keep this setting" in old:
        assert b"keep this setting" in baseline
    run(args)
    assert baseline == file.read_bytes(), platform
    if scenario == "handwritten":
        return data
    entry = server(platform, data, skills.PLATFORMS[platform]["key"])
    flattened = isinstance(entry["command"], list)
    tokens = entry["command"] if flattened else [entry["command"], *entry["args"]]
    native = [str(binary), "serve"]
    python = [sys.executable, "-m", "code_review_graph", "serve"]
    suffix = ["--repo", str(root)] if platform in ("opencode", "mimo") else []
    assert tokens == (python if is_python else native) + suffix, (platform, tokens)
    if scenario == "fresh":
        wire = WireClient(tokens, Path(entry.get("cwd", root)), env)
        try:
            hello = wire.call(
                "initialize",
                {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "platform-test", "version": "1"},
                },
            )
            assert hello["response"]["result"]["protocolVersion"] == "2025-11-25"
            wire.send({"method": "notifications/initialized"})
            assert any(
                t["name"] == "query_graph_tool"
                for t in wire.call("tools/list", {})["response"]["result"]["tools"]
            )
        finally:
            wire.close()
    if flattened:
        entry["command"] = ["<launcher>", "serve", *suffix]
    else:
        entry.update(command="<launcher>", args=["serve", *suffix])
    return data


def auto_detect(base, env, cmd, skills, dsh):
    root = base / "repo"
    root.mkdir(parents=True)
    git = shutil.which("git", path=env["PATH"])
    tools = base / "bin"
    tools.mkdir()
    (tools / "git").symlink_to(git)
    env = {**env, "PATH": str(tools)}
    subprocess.run([git, "init", "--template=", "-q", str(root)], env=env, check=True)
    layout = [
        "home/.claude",
        "home/.cursor",
        "home/.codeium/windsurf",
        "home/.continue",
        "home/.opencode",
        "xdg/mimocode",
        "home/.gemini/antigravity",
        "home/.qwen",
        "repo/.kiro",
        "home/.qoder",
        "home/.vscode/extensions/github.copilot-1.2",
        "home/.copilot",
        "hermes",
        "dsh",
        "home/.codebuddy",
    ]
    zed = location("zed", root, env, skills).parent
    for folder in [zed, *[base / n for n in layout]]:
        folder.mkdir(parents=True, exist_ok=True)

    def invoke(*more):
        result = subprocess.run(
            [
                *cmd,
                "init",
                "--repo",
                str(root),
                "--no-hooks",
                "--no-skills",
                "--no-instructions",
                "--yes",
                *more,
            ],
            cwd=root,
            env=env,
            input="",
            capture_output=True,
            text=True,
            timeout=40,
        )
        assert result.returncode == 0, (cmd, result.stderr, result.stdout)

    before = snapshot(base)
    invoke("--dry-run")
    assert snapshot(base) == before
    invoke()
    for name, spec in skills.PLATFORMS.items():
        loc = location(name, root, env, skills)
        assert loc.is_file(), (name, loc)
        assert server(name, file_doc(name, loc, skills, dsh), spec["key"])["command"]
    return set(skills.PLATFORMS)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--upstream", required=True, type=Path)
    ap.add_argument("--binary", required=True, type=Path)
    args = ap.parse_args()
    upstream = args.upstream.resolve()
    binary = args.binary.resolve()
    pinned_reference(upstream)
    sys.path.insert(0, str(upstream))
    from code_review_graph import dsh, skills

    assert Path(skills.__file__).resolve().is_relative_to(upstream)
    failures = []
    with tempfile.TemporaryDirectory(prefix="crg-bindings-") as tmp:
        base = Path(tmp).resolve() / "case"
        env = {k: v for k, v in os.environ.items() if k in ("PATH", "TMPDIR", "TEMP", "TMP")}
        env.update(
            HOME=str(base / "home"),
            CODEX_HOME=str(base / "codex"),
            HERMES_HOME=str(base / "hermes"),
            DSH_HOME=str(base / "dsh"),
            XDG_CONFIG_HOME=str(base / "xdg"),
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            NO_COLOR="1",
            PYTHONPATH=str(upstream),
            PYTHONDONTWRITEBYTECODE="1",
        )
        for platform in skills.PLATFORMS:
            for case in ("fresh", "upgrade", "handwritten"):
                pair = []
                try:
                    for cmd, py in [
                        ([sys.executable, "-B", "-m", "code_review_graph"], True),
                        ([str(binary)], False),
                    ]:
                        if base.exists():
                            shutil.rmtree(base)
                        pair.append(
                            compare_run(base, platform, case, env, skills, dsh, cmd, binary, py)
                        )
                    assert pair[0] == pair[1], pair
                    print("PASS", platform, case, flush=True)
                except (AssertionError, StopIteration, KeyError, FileNotFoundError) as err:
                    failures.append((platform, case))
                    print("DIFF", platform, case, repr(err), flush=True)
        for cmd in [[sys.executable, "-B", "-m", "code_review_graph"], [str(binary)]]:
            if base.exists():
                shutil.rmtree(base)
            assert auto_detect(base, env, cmd, skills, dsh) == set(skills.PLATFORMS)
        print("PASS init autodetection for all 18 registered clients")
    total = len(skills.PLATFORMS) * 3
    print(f"{total - len(failures)}/{total} registration cases; fresh cases launch both servers")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
