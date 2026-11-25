"""Compare Lua and Luau require forms, table methods, assigned functions and test edges."""

from hashlib import sha256
import json
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    fixtures = upstream / "tests/fixtures"
    hashes = {
        "sample.lua": "51c29521de0ffc7c288739bf65f9f14c411af3cec9df91d83e9ef18651d6dfc8",
        "sample.luau": "6618eb68fe06399b930d70c71773ec9872bb9d91db64b61439de7e6df02a919c",
    }
    files = {}
    for name, expected in hashes.items():
        raw = (fixtures / name).read_bytes()
        assert sha256(raw).hexdigest() == expected, name
        files[name] = raw.decode()
    files["features/runner.lua"] = (
        "local apply = function(x) return x end\n"
        "function run(x) return apply(x) end\n"
        "function verify() return run(3) end\n"
    )
    return files


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as conn:
        for ext in ("lua", "luau"):
            path = str(root / ("sample." + ext))

            def exists(kind, owner, to):
                return conn.execute(
                    "SELECT extra FROM edges WHERE kind=? AND source_qualified=? AND target_qualified=?",
                    (kind, owner, to),
                ).fetchone()

            assert exists("IMPORTS_FROM", path, "lib.utils")
            assert exists("CALLS", path + "::test_greet", path + "::greet")
            assert exists("TESTED_BY", path + "::greet", path + "::test_greet")
            assert exists("CONTAINS", path + "::Animal", path + "::Animal.speak")
            row = exists("CALLS", path + "::test_animal_speak", "speak")
            assert row and json.loads(row[0])["claimed_endpoint"] == path + "::speak"
        assert conn.execute(
            "SELECT kind FROM nodes WHERE qualified_name=?",
            (str(root / "sample.luau") + "::Vector3",),
        ).fetchone() == ("Class",)


def update(root, files):
    (root / "features/runner.lua").unlink()
    (root / "sample.lua").write_text(files["sample.lua"].replace("greet(", "welcome("))
    git(root, "commit", "-qam", "rename Lua helper and remove assigned-function module")


def proof(callers, command):
    name = "greet" if command == "build" else "welcome"
    assert callers("sample.lua::" + name) == {"test_" + name}
    assert callers("sample.luau::greet") == {"test_greet"}


if __name__ == "__main__":
    sys.exit(compare("lua", corpus, assertions, update, proof))
