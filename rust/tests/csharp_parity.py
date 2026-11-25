"""Exercise pinned C# names, inheritance, namespace bindings, and project-scoped global imports."""

import ast
from hashlib import sha256
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(source):
    sample = (source / "tests/fixtures/Sample.cs").read_bytes()
    assert (
        sha256(sample).hexdigest()
        == "f81d6a2f49408a216e777c14d71ce875610fdf0ea8b6b7c0f600877fee58c1ce"
    )
    files = {"Sample.cs": sample.decode()}
    cases = {
        "same_leaf_nested_types_keep_distinct_identities_and_bind_own_members": "nested",
        "type_kinds_top_level_statements_and_members_bind_through_usings": "types",
        "unicode_identifiers_and_paths_with_spaces_bind": "unicode",
        "nearest_csproj_owns_global_usings": "projects",
        "type_in_a_parent_namespace_binds_without_a_using": "parents",
    }
    suite = ast.parse((source / "tests/test_csharp_namespace_binding_edges.py").read_text())
    found = set()
    for fn in suite.body:
        if not isinstance(fn, ast.FunctionDef) or fn.name.removeprefix("test_") not in cases:
            continue
        case = fn.name.removeprefix("test_")
        write = next(
            n
            for n in ast.walk(fn)
            if isinstance(n, ast.Call) and isinstance(n.func, ast.Name) and n.func.id == "_write"
        )
        for name, content in ast.literal_eval(write.args[1]).items():
            files[cases[case] + "/" + name] = content
        found.add(case)
    assert found == cases.keys()
    return files


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as conn:

        def exits(source, target):
            e = conn.execute(
                "SELECT extra FROM edges WHERE kind='CALLS' AND source_qualified=? AND target_qualified=?",
                (str(root / source), str(root / target)),
            ).fetchone()
            assert e and "scoped_resolved" in e[0], (source, target)

        exits(
            "Sample.cs::SampleApp.UserService.GetUser", "Sample.cs::SampleApp.IRepository.FindById"
        )
        exits("nested/Both.cs::Beta.Same.M", "nested/Both.cs::Beta.Same.N")
        exits("types/Program.cs", "types/Types.cs::A.S.Run")
        exits("projects/RootCaller.cs::D.Go", "projects/Other.cs::Other.Service.Run")
        exits("unicode/My Proj é/Caller.cs::C.Go", "unicode/My Proj é/Café.cs::Café.Ünï.Rün")
        assert conn.execute(
            "SELECT target_qualified FROM edges WHERE kind='CALLS' AND source_qualified=?",
            (str(root / "projects/Tests/Caller.cs::C.Go"),),
        ).fetchone() == ("Service::Run",)


def update(root, files):
    (root / "types/Program.cs").unlink()
    (root / "projects/Imports.cs").write_text("global using Missing;")
    (root / "Sample.cs").write_text(files["Sample.cs"].replace("GetUser(", "ReadUser("))
    git(root, "commit", "-qam", "rename C# method and delete a top-level caller")


def check_proof(callers, command):
    name = "SampleApp.UserService." + ("GetUser" if command == "build" else "ReadUser")
    assert callers("Sample.cs::SampleApp.IRepository.FindById") == {name}
    assert callers("nested/Both.cs::Alpha.Same.N") == {"Alpha.Same.M"}


if __name__ == "__main__":
    sys.exit(compare("csharp", corpus, assertions, update, check_proof))
