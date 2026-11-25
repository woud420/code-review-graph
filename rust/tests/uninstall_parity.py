"""Run each public uninstall against identical files, including comments, modes and links."""

import argparse
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

from public_parity import pinned_reference


def write(p, data):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(data)
    p.chmod(0o640)


def snapshot(base):
    items = {}
    for p in base.rglob("*"):
        rel = p.relative_to(base)
        if ".git" in rel.parts and "hooks" not in rel.parts:
            continue
        mode = p.lstat().st_mode
        items[str(rel)] = (
            mode,
            os.readlink(p) if stat.S_ISLNK(mode) else p.read_bytes() if p.is_file() else None,
        )
    return items


def run(args, base, env):
    return subprocess.run(
        args, cwd=base / "repo", env=env, input=b"", capture_output=True, timeout=40
    )


def git(root, *args, env):
    subprocess.run(["git", "-C", str(root), *args], check=True, env=env, capture_output=True)


def fixture(base, env, platform, source):
    from code_review_graph import dsh, skills

    repo, home = base / "repo", base / "home"
    home.mkdir(parents=True)
    for root in [repo, base / "second"]:
        root.mkdir()
        git(root, "init", "--template=", "-q", env=env)
        write(root / ".code-review-graph/graph.db", "database bytes\n")
        write(root / ".code-review-graph.db", "historical database\n")
    write(base / "external/graph.db", "unowned external data\n")
    write(
        home / ".code-review-graph/registry.json",
        json.dumps({"repos": [{"path": str(base / "second"), "data_dir": str(base / "external")}]}),
    )
    write(home / "user-settings", "unaltered\n")
    paths = {
        "claude": [("repo/.mcp.json", "mcpServers")],
        "claude-code": [("repo/.mcp.json", "mcpServers")],
        "codebuddy": [("repo/.mcp.json", "mcpServers")],
        "windsurf": [("home/.codeium/windsurf/mcp_config.json", "mcpServers")],
        "cursor": [("repo/.cursor/mcp.json", "mcpServers")],
        "opencode": [("repo/opencode.jsonc", "mcp"), ("repo/opencode.json", "mcp")],
        "mimo": [("repo/.mimocode/mimocode.json", "mcp"), ("repo/.mimocode/mimocode.jsonc", "mcp")],
        "antigravity": [("home/.gemini/antigravity/mcp_config.json", "mcpServers")],
        "gemini-cli": [("repo/.gemini/settings.json", "mcpServers")],
        "qwen": [("home/.qwen/settings.json", "mcpServers")],
        "kiro": [("repo/.kiro/settings/mcp.json", "mcpServers")],
        "qoder": [("repo/.qoder/mcp.json", "mcpServers")],
        "copilot": [("repo/.vscode/mcp.json", "servers")],
        "copilot-cli": [("home/.copilot/mcp-config.json", "mcpServers")],
        "zed": [
            (
                (
                    "home/Library/Application Support/Zed/settings.json"
                    if sys.platform == "darwin"
                    else "home/.config/zed/settings.json"
                ),
                "context_servers",
            )
        ],
    }
    for name, group in paths.items():
        for path, key in group:
            text = (
                '{\n  // keep this comment and spacing\n  "'
                + key
                + '": {\n    "mine": {"url":"https://example.test/db"},\n'
                '    "code-review-graph": {"command":"uvx"},\n  },\n  "theme": "navy",\n}\n'
            )
            if name == "copilot-cli":
                text = text.replace(
                    '"theme"',
                    '"servers": {"code-review-graph": {"command":"older"}, "mine":{}},\n  "theme"',
                )
            write(base / path, text)
    write(
        home / ".continue/config.json",
        '{"mcpServers":[{"name":"keep"},{"name":"code-review-graph"},{"name":"code-review-graph"}]}\n'
        "",
    )
    write(
        base / "custom-codex/config.toml",
        "# keep\n[mcp_servers.code-review-graph]\n"
        'command="uvx"\n\n# keep mine\n[mcp_servers.mine]\ncommand="mine"\n',
    )
    write(
        base / "custom-hermes/config.yaml",
        "theme: violet\nmcp_servers:\n  code-review-graph:\n"
        "    command: uvx\n  # keep this line\n  mine:\n    command: other\n",
    )
    operation = dsh.render_operation(
        dsh.server_row({"command": "uvx", "args": ["code-review-graph", "serve"]})
    )
    write(
        base / "custom-dsh/cordis.patch.yml",
        "# leave this non-JSON tag alone\n- other: !!js foo\n" + operation,
    )
    if platform:
        return
    if source == "installed":
        for target in ("codex", "claude"):
            flags = ["--platform", target, "--repo", str(repo), "--yes", "--gitignore"]
            if target == "claude":
                flags.append("--steering")
            # Install into clean strict JSON; this case also keeps unrelated data.
            if target == "claude":
                write(repo / ".mcp.json", '{"mcpServers":{"mine":{"command":"mine"}}}\n')
            proc = run(
                [sys.executable, "-B", "-m", "code_review_graph", "install", *flags], base, env
            )
            assert proc.returncode == 0, proc.stderr
        return
    write(
        repo / ".code-review-graph/install-manifest.json",
        json.dumps({"version": 1, "files": [".vscode/mcp.json", ".cursor/mcp.json", "mine.json"]})
        + "\n",
    )
    for path, key in [(".vscode/mcp.json", "servers"), (".cursor/mcp.json", "mcpServers")]:
        write(repo / path, json.dumps({key: {"code-review-graph": {}}}) + "\n")
    git(repo, "add", ".cursor/mcp.json", env=env)
    write(repo / "mine.json", '{"mine":true}\n')
    write(repo / ".claude/skills/debug-issue/SKILL.md", "generated skill")
    write(repo / ".claude/skills/my-skill/SKILL.md", "user skill")
    write(
        repo / "CLAUDE.md",
        "original\n\n" + skills._known_instruction_sections()[0].strip() + "\n\noriginal tail\n",
    )
    write(repo / ".gitignore", "dist/\n# Added by code-review-graph\n.code-review-graph/\n")
    owned = base / "custom-codex/skills/code-review-graph"
    write(owned / "SKILL.md", "unmodified native skill")
    write(owned / "EDITED.md", "current user changes")
    write(
        owned / ".code-review-graph-managed.json",
        json.dumps(
            {
                "version": 1,
                "files": {
                    "EDITED.md": "old",
                    "SKILL.md": hashlib.sha256((owned / "SKILL.md").read_bytes()).hexdigest(),
                },
            }
        ),
    )
    # An out-of-boundary config target and an in-boundary parent symlink.
    (repo / ".qoder/mcp.json").unlink()
    (repo / ".qoder/mcp.json").symlink_to(base / "external/graph.db")
    (repo / ".codebuddy").symlink_to(repo / ".claude", target_is_directory=True)
    write(repo / ".claude/settings.json", '{"other":17}\n')


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--upstream", required=True, type=Path)
    ap.add_argument("--binary", required=True, type=Path)
    args = ap.parse_args()
    upstream = args.upstream.resolve()
    binary = args.binary.resolve()
    pinned_reference(upstream)
    sys.path.insert(0, str(upstream))
    from code_review_graph import skills

    assert Path(skills.__file__).resolve().is_relative_to(upstream)
    cases = [(p, [], "mixed") for p in [*skills.PLATFORMS, "claude-code"]]
    cases.extend(
        [
            (None, [], "mixed"),
            (None, ["--keep-data"], "mixed"),
            (None, ["--keep-user-configs"], "mixed"),
            (None, ["--all-repos"], "mixed"),
            (None, ["--keep-data", "--all-repos"], "mixed"),
            (None, [], "installed"),
        ]
    )
    failures = 0
    with tempfile.TemporaryDirectory(prefix="crg-uninstall-") as tmp:
        base = Path(tmp).resolve() / "case"
        env = {k: v for k, v in os.environ.items() if k in ("PATH", "TMPDIR", "TEMP", "TMP")}
        env.update(
            HOME=str(base / "home"),
            CODEX_HOME=str(base / "custom-codex"),
            HERMES_HOME=str(base / "custom-hermes"),
            DSH_HOME=str(base / "custom-dsh"),
            XDG_CONFIG_HOME=str(base / "home/.config"),
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            NO_COLOR="1",
            PYTHONPATH=str(upstream),
            PYTHONDONTWRITEBYTECODE="1",
        )
        for platform, flags, state in cases:
            label = " ".join([platform or "full", *flags, state])
            outcomes = []
            for cmd in [[sys.executable, "-B", "-m", "code_review_graph"], [str(binary)]]:
                if base.exists():
                    shutil.rmtree(base)
                fixture(base, env, platform, state)
                original = snapshot(base)
                steps = []
                for options in (["--dry-run"], [], ["--yes"], ["--yes"]):
                    argv = [*cmd, "uninstall", "--repo", str(base / "repo"), *flags, *options]
                    if platform:
                        argv += ["--platform", platform]
                    proc = run(argv, base, env)
                    current = snapshot(base)
                    assert proc.returncode == 0, (label, argv, proc.stdout, proc.stderr)
                    if options != ["--yes"]:
                        assert current == original, (label, options)
                    assert (base / "external/graph.db").read_text() == "unowned external data\n"
                    assert (base / "home/user-settings").read_text() == "unaltered\n"
                    steps.append((proc.stdout, proc.stderr, current))
                assert steps[2][2] != original, label
                assert steps[2][2] == steps[3][2], label
                outcomes.append(steps)
            mismatch = False
            for i, (left, right) in enumerate(zip(*outcomes)):
                if left != right:
                    mismatch = True
                    if left[:2] != right[:2]:
                        import difflib

                        print(
                            "".join(
                                difflib.unified_diff(
                                    left[0].decode().splitlines(True),
                                    right[0].decode().splitlines(True),
                                    n=1,
                                )
                            )[:3000]
                        )
                        if left[1] != right[1]:
                            print("stderr", left[1], right[1])
                    if left[2] != right[2]:
                        names = sorted(
                            k
                            for k in set(left[2]) | set(right[2])
                            if left[2].get(k) != right[2].get(k)
                        )
                        print(label, i, "differing paths", names)
                        for n in names[:3]:
                            print(n, repr(left[2].get(n))[:350], repr(right[2].get(n))[:350])
            print("DIFF" if mismatch else "PASS", label, flush=True)
            failures += int(mismatch)
    print(
        f"{len(cases) - failures}/{len(cases)} cases match reports, files, modes, links and reruns"
    )
    return int(bool(failures))


if __name__ == "__main__":
    sys.exit(main())
