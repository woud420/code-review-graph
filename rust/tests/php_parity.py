"""Compare native PHP indexing with the pinned upstream CLI on declarations, imports and scoped calls."""

import ast
from hashlib import sha256
import json
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    raw = (upstream / "tests/fixtures/sample.php").read_bytes()
    assert (
        sha256(raw).hexdigest()
        == "44d755db240fa157398c8f98cec417b3b957c29aebb0f08070e009f06250f4b3"
    )
    files = {"sample.php": raw.decode()}
    tests = ast.parse((upstream / "tests/test_multilang.py").read_text())
    suite = next(c for c in tests.body if getattr(c, "name", None) == "TestPHPParsing")
    case = next(
        f
        for f in suite.body
        if getattr(f, "name", None) == "test_finds_extended_php_types_bases_and_object_creation"
    )
    files["extended.php"] = ast.literal_eval(
        next(n.value for n in case.body if isinstance(n, ast.Assign))
    ).decode()
    suite = next(c for c in tests.body if getattr(c, "name", None) == "TestPHPTestAnnotations")
    case = next(f for f in suite.body if getattr(f, "name", None) == "_parse")
    write = next(
        n
        for n in ast.walk(case)
        if isinstance(n, ast.Call)
        and isinstance(n.func, ast.Attribute)
        and n.func.attr == "write_text"
    )
    files["tests/ExampleTest.php"] = ast.literal_eval(write.args[0])
    files["src/App/Domain/Job.php"] = (
        "<?php\nnamespace App\\Domain;\nclass Job { static function apply() {} function inspect() {} }\n"
    )
    files["src/App/Domain/Status.php"] = (
        "<?php\nnamespace App\\Domain;\nenum Status { case Ready; }\n"
    )
    files["src/App/Service/Runner.php"] = (
        "<?php\nnamespace App\\Service;\nuse App\\Domain\\{Job, Status};\n"
        "class Runner { function go() {\n Job::apply();\n $job = new Job();\n $job->inspect();\n} }\n"
    )
    composer = ast.parse((upstream / "tests/test_php_laravel.py").read_text())
    suite = next(n for n in composer.body if getattr(n, "name", None) == "TestComposerPsr4")
    imported = {
        "test_composer_resolves_standard_psr4_mapping",
        "test_composer_uses_longest_matching_prefix",
        "test_composer_checks_every_directory_for_prefix",
        "test_composer_merges_autoload_and_autoload_dev_directories",
    }
    for case in suite.body:
        if getattr(case, "name", None) not in imported:
            continue
        prefix = "composer/" + case.name.removeprefix("test_composer_") + "/"
        for call in ast.walk(case):
            if not isinstance(call, ast.Call) or not isinstance(call.func, ast.Name):
                continue
            if call.func.id == "_write_composer":
                files[prefix + "composer.json"] = json.dumps(ast.literal_eval(call.args[1]))
            elif call.func.id == "_write_php":
                files[prefix + ast.literal_eval(call.args[0].right)] = (
                    "<?php\nclass Placeholder {}\n"
                )
            elif call.func.id == "_php_import_target":
                path = ast.literal_eval(call.args[1].right)
                module = ast.literal_eval(call.args[2])
                files[prefix + path] = f"<?php\nuse {module};\nclass Caller {{}}\n"
    assert len([p for p in files if p.endswith("composer.json")]) == len(imported)
    return files


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as c:

        def target(file, caller, callee):
            prefix = str(root / file)
            row = c.execute(
                "SELECT extra FROM edges WHERE kind='CALLS' AND source_qualified=? AND target_qualified=?",
                (prefix + "::" + caller, callee),
            ).fetchone()
            assert row, (caller, callee)
            return json.loads(row[0])

        scoped = str(root / "sample.php") + "::ExtendedRepo.factory"
        assert (
            target("sample.php", "ExtendedRepo.runQueries", scoped)["scoped_resolution"]
            == "enclosing_class"
        )
        for case, caller, source in [
            ("resolves_standard_psr4_mapping", "app/Services/Report.php", "app/Models/User.php"),
            ("uses_longest_matching_prefix", "src/Caller.php", "domain/Thing.php"),
            ("checks_every_directory_for_prefix", "app/Caller.php", "src/Thing.php"),
            (
                "merges_autoload_and_autoload_dev_directories",
                "tests/Caller.php",
                "dev/Tests/Factory.php",
            ),
        ]:
            folder = root / "composer" / case
            assert c.execute(
                "SELECT target_qualified FROM edges WHERE kind='IMPORTS_FROM' AND file_path=?",
                (str(folder / caller),),
            ).fetchone() == (str(folder / source),)
        for method, proof in [("apply", "scoped_resolution"), ("inspect", "scoped_resolved")]:
            assert proof in target(
                "src/App/Service/Runner.php",
                "Runner.go",
                str(root / "src/App/Domain/Job.php") + "::Job." + method,
            )
        assert c.execute(
            "SELECT kind FROM nodes WHERE name='aliased_attribute_method'"
        ).fetchone() == ("Test",)
        assert c.execute(
            "SELECT kind FROM nodes WHERE name='unrelated_qualified_attribute'"
        ).fetchone() == ("Function",)


def update(root, files):
    (root / "src/App/Domain/Status.php").unlink()
    (root / "sample.php").write_text(files["sample.php"].replace("sqlQuery", "runSql"))
    git(root, "commit", "-qam", "rename PHP helper and remove grouped import")


def proof(callers, command):
    assert callers("sample.php::" + ("sqlQuery" if command == "build" else "runSql")) == {
        "ExtendedRepo.runQueries"
    }
    assert callers("src/App/Domain/Job.php::Job.apply") == {"Runner.go"}


if __name__ == "__main__":
    sys.exit(compare("php", corpus, assertions, update, proof))
