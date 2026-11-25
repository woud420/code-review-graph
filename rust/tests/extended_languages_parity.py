"""Differential raw, postprocessed and incremental graph comparisons on pinned fixtures."""
from pathlib import Path
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    directory = upstream / "tests/fixtures"
    names = ["sample.res", "sample.resi", "sample_xslt.xsl", "sample_xslt_common.xsl", "sample.kt", "sample.swift", "sample.R", "test_sample.R", "sample_zig.zig", "sample_zig_util.zig",
             "sample.nix", "sample_module.nix"]
    files = {name: (directory / name).read_text() for name in names}
    files["flake.nix"] = (
        '{ inputs.example.url = "github:example/project";\n'
        '  inputs = { inner.url = "github:inner/project"; };\n'
        '  build = import ./sample_module.nix; }\n'
    )
    return files


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as conn:
        for kind, file, owner, target in [
            ("CALLS","sample.res","::helper","sample.res::fact"),
            ("CONTAINS","sample.resi","::User","sample.resi::User.greet"),
            ("CALLS","sample_xslt.xsl","::match=/","sample_xslt.xsl::page.header"),
            ("IMPORTS_FROM","sample_xslt.xsl","","sample_xslt_common.xsl"),
            ("CALLS","sample.swift","::createUser","sample.swift::InMemoryRepo.save"),
            ("CALLS","sample.kt","::createUser","sample.kt::UserRepository.save"),
            ("CALLS", "sample.R", "::process_data", "sample.R::add"),
            ("CONTAINS","sample.R","::MyClass","sample.R::MyClass.greet"),
            ("TESTED_BY","sample_zig.zig","::helper","sample_zig.zig::test:helper increments@L36"),
            ("IMPORTS_FROM","sample_zig.zig","","sample_zig_util.zig"),
            ("CONTAINS","sample.nix","","sample.nix::packages.default"),
            ("IMPORTS_FROM","flake.nix","","sample_module.nix"),
        ]:
            row=conn.execute("SELECT 1 FROM edges WHERE kind=? AND source_qualified=? AND target_qualified=?",
                             (kind,str(root/file)+owner,str(root/target))).fetchone()
            assert row, (kind,file,owner,target)
        for url in ["github:example/project", "github:inner/project"]:
            assert conn.execute("SELECT 1 FROM edges WHERE kind='IMPORTS_FROM' AND target_qualified=?",(url,)).fetchone()


def update(root, files):
    (root/"sample.res").write_text(files["sample.res"].replace("fact", "factorial"))
    (root/"sample_xslt.xsl").write_text(files["sample_xslt.xsl"].replace("page.header", "page.head"))
    for name in ("sample.kt", "sample.swift"):
        (root/name).write_text(files[name].replace("createUser","makeUser"))
    (root/"sample_zig_util.zig").unlink()
    (root/"sample.R").write_text(files["sample.R"].replace("add", "add_values"))
    (root/"sample.nix").write_text(files["sample.nix"].replace("packages.default", "packages.build"))
    git(root,"commit","-qam","rename R and Nix definitions and remove the Zig import target")


def proof(callers, command):
    fact = "fact" if command=="build" else "factorial"
    assert callers("sample.res::"+fact) == {fact, "main", "helper", "test_"+fact+"_base"}
    template = "page.header" if command=="build" else "page.head"
    assert callers("sample_xslt.xsl::"+template) == {"match=/", "match=title mode=toc", "f:clean"}
    creator = "createUser" if command=="build" else "makeUser"
    assert callers("sample.kt::UserRepository.save") == {creator}
    assert callers("sample.swift::InMemoryRepo.save") == {creator, "InMemoryRepo.init"}
    helper="add" if command=="build" else "add_values"
    assert callers("sample.R::"+helper)=={"process_data"}
    assert callers("sample_zig.zig::helper")=={"main","Point.distance","test:helper increments@L36"}


if __name__=="__main__":
    sys.exit(compare("extended-languages",corpus,assertions, update,proof))
