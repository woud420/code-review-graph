"""Compare public graph output for the pinned Rust, TypeScript/TSX, shell, SQL, YAML, Terraform and Ruby fixtures."""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from canonical_parity import read_graph
from differential import new_repo, verify_source


EXTENSIONS = {".rs", ".js", ".jsx", ".ts", ".tsx", ".sh", ".bash", ".zsh", ".ksh", ".sql", ".yml", ".yaml", ".tf", ".hcl", ".rb"}
FIXTURES_SHA256 = "01f97cbd3ef165b91124fb2feffc0f95af81a0703fafb13802ea87ed00fdcd4a"


def compare(directory, upstream, binary):
    verify_source(upstream)
    paths = sorted(p for p in (upstream / "tests/fixtures").rglob("*")
                   if p.is_file() and (p.suffix in EXTENSIONS or p.name == "tsconfig.json"))
    digest = hashlib.sha256()
    for path in paths:
        digest.update(path.relative_to(upstream).as_posix().encode() + b"\0"
                      + hashlib.sha256(path.read_bytes()).digest())
    if digest.hexdigest() != FIXTURES_SHA256:
        raise ValueError("fixture files differ from the pinned revision")
    root = directory / "source"
    if not root.exists():
        new_repo(root, {p.relative_to(upstream).as_posix(): p.read_text(encoding="utf-8") for p in paths})
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON", "FASTMCP_"))}
    env.update(CRG_HOME=str(directory / "registry"), CRG_SERIAL_PARSE="1", PYTHONHASHSEED="0",
               PYTHONDONTWRITEBYTECODE="1")
    results = []
    for label, command in [("python", [sys.executable, "-m", "code_review_graph"]), ("rust", [str(binary)])]:
        data = directory / label
        options = dict(env, CRG_DATA_DIR=str(data))
        if label == "python":
            options["PYTHONPATH"] = str(upstream)
        subprocess.run([*command, "build", "--quiet", "--repo", str(root)], cwd=root, env=options, check=True)
        results.append(read_graph(data / "graph.db", root))
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="crg-source-fixtures-") as temp:
        reference, native = compare(Path(temp).resolve(), args.upstream.resolve(), args.binary.resolve())
    failed = False
    for group, expected in reference.items():
        missing, extra = expected - native[group], native[group] - expected
        print(f"{group}: {expected.total()} reference, {missing.total()} missing, {extra.total()} extra")
        if missing or extra:
            failed = True
            print(f"  first missing: {next(iter(sorted(missing)), None)}")
            print(f"  first extra: {next(iter(sorted(extra)), None)}")
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
