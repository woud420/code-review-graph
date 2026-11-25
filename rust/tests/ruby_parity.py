"""Compare a Ruby gem's namespaced classes, local imports and RSpec call evidence across edits."""
import argparse
from pathlib import Path
import tempfile

from differential import git, new_repo, verify_source
from incremental_snapshot import compare_builds

CART = """module Shop
  class Cart
    def initialize
      @balance = 0
    end
    def balance
      @balance
    end
  end
end
"""
SPEC = """require_relative '../lib/shop/cart'
RSpec.describe Shop::Cart do
  subject { described_class.new }
  it 'knows the balance' do
    subject.balance()
  end
end
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    upstream, binary = args.upstream.resolve(), args.binary.resolve()
    verify_source(upstream)
    with tempfile.TemporaryDirectory(prefix="crg-ruby-") as temp:
        scratch = Path(temp).resolve()
        root = scratch / "source"
        new_repo(root, {"lib/shop/cart.rb": CART, "spec/cart_spec.rb": SPEC,
                        "lib/report.rb": "require 'shop/cart'\nrequire 'json'\n"
                                         "def report(c)\n  c.balance()\nend\n"})
        for step in ("initial", "rename", "delete", "unchanged"):
            if step == "rename":
                (root / "lib/shop/cart.rb").write_text(CART.replace("balance", "amount"))
                (root / "spec/cart_spec.rb").write_text(SPEC.replace("balance", "amount"))
            elif step == "delete":
                (root / "lib/shop/cart.rb").unlink()
            if step in ("rename", "delete"):
                git(root, "add", "-A")
                git(root, "commit", "-qm", step)
            native = compare_builds(scratch, root, upstream, binary, step, update=step != "initial")
            print(f"{step}: {native['symbols'].total()} nodes, {native['edges'].total()} edges, all nine groups matched")


if __name__ == "__main__":
    main()
