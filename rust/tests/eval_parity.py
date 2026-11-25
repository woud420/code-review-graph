"""Compare the public eval report's console tables and Markdown bytes to the pinned Python CLI."""
import argparse
import csv
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from differential import verify_source


def write(directory, name, rows):
    directory.mkdir(exist_ok=True, parents=True)
    with (directory / name).open("w", encoding="utf-8", newline="") as target:
        csv.writer(target).writerows(rows)


def compare(scratch, label, upstream, binary, arguments, *, success=True, defaults=None):
    outcomes = []
    for name, command in (("python", [sys.executable, "-m", "code_review_graph"]), ("rust", [str(binary)])):
        cwd = scratch / label / name
        cwd.mkdir(parents=True)
        if defaults is not None:
            shutil.copytree(defaults, cwd / "evaluate/results")
        env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "FASTMCP_"))}
        env.update(CRG_HOME=str(cwd / "state"), PYTHONDONTWRITEBYTECODE="1")
        if name == "python":
            env["PYTHONPATH"] = str(upstream)
        result = subprocess.run([*command, "eval", *arguments], cwd=cwd, env=env,
                                capture_output=True, timeout=30)
        file = cwd / "evaluate/reports/summary.md"
        assert (result.returncode == 0) == success, (name, result.stderr.decode(errors="replace"))
        if not success:
            assert not file.exists() and result.stderr, (name, label)
        outcomes.append((result.returncode, result.stdout, file.read_bytes() if file.exists() else None))
    assert outcomes[0] == outcomes[1], f"{label}: report output differs"
    return outcomes[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-eval-") as temp:
        scratch = Path(temp).resolve()
        actual = upstream / "evaluate/results"
        count = len(list(actual.glob("*.csv")))
        assert count == 24
        _, console, report = compare(scratch, "published", upstream, binary, ["--report"], defaults=actual)
        print(f"published: {count} real CSVs, {len(console)} console bytes, {len(report)} Markdown bytes "
              f"(sha256 {hashlib.sha256(report).hexdigest()[:16]})")
        compare(scratch, "no-command", upstream, binary, [])
        unknown = scratch / "no-results"
        compare(scratch, "no-results", upstream, binary, ["--report", "--output-dir", str(unknown)])
        data = scratch / "results"
        write(data, "org_repo_token_efficiency_2025-01-01.csv", [["repo", "changed_files"], ["old-sentinel", "9"]])
        write(data, "org_repo_token_efficiency_2026-01-01.csv", [["repo", "changed_files"], ["org_repo", "2"]])
        write(data, "z_token_efficiency_2026-03-01.csv", [["repo", "graph_tokens"], ["z", "4"]])
        write(data, "r_impact_accuracy_z.csv", [["repo", "status", "ground_truth_mode", "f1"],
              ["r", "ok", "graph-derived", "0.125"], ["r", "", "graph-derived calibrated", "0.875"],
              ["r", "error", "graph-derived", "99"], ["r", "ok", "co-change", "99"], ["r", "", "", "wrong"]])
        write(data, "r_flow_completeness_z.csv", [["repo", "recall"], ["r", "0.9"]])
        write(data, "r_search_quality_z.csv", [["repo", "reciprocal_rank"], ["r", "bad"],
              ["r", "1.0"], ["r", "0.0"], ["quoted, name", "not a number"]])
        write(data, "r_build_performance_z.csv", [["repo", "node_count"], ["r", "8"]])
        write(data, "r_incremental_fidelity_z.csv", [["repo", "edit_kind", "worst_table"], ["r", "rename", ""]])
        write(data, "r_agent_baseline_z.csv", [["repo", "status", "question"], ["r", "", 'line 1\nline 2, "why"?']])
        write(data, "unrelated.csv", [["ignored"]])
        _, console, report = compare(scratch, "aggregates", upstream, binary, ["--report", "--output-dir", str(data)])
        assert b"old-sentinel" not in console + report and b"| r | 0.5 | 0.9 | 0.5 |" in console
        write(data, "x_token_efficiency_z.csv", [["repo", "changed_files"], ["x"]])
        compare(scratch, "short-record", upstream, binary, ["--report", "--output-dir", str(data)], success=False)
        print("no arguments, no results, stale files, mixed schemas, malformed metrics and short CSV records matched")


if __name__ == "__main__":
    main()
