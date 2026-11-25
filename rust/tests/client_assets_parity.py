"""Compare every generated integration file for each platform, including executable hooks."""

import argparse
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

from bindings_parity import file_doc, location, server
from public_parity import pinned_reference
from uninstall_parity import snapshot


def content(path, raw, root, env, platform, cmd, py, binary, skills, dsh):
    target = location(platform, root, env, skills)
    if path == target or path == target.with_name(target.name + ".bak"):
        value = file_doc(platform, path, skills, dsh)
        key = skills.PLATFORMS[platform]["key"]
        if isinstance(value, dict) and value.get(key) is None:
            return value
        try:
            entry = server(platform, value, key)
        except (KeyError, StopIteration):
            return value
        tokens = (
            entry["command"]
            if isinstance(entry["command"], list)
            else [entry["command"], *entry["args"]]
        )
        head = [sys.executable, "-m", "code_review_graph"] if py else [str(binary)]
        if tokens[: len(head)] == head:
            rest = tokens[len(head) :]
            if isinstance(entry["command"], list):
                entry["command"] = ["<launcher>", *rest]
            else:
                entry.update(command="<launcher>", args=rest)
        return value
    if path.suffix == ".json":
        return json.loads(normalize_launchers(raw.decode(), root, binary))
    if (
        path.suffix in (".md", ".yaml", ".toml")
        or path.name.startswith(".")
        and path.suffix != ".sh"
    ):
        return raw
    if path.suffix in (".ts", ".sh") or path.parent.name == "hooks":
        return normalize_launchers(raw.decode(), root, binary)
    return raw


def normalize_launchers(raw, root, binary):
    # Replace only the two intended launch vectors and their exact path guard.
    python = "'" + sys.executable + "'"
    native = "'" + str(binary) + "'"
    for source in (python, native):
        raw = raw.replace("[ -x " + source + " ]", "[ -x <launcher> ]")
    raw = raw.replace(python + " -m code_review_graph ", "<launcher> ").replace(
        native + " ", "<launcher> "
    )
    for argv in [[sys.executable, "-m", "code_review_graph"], [str(binary)]]:
        raw = raw.replace(json.dumps(argv), json.dumps(["<launcher>"]))
    return raw.replace("--repo '" + str(root) + "'", '--repo "' + str(root) + '"')


def observe(base, env, platform, cmd, py, binary, skills, dsh, major, mixed):
    root = base / "repo"
    root.mkdir(parents=True)
    (base / "home").mkdir()
    bindir = base / "bin"
    bindir.mkdir()
    (bindir / "git").symlink_to(shutil.which("git", path=env["PATH"]))
    version = bindir / "opencode"
    version.write_text("#!/bin/sh\necho " + major + ".3.1\n")
    version.chmod(0o755)
    env = {**env, "PATH": str(bindir)}
    subprocess.run(["git", "init", "--template=", "-q", str(root)], check=True, env=env)
    options = [*cmd, "install", "--platform", platform, "--repo", str(root), "--yes"]

    def run(flags):
        p = subprocess.run(
            [*options, *flags],
            env=env,
            cwd=root,
            input="",
            text=True,
            capture_output=True,
            timeout=30,
        )
        assert p.returncode == 0, (platform, p.stdout, p.stderr)

    if mixed:
        user_hook = {"command": "my-preexisting-hook"}
        settings = [
            root / ".claude/settings.json",
            root / ".qoder/settings.json",
            root / ".gemini/settings.json",
            root / ".codebuddy/settings.json",
            base / "home/.cursor/hooks.json",
        ]
        for path in settings:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                json.dumps({"mine": "preserve", "hooks": {"BeforeAnything": [user_hook]}}, indent=2)
                + "\n"
            )
        for name in [
            "CLAUDE.md",
            "AGENTS.md",
            "QODER.md",
            "GEMINI.md",
            "CODEBUDDY.md",
            ".cursorrules",
            ".windsurfrules",
        ]:
            (root / name).write_text("User instructions\n")
    before = snapshot(base)
    run(["--dry-run"])
    assert snapshot(base) == before
    run([])
    applied = snapshot(base)
    run([])
    assert snapshot(base) == applied, (
        "rerun",
        platform,
        [k for k in set(applied) | set(snapshot(base)) if applied.get(k) != snapshot(base).get(k)],
    )
    if mixed:
        for p in settings:
            doc = json.loads(skills._strip_jsonc(p.read_text()))
            assert doc["mine"] == "preserve" and doc["hooks"]["BeforeAnything"] == [user_hook], (
                p,
                doc,
            )
    files = {}
    for k, (mode, raw) in applied.items():
        if before.get(k) == (mode, raw) or not stat.S_ISREG(mode):
            continue
        path = base / k
        value = content(path, raw, root, env, platform, cmd, py, binary, skills, dsh)
        files[k] = (value, stat.S_IMODE(mode))
    assert files, platform
    cleanup = [*cmd, "uninstall", "--yes", "--repo", str(root)]
    cleaned = {}
    for step in range(2):
        proc = subprocess.run(
            cleanup, env=env, cwd=root, capture_output=True, input="", text=True, timeout=30
        )
        assert proc.returncode == 0, proc.stderr
        now = snapshot(base)
        if step:
            assert now == cleaned, ("uninstall rerun", platform)
        cleaned = now
    leftovers = {
        k: (
            content(base / k, value, root, env, platform, cmd, py, binary, skills, dsh),
            stat.S_IMODE(mode),
        )
        for k, (mode, value) in cleaned.items()
        if stat.S_ISREG(mode) and before.get(k) != (mode, value)
    }
    files.update({"after uninstall/" + k: v for k, v in leftovers.items()})
    return files


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--binary", type=Path, required=True)
    ap.add_argument("--upstream", type=Path, required=True)
    args = ap.parse_args()
    up = args.upstream.resolve()
    binary = args.binary.resolve()
    pinned_reference(up)
    sys.path.insert(0, str(up))
    from code_review_graph import dsh, skills

    assert Path(skills.__file__).resolve().is_relative_to(up)
    errors = []
    with tempfile.TemporaryDirectory(prefix="crg-client-assets-") as t:
        base = Path(t).resolve() / "case"
        env = {k: v for k, v in os.environ.items() if k in ("PATH", "TMPDIR", "TEMP", "TMP")}
        env.update(
            HOME=str(base / "home"),
            CODEX_HOME=str(base / "codex"),
            HERMES_HOME=str(base / "hermes"),
            DSH_HOME=str(base / "dsh"),
            XDG_CONFIG_HOME=str(base / "xdg"),
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            PYTHONPATH=str(up),
            PYTHONDONTWRITEBYTECODE="1",
            NO_COLOR="1",
        )
        cases = [
            *((p, "2") for p in skills.PLATFORMS if p != "codex"),
            ("opencode", "1"),
        ]
        cases = [(p, v, False) for p, v in cases] + [
            (p, "2", True)
            for p in ["cursor", "gemini-cli", "qoder", "codebuddy", "opencode", "copilot"]
        ]
        for platform, version, mixed in cases:
            # installer_parity.py separately checks the native Codex read-only launcher.
            try:
                pair = []
                for cmd, py in [
                    ([sys.executable, "-B", "-m", "code_review_graph"], True),
                    ([str(binary)], False),
                ]:
                    if base.exists():
                        shutil.rmtree(base)
                    pair.append(
                        observe(base, env, platform, cmd, py, binary, skills, dsh, version, mixed)
                    )
                a, b = pair
                diff = [k for k in sorted(set(a) | set(b)) if a.get(k) != b.get(k)]
                assert not diff, [(k, str(a.get(k))[:240], str(b.get(k))[:240]) for k in diff]
                print(
                    "PASS",
                    platform,
                    version,
                    "mixed" if mixed else "fresh",
                    len(a),
                    "artifacts",
                    flush=True,
                )
            except (AssertionError, KeyError, ValueError) as e:
                errors.append(platform)
                print("DIFF", platform, version, repr(e), flush=True)
    print(f"{len(cases) - len(errors)}/{len(cases)} client artifact sets matched")
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
