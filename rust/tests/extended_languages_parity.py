"""Differential raw, postprocessed and incremental graph comparisons on pinned fixtures."""

import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    directory = upstream / "tests/fixtures"
    names = [
        "sample_notebook.ipynb",
        "sample_databricks_notebook.ipynb",
        "sample_databricks_export.py",
        "sample_html.html",
        "sample_vue.vue",
        "sample.liquid",
        "sample.pl",
        "sample.gd",
        "sample.m",
        "sample.scala",
        "sample.ex",
        "sample.xs",
        "sample.dart",
        "sample.sv",
        "sample.jl",
        "sample.res",
        "sample.resi",
        "sample_xslt.xsl",
        "sample_xslt_common.xsl",
        "sample.kt",
        "sample.swift",
        "sample.R",
        "test_sample.R",
        "sample_zig.zig",
        "sample_zig_util.zig",
        "sample.nix",
        "sample_module.nix",
    ]
    files = {name: (directory / name).read_text() for name in names}
    files["flake.nix"] = (
        '{ inputs.example.url = "github:example/project";\n'
        '  inputs = { inner.url = "github:inner/project"; };\n'
        "  build = import ./sample_module.nix; }\n"
    )
    files["snippets/product-card.liquid"] = "{{ product.title }}\n"
    return files


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as conn:
        for kind, file, owner, target in [
            (
                "CALLS",
                "sample_notebook.ipynb",
                "::DataProcessor.process",
                "sample_notebook.ipynb::add",
            ),
            (
                "CALLS",
                "sample_databricks_export.py",
                "::process_events",
                "sample_databricks_export.py::load_config",
            ),
            ("CALLS", "sample_html.html", "", "sample_html.html::openSearch"),
            ("REFERENCES", "sample_vue.vue", "::onSelectUser", "sample_vue.vue::User"),
            ("IMPORTS_FROM", "sample.liquid", "", "snippets/product-card.liquid"),
            ("CONTAINS", "sample.liquid", "", "sample.liquid::output:item.title | escape"),
            ("CALLS", "sample.m", "::main", "sample.m::Calculator.add"),
            ("CALLS", "sample.gd", "::_ready", "sample.gd::_load_items"),
            ("CALLS", "sample.pl", "::bark", "sample.pl::speak"),
            ("CALLS", "sample.scala", "::UserService.createUser", "sample.scala::Repository.save"),
            ("CALLS", "sample.ex", "::Calculator.compute", "sample.ex::Calculator.add"),
            ("CALLS", "sample.dart", "", "sample.dart::Dog._run"),
            ("CALLS", "sample.sv", "::FIFOController", "sample.sv::Adder"),
            ("REFERENCES", "sample.sv", "::FIFOController", "sample.sv::FIFOController.wr_ptr"),
            ("CALLS", "sample.jl", "::SampleModule.outer", "sample.jl::SampleModule.outer.inner"),
            ("REFERENCES", "sample.jl", "::SampleModule", "sample.jl::SampleModule.greet"),
            ("CALLS", "sample.res", "::helper", "sample.res::fact"),
            ("CONTAINS", "sample.resi", "::User", "sample.resi::User.greet"),
            ("CALLS", "sample_xslt.xsl", "::match=/", "sample_xslt.xsl::page.header"),
            ("IMPORTS_FROM", "sample_xslt.xsl", "", "sample_xslt_common.xsl"),
            ("CALLS", "sample.swift", "::createUser", "sample.swift::InMemoryRepo.save"),
            ("CALLS", "sample.kt", "::createUser", "sample.kt::UserRepository.save"),
            ("CALLS", "sample.R", "::process_data", "sample.R::add"),
            ("CONTAINS", "sample.R", "::MyClass", "sample.R::MyClass.greet"),
            (
                "TESTED_BY",
                "sample_zig.zig",
                "::helper",
                "sample_zig.zig::test:helper increments@L36",
            ),
            ("IMPORTS_FROM", "sample_zig.zig", "", "sample_zig_util.zig"),
            ("CONTAINS", "sample.nix", "", "sample.nix::packages.default"),
            ("IMPORTS_FROM", "flake.nix", "", "sample_module.nix"),
        ]:
            row = conn.execute(
                "SELECT 1 FROM edges WHERE kind=? AND source_qualified=? AND target_qualified=?",
                (kind, str(root / file) + owner, str(root / target)),
            ).fetchone()
            assert row, (kind, file, owner, target)
        for url in ["github:example/project", "github:inner/project"]:
            assert conn.execute(
                "SELECT 1 FROM edges WHERE kind='IMPORTS_FROM' AND target_qualified=?", (url,)
            ).fetchone()


def update(root, files):
    (root / "sample_notebook.ipynb").write_text(
        files["sample_notebook.ipynb"].replace("multiply", "times")
    )
    (root / "snippets/product-card.liquid").unlink()
    (root / "sample.liquid").write_text(files["sample.liquid"].replace("heading", "headline"))
    (root / "sample_html.html").write_text(
        files["sample_html.html"].replace("renderTabs", "renderActive")
    )
    (root / "sample_vue.vue").write_text(
        files["sample_vue.vue"].replace("onSelectUser", "onPickUser")
    )
    (root / "sample.gd").write_text(files["sample.gd"].replace("_load_items", "_load_all"))
    (root / "sample.scala").write_text(files["sample.scala"].replace("getUser", "loadUser"))
    (root / "sample.ex").write_text(files["sample.ex"].replace("triple", "three_times"))
    (root / "sample.sv").write_text(files["sample.sv"].replace("Adder", "SlowAdder"))
    (root / "sample.jl").write_text(files["sample.jl"].replace("inner", "nested"))
    (root / "sample.res").write_text(files["sample.res"].replace("fact", "factorial"))
    (root / "sample_xslt.xsl").write_text(
        files["sample_xslt.xsl"].replace("page.header", "page.head")
    )
    for name in ("sample.kt", "sample.swift"):
        (root / name).write_text(files[name].replace("createUser", "makeUser"))
    (root / "sample_zig_util.zig").unlink()
    (root / "sample.R").write_text(files["sample.R"].replace("add", "add_values"))
    (root / "sample.nix").write_text(
        files["sample.nix"].replace("packages.default", "packages.build")
    )
    git(root, "commit", "-qam", "rename R and Nix definitions and remove the Zig import target")


def proof(callers, command):
    operator = "multiply" if command == "build" else "times"
    assert callers("sample_notebook.ipynb::" + operator) == {"DataProcessor.process"}
    assert callers("sample.dart::Dog._run") == {"<module>"}
    assert callers("sample.ex::MathHelpers.double") == (
        {"MathHelpers.triple"} if command == "build" else {"MathHelpers.three_times"}
    )
    assert callers("sample.scala::Repository.findById") == (
        {"UserService.getUser"} if command == "build" else {"UserService.loadUser"}
    )
    assert callers("sample.gd::" + ("_load_items" if command == "build" else "_load_all")) == {
        "_ready"
    }
    assert callers("sample.pl::speak") == {"bark"}
    assert callers("sample.m::Calculator.add") == {"main"}
    assert callers(
        "sample_html.html::" + ("renderTabs" if command == "build" else "renderActive")
    ) == {"openSearch", "Tracker.update"}
    adder = "Adder" if command == "build" else "SlowAdder"
    assert callers("sample.sv::" + adder) == {"FIFOController"}
    name = "inner" if command == "build" else "nested"
    assert callers("sample.jl::SampleModule.outer." + name) == {"SampleModule.outer"}
    fact = "fact" if command == "build" else "factorial"
    assert callers("sample.res::" + fact) == {fact, "main", "helper", "test_" + fact + "_base"}
    template = "page.header" if command == "build" else "page.head"
    assert callers("sample_xslt.xsl::" + template) == {"match=/", "match=title mode=toc", "f:clean"}
    creator = "createUser" if command == "build" else "makeUser"
    assert callers("sample.kt::UserRepository.save") == {creator}
    assert callers("sample.swift::InMemoryRepo.save") == {creator, "InMemoryRepo.init"}
    helper = "add" if command == "build" else "add_values"
    assert callers("sample.R::" + helper) == {"process_data"}
    assert callers("sample_zig.zig::helper") == {
        "main",
        "Point.distance",
        "test:helper increments@L36",
    }


if __name__ == "__main__":
    sys.exit(compare("extended-languages", corpus, assertions, update, proof))
