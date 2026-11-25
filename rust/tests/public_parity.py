"""Cross-process compatibility checks for both public console entry points and MCP stdio.

Use --cutover to also require the CLI help and flag inventory, a daemon binary, and
behavioral coverage of every advertised operation. Both modes return 1 on mismatches.
--report saves complete reference/candidate observations, including the MCP schemas.
"""
import argparse
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

from differential import UPSTREAM_REV, git, verify_source
from mcp_contract import QUERY_CASES, REVIEW_CASES, inspect

CONTRACT_DIGEST = "a81e00d61eab717061a2be38cfbc29ea15d237ecb95475c0076924d6ed275f65"
FILES = {
    "lib.ts": "export function target() { return 1; }\n",
    "client.ts": 'import {target} from "./lib.js";\nexport function caller() {\n target();\n target();\n}\n',
}
SAVED_QUERIES = {
    "status/text": ["status"], "status/json": ["status", "--json"], "status/quiet": ["status", "-q"],
    "query/callers": ["query", "callers_of", "target"], "query/callees": ["query", "callees_of", "caller"],
    "query/missing": ["query", "callers_of", "notInFixture"], "search/limit": ["search", "target", "--limit", "1"],
}
REVIEW_COMMANDS = {
    "impact/one-file": ["impact", "--files", "lib.ts", "--depth", "1", "--max-results", "1"],
    "dead-code/limit": ["dead-code", "--kind", "Function", "--limit", "1", "--json"],
    "large-functions/limit": ["large-functions", "--min-lines", "1", "--limit", "1"],
    "flows/limit": ["flows", "--limit", "1"], "flow/missing": ["flow", "--name", "absent"],
    "communities/list": ["communities", "--min-size", "1"], "community/missing": ["community", "--name", "absent"],
    "architecture/minimal": ["architecture", "--detail-level", "minimal"],
    "detect-changes/clean": ["detect-changes", "--brief"],
    "refactor/dead": ["refactor", "dead_code", "--kind", "Function"],
    "context-history/empty": ["context-history", "--json"],
    "forget/dry-run": ["forget", "--dry-run", "lib.ts"],
}


def pinned_reference(source):
    verify_source(source)
    files = sorted(p for folder in ("code_review_graph", "docs") for p in (source / folder).rglob("*")
                   if p.is_file() and "__pycache__" not in p.parts)
    digest = hashlib.sha256()
    for path in [*files, source / "pyproject.toml"]:
        digest.update(path.relative_to(source).as_posix().encode() + b"\0" + hashlib.sha256(path.read_bytes()).digest())
    if digest.hexdigest() != CONTRACT_DIGEST:
        raise ValueError(f"reference package, documentation or manifest differs from {UPSTREAM_REV}")


def run(command, cwd, env):
    result = subprocess.run(command, cwd=cwd, env=env, capture_output=True, encoding="utf-8", timeout=45)
    return {"exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr}


def stdout_value(observation):
    output = observation["stdout"]
    if output.startswith(("{", "[")):
        try:
            return {"json": json.loads(output)}
        except json.JSONDecodeError:
            pass
    return {"text": output}


def json_equal(a, b):
    return json.dumps(a, sort_keys=True) == json.dumps(b, sort_keys=True)


class Results:
    def __init__(self):
        self.checks = {}
        self.inventory = {}

    def check(self, name, reference, candidate, cli=False):
        if name in self.checks:
            raise AssertionError(f"duplicate check {name}")
        equal = json_equal(reference, candidate)
        record = {"match": equal, "reference": reference, "candidate": candidate}
        if cli and not equal and candidate is not None:
            record["json_format_only"] = (reference["exit"] == candidate["exit"]
                                         and reference["stderr"] == candidate["stderr"]
                                         and json_equal(stdout_value(reference), stdout_value(candidate)))
        self.checks[name] = record


def setup(repo):
    repo.mkdir()
    for name, text in FILES.items():
        (repo / name).write_text(text, encoding="utf-8")
    git(repo, "init", "-q", "-b", "main")
    git(repo, "add", "--", *FILES)
    git(repo, "commit", "-qm", "oracle fixture")


def lifecycle(command, repo, env):
    results = {}
    def probe(name, *args):
        results[name] = run(command + list(args), repo, env)
    common = ["--repo", str(repo)]
    probe("version", "--version")
    probe("short-version", "-v")
    probe("unknown-command", "querry")
    probe("status/missing", "status", "--json", *common)
    probe("query/unbuilt", "query", "callers_of", "target", *common)
    probe("build/no-postprocess", "build", "--skip-postprocess", *common)
    probe("query/callers", "query", "callers_of", "target", *common)
    probe("query/callees", "query", "callees_of", "caller", *common)
    probe("query/unknown-symbol", "query", "callers_of", "notInFixture", *common)
    probe("query/invalid-pattern", "query", "no_such_pattern", "target", *common)
    probe("query/missing-arg", "query", "callers_of", *common)
    (repo / "lib.ts").write_text(FILES["lib.ts"].replace("target", "renamed"), encoding="utf-8")
    (repo / "client.ts").write_text(FILES["client.ts"].replace("target", "renamed"), encoding="utf-8")
    git(repo, "commit", "-qam", "rename")
    probe("update/rename", "update", "--skip-postprocess", *common)
    probe("query/after-rename", "query", "callers_of", "renamed", *common)
    probe("update/noop", "update", "--skip-postprocess", *common)
    (repo / "lib.ts").unlink()
    git(repo, "commit", "-qam", "delete")
    probe("update/delete", "update", "--skip-postprocess", *common)
    probe("query/after-delete", "query", "callers_of", "renamed", *common)
    probe("build/quiet", "build", "-q", "--skip-postprocess", *common)
    return results


def adopt_graph(command, repo, env):
    def invoke(*argv):
        return run([*command, *argv, "--repo", str(repo)], repo, env)
    results = {"unchanged": invoke("update", "--skip-postprocess")}
    for name, contents in FILES.items():
        (repo / name).write_text(contents.replace("target", "renamed"), encoding="utf-8")
    git(repo, "commit", "-qam", "update saved graph")
    results["rename"] = invoke("update", "--skip-postprocess")
    results["callers"] = invoke("query", "callers_of", "renamed")
    results["old-symbol"] = invoke("query", "callers_of", "target")
    return results


def saved_graph(command, repo, env, extended):
    cases = SAVED_QUERIES | REVIEW_COMMANDS if extended else SAVED_QUERIES
    return {name: run(command + [*args, "--repo", str(repo)], repo, env) for name, args in cases.items()}


def reference_cli_tree(python, cwd, env):
    # Ask the installed reference parser for hidden spellings too. Every help result
    # is subsequently obtained by launching the actual public command in each process.
    script = """
import argparse, json
from code_review_graph.cli import build_parser
entries = {}
def walk(p, path):
    entries[path] = sorted(s for a in p._actions for s in a.option_strings)
    for a in p._actions:
        if isinstance(a, argparse._SubParsersAction):
            for key, child in a.choices.items():
                walk(child, (path + ' ' + key).strip())
walk(build_parser(), '')
print(json.dumps(entries))
"""
    result = run([python, "-c", script], cwd, env)
    if result["exit"]:
        raise RuntimeError(result["stderr"])
    return json.loads(result["stdout"])


def advertised_options(help_text):
    flags = set()
    for line in help_text.splitlines():
        if re.match(r"^ {2,8}-", line):
            declaration = re.split(r"\s{2,}", line.strip(), maxsplit=1)[0]
            flags.update(re.findall(r"(?<![\w-])--?[A-Za-z][\w-]*", declaration))
    return sorted(flags)


def advertised_commands(help_text):
    commands = set()
    reading = False
    for line in help_text.splitlines():
        if line.strip() in {"Commands:", "Subcommands:", "positional arguments:"}:
            reading = True
            continue
        if reading and (not line.strip() or (not line.startswith(" ") and line.endswith(":"))):
            reading = False
        if reading:
            choice = re.match(r"^\s+\{([a-z][\w,-]*)\}", line)
            if choice:
                commands.update(choice[1].split(","))
            entry = re.match(r"^ {2,4}([a-z][\w-]*)\s{2,}\S", line)
            if entry and ("<command>" in help_text or "Commands:" in help_text or "Subcommands:" in help_text):
                commands.add(entry[1])
    return commands


def cli_inventory(results, reference, candidate, ref_daemon, daemon, tree, repo, env, candidate_env):
    inventories = {}
    for program, expected, actual, paths in [
        ("code-review-graph", reference, candidate, set(tree)),
        ("crg-daemon", ref_daemon, daemon, {p.removeprefix("daemon").strip() for p in tree
                                            if p == "daemon" or p.startswith("daemon ")}),
    ]:
        pending = sorted(paths, key=lambda p: (len(p.split()), p))
        records = {}
        while pending:
            path = pending.pop(0)
            if path in records:
                continue
            args = [*path.split(), "--help"]
            a = run(expected + args, repo, env)
            b = run(actual + args, repo, candidate_env) if actual else None
            prefix = program + (" " + path if path else "")
            results.check("help/" + prefix, a, b, cli=True)
            entry = {"reference": advertised_options(a["stdout"]) if a["exit"] == 0 else None,
                     "candidate": advertised_options(b["stdout"]) if b and b["exit"] == 0 else None}
            if program == "code-review-graph":
                entry["reference_parser_options"] = tree.get(path)
                for option in sorted(set(tree.get(path, [])) - set(entry["reference"] or [])):
                    # Force argument parsing to fail before the install/init handler runs.
                    # An unsupported hidden flag will appear in the diagnostic as well.
                    probe = [*path.split(), option, "--parity-invalid-option"]
                    results.check("hidden-option/" + prefix + " " + option,
                                  run(expected + probe, repo, env),
                                  run(actual + probe, repo, candidate_env) if actual else None, cli=True)
            records[path] = entry
            results.check("flags/" + prefix, entry["reference"], entry["candidate"])
            if b and b["exit"] == 0:
                # Argparse also prints braces around ordinary positional choices.
                # Only treat that section as subcommands at known dispatcher levels.
                dispatcher = path == "" or any(name.startswith(path + " ") for name in paths)
                if dispatcher or re.search(r"^(?:Commands|Subcommands):$", b["stdout"], re.MULTILINE):
                    for child in sorted(advertised_commands(b["stdout"])):
                        if child not in path.split():
                            pending.append((path + " " + child).strip())
        inventories[program] = records
    return inventories


def compare_group(results, prefix, reference, candidate, cli=False):
    for name in sorted(reference.keys() | candidate.keys()):
        results.check(prefix + "/" + name, reference.get(name), candidate.get(name), cli)


def reference_witnesses(life, seeded, wire, inventory):
    for key in ("build/no-postprocess", "query/callers", "update/rename", "update/noop", "update/delete"):
        if life[key]["exit"] != 0:
            raise RuntimeError(f"broken reference lifecycle {key}: {life[key]}")
    for observations, name in [(life, "query/callers"), (life, "query/after-rename"), (seeded, "query/callers")]:
        data = json.loads(observations[name]["stdout"])
        lines = [hit["call_site"]["line"] for hit in data["results"]]
        if data["status"] != "ok" or sorted(lines) != [3, 4]:
            raise RuntimeError(f"{name}: expected two independent reference call sites, got {data}")
    for key in ("query/after-delete", "query/unknown-symbol"):
        if json.loads(life[key]["stdout"])["status"] != "not_found":
            raise RuntimeError(f"{key}: missing-symbol reference witness failed")
    if len(inventory["tools"]) != 30 or len(inventory["prompts"]) != 5:
        raise RuntimeError("pinned reference did not advertise all 30 tools and 5 prompts")
    if wire["tools/call/limit"]["response"]["result"]["structuredContent"]["results_omitted"] != 1:
        raise RuntimeError("reference MCP did not enforce the one-result limit")
    if not wire["tools/call/missing-arg"]["response"]["result"]["isError"]:
        raise RuntimeError("reference MCP failed to reject missing argument")


def capture(args, temp):
    source = args.upstream.resolve()
    pinned_reference(source)
    reference = [args.python, "-m", "code_review_graph"]
    ref_daemon = [args.python, "-c", "from code_review_graph.daemon_cli import main; main()"]
    candidate = [str(args.binary.resolve(strict=True))]
    path = args.daemon_binary or args.binary.with_name("crg-daemon")
    daemon = [str(path.resolve())] if path.is_file() else None
    for key, value in {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
                       "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z",
                       "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z"}.items():
        os.environ[key] = value
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "FASTMCP_", "PYTHON"))}
    env.update(NO_COLOR="1", COLUMNS="80", LC_ALL="C", TZ="UTC", CRG_SERIAL_PARSE="1",
               CRG_HOME=str(temp / "registry"),
               PYTHONPATH=str(source), PYTHONNOUSERSITE="1", PYTHONDONTWRITEBYTECODE="1",
               PYTHONHASHSEED="0")
    candidate_env = env.copy()
    del candidate_env["PYTHONPATH"]
    repo = temp / "fixture"
    setup(repo)
    ref_version = run(reference + ["--version"], repo, env)
    if ref_version != {"exit": 0, "stdout": "code-review-graph 2.4.0\n", "stderr": ""}:
        raise RuntimeError(f"reference needs a Python environment with distribution version 2.4.0: {ref_version}")
    preflight = run([args.python, "-c", "import code_review_graph as c,importlib.metadata as m,json;"
                     "print(json.dumps([c.__file__,*[m.version(x) for x in ('code-review-graph','mcp','fastmcp')]]))"],
                    repo, env)
    info = json.loads(preflight["stdout"])
    if Path(info[0]).resolve() != source / "code_review_graph" / "__init__.py":
        raise ValueError(f"imported the wrong reference source: {info[0]}")
    result = Results()
    result.inventory["reference"] = {"revision": UPSTREAM_REV, "module": info[0], "distribution": info[1],
                                     "mcp": info[2], "fastmcp": info[3], "protocol": "2025-11-25"}
    tree = reference_cli_tree(args.python, repo, env)
    pristine = temp / "pristine"
    shutil.copytree(repo, pristine)
    def reset(snapshot):
        # repo and snapshots are fixed children of this function's private TemporaryDirectory.
        shutil.rmtree(repo)
        shutil.copytree(snapshot, repo)
    a = lifecycle(reference, repo, env)
    reset(pristine)
    b = lifecycle(candidate, repo, candidate_env)
    compare_group(result, "native-lifecycle", a, b, cli=True)

    reset(pristine)
    built = run(reference + ["build", "--repo", str(repo), "--skip-postprocess"], repo, env)
    if built["exit"] or not (repo / ".code-review-graph/graph.db").is_file():
        raise RuntimeError(f"unable to create interoperable reference DB: {built}")
    # Upstream clamps the relative age to zero for a future timestamp. Give both
    # binaries the exact same deterministic input instead of deleting time fields
    # from their responses. The separate native lifecycle uses real write times.
    with sqlite3.connect(repo / ".code-review-graph/graph.db") as connection:
        changed = connection.execute("UPDATE metadata SET value=? WHERE key='last_updated'",
                                     ("2099-01-01T00:00:00+00:00",))
        if changed.rowcount != 1:
            raise RuntimeError("reference graph has no last_updated metadata")
    connection.close()
    saved = temp / "pinned-database"
    shutil.copytree(repo, saved)
    seeded_exact = saved_graph(reference, repo, env, args.cutover)
    wire, inventory = inspect(reference, repo, env, extended=args.cutover)
    reset(saved)
    adopted = adopt_graph(reference, repo, env)
    if adopted["rename"]["exit"] or len(json.loads(adopted["callers"]["stdout"])["results"]) != 2:
        raise RuntimeError("Python failed to update its own saved graph")
    reset(saved)
    compare_group(result, "database-upgrade", adopted, adopt_graph(candidate, repo, candidate_env), cli=True)
    reference_witnesses(a, seeded_exact, wire, inventory)
    reset(saved)
    compare_group(result, "python-database", seeded_exact,
                  saved_graph(candidate, repo, candidate_env, args.cutover), cli=True)
    reset(saved)
    try:
        candidate_wire, candidate_inventory = inspect(candidate, repo, candidate_env, inventory["prompts"],
                                                      extended=args.cutover)
    except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.TimeoutExpired) as error:
        candidate_wire = {"transport-failure": repr(error)}
        candidate_inventory = {}
    compare_group(result, "MCP", wire, candidate_wire)
    for category in ("tools", "prompts"):
        compare_group(result, "schema/" + category, inventory[category], candidate_inventory.get(category, {}))
    if args.cutover:
        result.inventory["cli"] = cli_inventory(result, reference, candidate, ref_daemon, daemon, tree,
                                                repo, env, candidate_env)
    else:
        result.inventory["reference_cli_options"] = tree
    result.inventory["mcp"] = {"reference": inventory, "candidate": candidate_inventory}
    cases = QUERY_CASES | REVIEW_CASES if args.cutover else QUERY_CASES
    untested_tools = sorted(set(inventory["tools"]) - {name for name, _ in cases.values()})
    tested_commands = {"build", "query", "update", "status", "search", "serve"}
    if args.cutover:
        tested_commands.update(argv[0] for argv in REVIEW_COMMANDS.values())
    daemon_paths = {p.removeprefix("daemon ") for p in tree if p.startswith("daemon ")}
    covered_daemon = set()
    if args.cutover:
        from daemon_parity import compare as compare_daemon
        coverage = compare_daemon(result, source, args.python, candidate[0], daemon[0] if daemon else None)
        covered_daemon = coverage["crg-daemon"]
        primary = coverage["code-review-graph daemon"]
        tested_commands.update("daemon " + subcommand for subcommand in primary)
        if primary == daemon_paths:
            tested_commands.add("daemon")
    untested_cli = sorted(set(tree) - tested_commands - {""})
    result.inventory["behavior_without_fixtures"] = {
        "code-review-graph": untested_cli,
        "crg-daemon": sorted(daemon_paths - covered_daemon),
        "tools": untested_tools,
    }
    if args.cutover:
        for group, pending in result.inventory["behavior_without_fixtures"].items():
            result.check("untested/" + group, [], pending)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True, help=f"external source checkout or archive at {UPSTREAM_REV}")
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--daemon-binary", type=Path, help="defaults to crg-daemon next to --binary")
    parser.add_argument("--python", default=sys.executable, help="interpreter with upstream 2.4.0 distribution and dependencies")
    parser.add_argument("--report", type=Path, help="save inventory, exact outputs and per-check comparison as JSON")
    parser.add_argument("--cutover", action="store_true", help="also compare all CLI help pages/flags and fail on uncovered operations")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="crg-public-parity-") as directory:
        result = capture(args, Path(directory).resolve())
    if args.report:
        args.report.write_text(json.dumps({"inventory": result.inventory, "checks": result.checks},
                                          indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    groups = {}
    for name, case in result.checks.items():
        group = name.split("/")[0]
        groups.setdefault(group, []).append((name, case["match"]))
    for group, rows in groups.items():
        failed = [n for n, ok in rows if not ok]
        print(f"{group}: {len(rows)-len(failed)}/{len(rows)} exact matches")
        if failed:
            print("  mismatch: " + ", ".join(failed[:4]) + (" ..." if len(failed) > 4 else ""))
    mismatches = sum(not check["match"] for name, check in result.checks.items() if not name.startswith("untested/"))
    untested = result.inventory["behavior_without_fixtures"]
    print(f"{mismatches} observable differences. Behavioral fixtures still needed for "
          f"{len(untested['code-review-graph'])} primary CLI paths, {len(untested['crg-daemon'])} daemon paths, "
          f"and {len(untested['tools'])} MCP tools.")
    if mismatches:
        return 1
    return 2 if args.cutover and any(untested.values()) else 0


if __name__ == "__main__":
    raise SystemExit(main())
