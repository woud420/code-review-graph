"""Compare native PowerShell extraction against the pinned grammar regression corpus."""

import ast
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    module = ast.parse((upstream / "tests/test_powershell_extraction.py").read_text())
    sample = next(
        n.value.value.decode()
        for n in module.body
        if isinstance(n, ast.Assign) and n.targets[0].id == "SOURCE"
    )
    literals = {
        n.name: ast.literal_eval(n.decorator_list[0].args[1])
        for n in module.body
        if isinstance(n, ast.FunctionDef)
        and n.name
        in {
            "test_numeric_ranges_are_not_command_calls",
            "test_range_like_command_names_remain_calls",
        }
    }
    ranges = [v.decode() for v in literals["test_numeric_ranges_are_not_command_calls"]]
    commands = literals["test_range_like_command_names_remain_calls"]
    return {
        "app.ps1": sample,
        "module.psm1": "function script:Invoke-Data { Get-State }\nInvoke-Data\n",
        "cases.psd1": "\n".join(ranges + commands) + "\n",
        "tests/health.psm1": "function Test-Resource { Get-State }\n",
        "Health.Tests.ps1": "class R { [bool] Test() { return Get-State } }\n",
    }


def assertions(db, root):
    with sqlite3.connect(db) as conn:
        assert (
            conn.execute(
                "select count(*) from nodes where language='powershell' and kind='File'"
            ).fetchone()[0]
            == 5
        )
        assert conn.execute(
            "select kind,is_test from nodes where qualified_name=?",
            (str(root / "app.ps1") + "::Test-Health",),
        ).fetchone() == ("Function", 0)
        assert conn.execute(
            "select kind,is_test from nodes where qualified_name=?",
            (str(root / "Health.Tests.ps1") + "::R.Test",),
        ).fetchone() == ("Test", 1)
        calls = {
            r[0] for r in conn.execute("select target_qualified from edges where kind='CALLS'")
        }
        assert {"1..3.ps1", "build..release", "1gbus..2gbus", "1.5ul..3ul", "0b1d..0b11d"} <= calls
        assert "1..3" not in calls


def edit(root, files):
    (root / "app.ps1").write_text(files["app.ps1"].replace("Convert-Thing", "Convert-Item"))
    (root / "module.psm1").unlink()
    git(root, "commit", "-qam", "rename the converter and delete a module")


def proof(callers, command):
    name = "Convert-Thing" if command == "build" else "Convert-Item"
    assert callers("app.ps1::" + name) == {"Get-Thing"}


if __name__ == "__main__":
    sys.exit(compare("powershell", corpus, assertions, edit, proof))
