"""Compare Rust import/trait target paths and confidence metadata against a pinned Cargo workspace."""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile

from differential import new_repo, verify_source
from public_parity import run


def edges(db, repo):
    root = str(repo) + "/"
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as con:
        return Counter(
            (kind, source.removeprefix(root), target.removeprefix(root), file.removeprefix(root),
             line, resolution, json.dumps(json.loads(extra or "{}"), sort_keys=True).replace(root,""), tier, confidence)
            for kind, source, target, file, line, resolution, extra, tier, confidence in con.execute(
                "SELECT kind,source_qualified,target_qualified,file_path,line,target_resolution,"
                "extra,confidence_tier,confidence FROM edges WHERE kind IN ('IMPORTS_FROM','IMPLEMENTS','REFERENCES') "
                "AND file_path LIKE '%.rs'"
            )
        )


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream",required=True,type=Path)
    p.add_argument("--binary",required=True,type=Path)
    args=p.parse_args()
    source=args.upstream.resolve(); binary=args.binary.resolve()
    verify_source(source)
    with tempfile.TemporaryDirectory(prefix="crg-cargo-") as temp:
        base=Path(temp).resolve();repo=base/"workspace"
        files={
            "Cargo.toml": '[workspace]\nmembers=["app", "dep"]\n[workspace.dependencies]\nrenamed-core={path="dep",package="traits-library"}\n',
            "app/Cargo.toml": '[package]\nname="app"\nversion="0.1.0"\nedition="2021"\n[target.\'cfg(unix)\'.dependencies]\nrenamed-core.workspace=true\n[dev-dependencies]\nother={package="traits-library",path="../dep"}\noutside={path="../../outside"}\nmissing={path="../noncrate"}\n',
            "dep/Cargo.toml": '[package]\nname="traits-library"\nversion="0.1.0"\n[lib]\npath="code/root.rs"\n',
            "dep/code/root.rs": "pub mod shapes;\npub trait Root {}\n",
            "dep/code/shapes/mod.rs":"pub trait Shape<T> {}\npub struct Square;\n",
            "app/src/lib.rs": "pub mod models;\npub mod inner;\n",
            "app/src/models.rs":"pub struct Model<T>(pub T);\npub trait Print<T> {}\npub fn notify() {}\n",
            "app/src/vec.rs":"pub struct Item;\n",
            "app/src/inner/mod.rs": "pub mod use_it;\npub struct Child;\n",
            "app/src/inner/use_it.rs": (
                "use crate::models::{Model as M, Print as P};\n"
                "use renamed_core::shapes::{self as shapes, Shape as S, Square};\n"
                "use renamed_core::shapes as lib_shapes;\n"
                "use other::Root as R;\n"
                "use self::Local;\nuse super::{self, Child};\n"
                "use super::super::models::Print;\n"
                "use outside::Lost;\nuse missing::Missing;\nuse Vec::Item;\n"
                "use std::{fmt::{self, Display as D}, io::Read};\nuse crate::models::*;\n"
                "use crate::models::{notify, notify as callback};\nuse unknown_pkg::foreign;\n"
                "fn deliver() { consume(notify); consume(callback); consume(foreign); }\n"
                "struct Wrap<T>(T);\ntrait Local {}\n"
                "impl<T> S<T> for Wrap<T> {}\nimpl<T> P<T> for Wrap<T> {}\n"
                "impl Local for Wrap<i32> {}\nimpl R for M<u8> {}\n"
                "impl lib_shapes::Shape<u8> for crate::models::Model<u8> {}\n"
                "impl<T> std::fmt::Display for &Wrap<T> {}\n"
                "impl<T> D for [T] {}\n"
            ),
            "noncrate/src/lib.rs":"pub struct Missing;\n",
            "alternate/Cargo.toml": '[package]\nname="traits-library"\nversion="0.2.0"\n',
            "alternate/src/lib.rs": "pub mod shapes; pub trait Root {}\n",
            "alternate/src/shapes.rs":"pub trait Shape<T> {}\npub struct Square;\n",
            "standalone/file.rs": "use crate::models::Model;\nstruct Cell<T>(T);\nimpl Drop for Cell<'_> {}\n",
        }
        new_repo(repo,files)
        outside=base/"outside";(outside/"src").mkdir(parents=True)
        (outside/"Cargo.toml").write_text('[package]\nname="outside"\nversion="0.1.0"\n')
        (outside/"src/lib.rs").write_text("pub struct Lost;")
        env={k:v for k,v in os.environ.items() if k in ("PATH","TMP","TEMP","TMPDIR")}
        env.update(HOME=str(base),CRG_HOME=str(base/"registry"),CRG_PARSE_EXECUTOR="serial",PYTHONDONTWRITEBYTECODE="1",PYTHONHASHSEED="0")
        commands=[([sys.executable,"-m","code_review_graph"],env|{"PYTHONPATH":str(source)},base/"reference"),
                  ([str(binary)],env,base/"native")]
        for label,cmd in [("raw","build"),("full","build"),("edit","update"),("manifest-only","update"),("new-manifest","build")]:
            if label=="edit":
                file=repo/"app/src/inner/use_it.rs"
                file.write_text(file.read_text().replace("use other::Root as R;", "use renamed_core::shapes::Shape as R;"))
            if label=="manifest-only":
                manifest=repo/"Cargo.toml"
                manifest.write_text(manifest.read_text().replace('path="dep"','path="alternate"'))
            pairs=[]
            for command,environ,directory in commands:
                req=[*command,cmd,"--data-dir",str(directory),"-q"]
                if label=="raw":req+=["--skip-postprocess"]
                r=run(req,repo,environ)
                assert r["exit"]==0,(req,r)
                pairs.append(edges(directory/"graph.db",repo))
            a,b=pairs
            assert a==b,(label,list((a-b).items())[:8],list((b-a).items())[:8])
            imp={row[2] for row in a if row[0]=="IMPORTS_FROM"}
            traits={(row[1],row[2]) for row in a if row[0]=="IMPLEMENTS"}
            dep = "alternate/src/shapes.rs" if label=="new-manifest" else "dep/code/shapes/mod.rs"
            assert {"outside::Lost","missing::Missing","Vec::Item",dep,"app/src/models.rs"}<=imp,imp
            assert ("app/src/inner/use_it.rs::Wrap",f"{dep}::Shape") in traits,traits
            assert ("app/src/inner/use_it.rs::Wrap","app/src/models.rs::Print") in traits,traits
            assert not any("[" in src or "&" in src for src,_ in traits),traits
            refs={row[2] for row in a if row[0]=="REFERENCES"}
            assert {"app/src/models.rs::notify","foreign"}<=refs, refs
            assert not any(str(outside) in row[2] for row in a)
            print(f"{label}: {sum(c for x,c in a.items() if x[0]=='IMPORTS_FROM')} imports and {sum(c for x,c in a.items() if x[0]=='IMPLEMENTS')} impl edges, {sum(c for x,c in a.items() if x[0]=='REFERENCES')} callback references match, including confidence and metadata")


if __name__=="__main__":
    main()
