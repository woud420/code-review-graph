"""Compare SQL routines, dbt model membership, YAML aliases and Spring config across updates."""
import argparse
from pathlib import Path
import tempfile

from incremental_snapshot import compare_builds
from differential import git, new_repo, verify_source

PACKAGE = """create or replace package body accounting is
    function fee(n number) return number;
    function fee(n number) return number is
    begin
        audit_log(n);
        return n;
    end;
    function fee(s varchar2) return number is
    begin
        note(q'{END fee; fake_call() FROM ghost}');
        return amount(s);
    end fee;
    procedure settle is
    begin
        accounting.fee(1);
        INSERT INTO payments(id) VALUES(1);
    end settle;
end accounting;
/
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    upstream = args.upstream.resolve()
    verify_source(upstream)
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="crg-sql-") as temp:
        scratch = Path(temp).resolve()
        root = scratch / "src"
        new_repo(root, {
            "schema.sql": "create table payments(id INT);\ncreate view owed as select * from payments;\n",
            "billing.pkb": PACKAGE,
            "models/charges.sql": "select * from {{ ref('amounts') }} join {{ source('ledger', 'payments') }}\n",
            "warehouse/amounts.sql": "select * from src.invoices\n",
            "report.sql": "select * from {{ ref('other') }}\n",
            "vars/feature.yaml": "profiles:\n - ready: true\n   tag: example\n - ready: maybe\n   tag: example\n"
                                 "---\nstate: &base\n  priority: 2\nduplicate: a\nduplicate: b\ncopy: *base\n",
            "locales/en.yml": "greeting: sentinel-not-for-the-index\n",
            "src/application-prod.yml": "server:\n  http-timeout: 30\n  ports: [80, 443]\n",
            "src/application.properties": "API_TIMEOUT = 15\nserver.http-port = 8080\n",
            "src/ignored.properties": "other.configuration=true\n",
            "broken.yml": "unknown: *missing\n",
            ".gitignore": "dbt_project.yml\n",
        })
        # Config remains parser input even if the code graph does not track it.
        config = root / "dbt_project.yml"
        config.write_text("name: billing\nmodel-paths: [models]\n")
        for step in ("initial", "updated", "project-only", "unchanged"):
            if step == "updated":
                (root / "billing.pkb").write_text(PACKAGE.replace("fee", "discount"))
                (root / "schema.sql").unlink()
                (root / "vars/feature.yaml").write_text("mode: &opt {ready: true}\ncopy: *opt\n")
                (root / "src/application.properties").write_text("API_TIMEOUT = 30\nserver.port = 8081\n")
                (root / "src/application-prod.yml").unlink()
                (root / "broken.yml").write_text("enabled: true\n")
                config.write_text("name: billing\nmodel-paths:\n  - warehouse\n")
                git(root, "add", "-A")
                git(root, "commit", "-qm", "rename routine and change model directories")
            if step == "project-only":
                config.write_text("name: billing\nmodel-paths: [models]\n")
            native = compare_builds(scratch, root, upstream, binary, step, update=step != "initial")
            assert not any("sentinel-not-for-the-index" in row for rows in native.values() for row in rows)
            print(f"{step}: {native['symbols'].total()} nodes, {native['edges'].total()} edges, all nine groups matched")


if __name__ == "__main__":
    main()
