"""Compare Java persisted graph rows after build/update against the pinned Python CLI."""

import ast
from hashlib import sha256
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    fixtures = upstream / "tests/fixtures"
    files = {p.name: p.read_text() for p in sorted(fixtures.glob("*.java"))}
    digest = sha256()
    for name, source in files.items():
        digest.update(name.encode() + b"\0" + sha256(source.encode()).digest())
    assert digest.hexdigest() == "22f3898ba2afbf8d73c05784831ff35feb371ad49e11a3e311a32b18e1590abe"
    tests = ast.parse((upstream / "tests/test_java_name_extraction.py").read_text())
    for fn in ast.walk(tests):
        if isinstance(fn, ast.FunctionDef):
            for call in ast.walk(fn):
                if (
                    isinstance(call, ast.Call)
                    and isinstance(call.func, ast.Name)
                    and call.func.id == "_java_funcs"
                ):
                    files["names/" + fn.name.removeprefix("test_") + ".java"] = ast.literal_eval(
                        call.args[0]
                    )
    tests = ast.parse((upstream / "tests/test_java_call_references.py").read_text())
    parse = next(
        n
        for n in ast.walk(tests)
        if isinstance(n, ast.Call)
        and isinstance(n.func, ast.Attribute)
        and n.func.attr == "parse_bytes"
    )
    files["RouterConfig.java"] = ast.literal_eval(parse.args[1]).decode()
    tests = ast.parse((upstream / "tests/test_spring_java_reconciliation.py").read_text())
    for fn in tests.body:
        if isinstance(fn, ast.FunctionDef) and fn.name.startswith("test_"):
            parse = next(
                n
                for n in ast.walk(fn)
                if isinstance(n, ast.Call)
                and isinstance(n.func, ast.Name)
                and n.func.id == "_injected_fields"
            )
            files["injections/" + fn.name.removeprefix("test_") + ".java"] = ast.literal_eval(
                parse.args[0]
            )
    assert len(files) == 16, sorted(files)
    return files


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as conn:
        count = dict(conn.execute("SELECT kind,count(*) FROM edges GROUP BY kind"))
        assert count["CONSUMES"] == 4 and count["PRODUCES"] == 2 and count["INJECTS"] >= 12
        for filename, caller, target, evidence in [
            ("SampleJava.java", "UserService.createUser", "UserRepository.save", "typed_receiver"),
            (
                "SpringDI.java",
                "OrderService.placeOrder",
                "JpaOrderRepository.save",
                "spring_resolved",
            ),
            (
                "TemporalWorkflow.java",
                "OrderWorkflowImpl.processOrder",
                "PaymentActivityImpl.chargeCard",
                "temporal_resolved",
            ),
            ("RouterConfig.java", "RouterConfig.routes", "Handler.handle", "method_reference"),
        ]:
            prefix = str(root / filename) + "::"
            value = conn.execute(
                "SELECT extra FROM edges WHERE kind='CALLS' AND source_qualified=? AND target_qualified=?",
                (prefix + caller, prefix + target),
            ).fetchone()
            assert value and evidence in value[0], (filename, caller, target, value)


def update(root, files):
    (root / "KafkaPatterns.java").unlink()
    (root / "SampleJava.java").write_text(
        files["SampleJava.java"].replace("createUser", "createProfile")
    )
    git(root, "commit", "-qam", "rename Java method and remove Kafka consumers")


def check_proof(callers, command):
    name = "UserService.createUser" if command == "build" else "UserService.createProfile"
    assert name in callers("SampleJava.java::UserRepository.save")
    assert callers("RouterConfig.java::Handler.handle") == {"RouterConfig.routes"}


if __name__ == "__main__":
    sys.exit(compare("java", corpus, assertions, update, check_proof))
