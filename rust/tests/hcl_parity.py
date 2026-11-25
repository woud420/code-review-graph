"""Compare Terraform directory scoping and local module-source rebinding across updates."""
import argparse
from pathlib import Path
import tempfile

from differential import git, new_repo, verify_source
from incremental_snapshot import compare_builds


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-hcl-") as temp:
        scratch = Path(temp).resolve()
        root = scratch / "source"
        new_repo(root, {
            "root/outputs.tf": 'output "region" { value = var.region }\n'
                               'module "sub" { source = "./modules/net" }\n'
                               'resource "instance" "app" { provider = aws.east }\n',
            "root/provider.tf": 'provider "aws" { alias = "east" }\n',
            "root/modules/net/network.tf": 'variable "region" {}\n',
            "another/example.hcl": 'job "irrelevant" { datacenters = ["dc1"] }\n',
        })
        for step in ("initial", "add", "ambiguous", "delete", "unchanged"):
            if step == "add":
                (root / "root/vars.tf").write_text('variable "region" {}\n')
                (root / "root/modules/net/main.tf").write_text('variable "subnet" {}\n')
            elif step == "ambiguous":
                (root / "root/other.tf").write_text('variable "region" {}\n')
            elif step == "delete":
                (root / "root/modules/net/main.tf").unlink()
                (root / "root/vars.tf").unlink()
                (root / "root/provider.tf").write_text('provider "aws" { alias = "west" }\n')
            if step not in ("initial", "unchanged"):
                git(root, "add", "-A")
                git(root, "commit", "-qm", step)
            graph = compare_builds(scratch, root, upstream, binary, step, update=step != "initial")
            print(f"{step}: {graph['symbols'].total()} nodes, {graph['edges'].total()} edges, all nine groups matched")


if __name__ == "__main__":
    main()
