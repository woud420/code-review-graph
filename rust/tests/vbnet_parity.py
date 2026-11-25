"""VB.NET regression sources from the pinned language-reconciliation suite."""

import ast
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    tree = ast.parse((upstream / "tests/test_language_reconciliation.py").read_text())
    block = next(
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == "TestVBNetReconciliation"
    )
    result = {}
    for case in block.body:
        strings = [
            node.value
            for node in ast.walk(case)
            if isinstance(node, ast.Constant) and isinstance(node.value, str)
        ]
        path = next(s for s in strings if s.endswith(".vb"))
        result[path] = next(s for s in strings if s.startswith("\n")).lstrip()
    result["Construction.vb"] = (
        "Imports AliasName = Global.System.Text, System.IO\n"
        "Public Class Construct\n Private Function Spawn() As Construct\n"
        "    Dim item = New Construct() ' Ignore()\n"
        '    Console.WriteLine("fake()")\n    Return item\n End Function\nEnd Class\n'
    )
    result["tests/Scoped.vb"] = (
        "Namespace Parent.Space\n Public Class Scope\n  Public Sub Exercise()\n"
        "   Resolve(1)\n  End Sub\n  Public Sub TestResolution()\n   Exercise()\n"
        "  End Sub\n End Class\nEnd Namespace\n"
    )
    return result


def assertions(db, root):
    with sqlite3.connect(db) as conn:
        assert (
            conn.execute(
                "select count(*) from nodes where kind='File' and language='vbnet'"
            ).fetchone()[0]
            == 5
        )
        owner = str(root / "Repository.vb") + "::Acme.Repository"
        for kind, source, target in [
            ("CALLS", owner + ".Save", owner + ".Helper"),
            ("INHERITS", owner, str(root / "Repository.vb") + "::Acme.BaseRepository"),
        ]:
            assert conn.execute(
                "select 1 from edges where kind=? and source_qualified=? and target_qualified=?",
                (kind, source, target),
            ).fetchone()
        assert conn.execute(
            "select json_array_length(extra,'$.vbnet_overloads') from nodes "
            "where name='Save' and file_path=?",
            (str(root / "Overloads.vb"),),
        ).fetchone() == (2,)


def update(root, files):
    (root / "Repository.vb").write_text(
        files["Repository.vb"].replace("Helper", "CheckValue").replace("hElPeR", "cHeCkVaLuE")
    )
    (root / "Services.vb").unlink()
    git(root, "commit", "-qam", "rename case-insensitive method and remove service")


def proof(callers, command):
    helper = "Helper" if command == "build" else "CheckValue"
    assert callers("Repository.vb::Acme.Repository." + helper) == {
        "Acme.Repository.Current",
        "Acme.Repository.Save",
    }


if __name__ == "__main__":
    sys.exit(compare("vbnet", corpus, assertions, update, proof))
