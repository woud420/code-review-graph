"""Exercise Robot Framework fixtures from the pinned parser acceptance suite."""

import ast
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    tree = ast.parse((upstream / "tests/test_parser.py").read_text())
    result = {}
    for case in ast.walk(tree):
        if not isinstance(case, ast.FunctionDef) or "robot" not in case.name:
            continue
        if case.name in {
            "test_robot_every_assignment_form_keeps_the_keyword",
            "test_robot_section_header_spellings",
        }:
            template = next(n for n in ast.walk(case) if isinstance(n, ast.JoinedStr))
            names, variants = map(ast.literal_eval, case.decorator_list[0].args)
            names = (names,) if isinstance(names, str) else names
            sources = []
            for variant in variants:
                values = dict(
                    zip(names, (variant,) if isinstance(variant, str) else variant, strict=True)
                )
                sources.append(
                    "".join(
                        n.value if isinstance(n, ast.Constant) else values[n.value.id]
                        for n in template.values
                    )
                )
        else:
            sources = [
                node.value
                for node in ast.walk(case)
                if isinstance(node, ast.Constant)
                and isinstance(node.value, (str, bytes))
                and "***" in str(node.value)
                and len(node.value.splitlines()) > 1
            ]
        for index, source in enumerate(sources):
            if isinstance(source, bytes):
                source = source.decode()
            if case.name == "test_parse_robot_and_resource_files":
                path = "sample.robot" if index == 0 else "keywords.resource"
            else:
                path = f"{case.name.removeprefix('test_robot_')}-{index}.robot"
            result[path] = source
    return result


def assertions(db, root):
    with sqlite3.connect(db) as conn:
        assert (
            conn.execute(
                "select count(*) from nodes where language='robot' and kind='File'"
            ).fetchone()[0]
            == 27
        )
        path = str(root / "sample.robot")
        for kind, source, target in [
            ("IMPORTS_FROM", path, str(root / "keywords.resource")),
            ("TESTED_BY", path + "::Prepare environment", path + "::A passing test"),
        ]:
            assert conn.execute(
                "select 1 from edges where kind=? and source_qualified=? and target_qualified=?",
                (kind, source, target),
            ).fetchone()
        assert conn.execute(
            "select count(distinct kind) from nodes where name='Shared'"
        ).fetchone() == (2,)


def update(root, files):
    (root / "sample.robot").write_text(
        files["sample.robot"].replace("Prepare environment", "Initialize environment")
    )
    (root / "keywords.resource").unlink()
    git(root, "commit", "-qam", "rename a keyword and remove its resource file")


def proof(callers, command):
    name = "Prepare environment" if command == "build" else "Initialize environment"
    assert callers("sample.robot::" + name) == {"A passing test"}


if __name__ == "__main__":
    sys.exit(compare("robot", corpus, assertions, update, proof))
