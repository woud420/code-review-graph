"""Compare the optional Rust SVG renderer with actual Matplotlib  + NetworkX SVG output."""

import argparse
import os
from pathlib import Path
import re
import sqlite3
import sys
import tempfile
import xml.etree.ElementTree as ET

from differential import new_repo
from public_parity import pinned_reference, run


def normalise(points):
    axes = list(zip(*points))
    return [tuple((v - min(a)) / (max(a) - min(a)) if min(a) != max(a) else 0.5
                  for v, a in zip(point, axes)) for point in points]


def read(xml, reference):
    root = ET.fromstring(xml, ET.XMLParser(target=ET.TreeBuilder(insert_comments=True)))
    assert root.tag == "{http://www.w3.org/2000/svg}svg"
    prefix = "{http://www.w3.org/2000/svg}"
    names = {n.tag for n in root.iter() if isinstance(n.tag, str)}
    assert names.isdisjoint({prefix + n for n in ("img", "evil", "script")})
    if reference:
        uses = root.findall(".//*[@id='PathCollection_1']//" + prefix + "use")
        colors = [re.search(r"fill: (#[a-z0-9]{6})", n.attrib["style"])[1] for n in uses]
        points = [(float(n.attrib["x"]), float(n.attrib["y"])) for n in uses]
        labels = [n.text.strip() for n in root.iter(ET.Comment)]
        assert labels.pop() == "Code Review Graph"
        paths = [n for n in root.iter(prefix+"path") if "stroke-opacity: 0.2" in n.get("style","") and "fill: none" in n.get("style","")]
    else:
        nodes = root.findall(".//*[@id='nodes']/" + prefix + "circle")
        colors = [n.attrib["fill"] for n in nodes]
        points = [(float(n.attrib["cx"]), float(n.attrib["cy"])) for n in nodes]
        labels = [n.text or "" for n in root.findall(".//*[@id='labels']/" + prefix + "text")]
        assert root.find(prefix+"title").text == "Code Review Graph"
        paths = root.findall(".//*[@id='edges']/" + prefix + "path")
    return colors, labels, len(paths), normalise(points)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--renderer-path", type=Path, help="isolated site-packages for the optional reference renderer")
    args = p.parse_args()
    source = args.upstream.resolve()
    pinned_reference(source)
    binary = [str(args.binary.resolve())]
    reference = [sys.executable, "-m", "code_review_graph"]
    with tempfile.TemporaryDirectory(prefix="crg-svg-") as temp:
        folder = Path(temp).resolve()
        env = {k:v for k,v in os.environ.items() if not k.startswith(("CRG_", "PYTHON"))}
        env.update(CRG_HOME=str(folder/"registry"), MPLCONFIGDIR=str(folder/"fontcache"),
                   PYTHONDONTWRITEBYTECODE="1", PYTHONHASHSEED="0", CRG_PARSE_EXECUTOR="serial")
        pyenv = env | {"PYTHONPATH": os.pathsep.join([str(source), *([str(args.renderer_path.resolve())] if args.renderer_path else [])])}
        # Font discovery emits platform-specific, one-time logs on a fresh cache.
        warm = run([sys.executable, "-c", "import matplotlib.font_manager, networkx"], folder, pyenv)
        assert warm["exit"] == 0, warm
        for n in (1, 4, 100, 101, 501):
            repo = folder / f"n{n}"
            functions = [f"def f{i}(): return " + (f"f{i-1}()" if i else "0") for i in range(n-1)]
            new_repo(repo, {"entry.py":"\n".join(functions)+"\n"})
            result = run([*reference, "build", "-q"], repo, pyenv)
            assert result["exit"] == 0, result
            db = repo / ".code-review-graph/graph.db"
            if n == 4:
                with sqlite3.connect(db) as conn:
                    conn.execute("UPDATE nodes SET name=? WHERE name='f1'", (r"<evil> $\qqq$ &",))
            outputs=[]
            for cmd, opts in ((reference, pyenv),(binary, env),(binary, env)):
                observation = run([*cmd, "visualize", "--format", "svg"], repo, opts)
                assert observation["exit"] == 0, observation
                outputs.append((observation, db.with_name("graph.svg").read_bytes()))
            (a, first), (b, second), (c, again) = outputs
            assert a == b == c and again == second, (a,b,c)
            labels1, text1, e1, xy1 = read(first, True)
            labels2, text2, e2, xy2 = read(second, False)
            # XML comments contain escaped spellings; native text nodes are decoded by the XML parser.
            from html import unescape
            assert ([unescape(text) for text in text1], labels1, e1) == (text2,labels2,e2), (n, text1[:5], text2[:5], e1,e2)
            assert len(xy1) == len(xy2) == n
            assert len(text1) == (n if n<=100 else 0), (n,text1)
            delta = max(abs(a-b) for u,v in zip(xy1,xy2) for a,b in zip(u,v))
            # The force solver uses the same seeded positions below 500 nodes. Normalize away canvas margins.
            if n < 500:
                assert delta < 0.001, (n,delta)
            print(f"{n} nodes: {e1} arrows, {len(text1)} labels, matching colors; max normalized coordinate difference {delta:.6g}")


if __name__ == "__main__":
    main()
