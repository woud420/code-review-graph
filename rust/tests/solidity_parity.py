"""Compare Solidity declarations, NatSpec, inheritance, modifiers and event edges."""

from hashlib import sha256
import sqlite3
import sys

from differential import git
from language_parity import compare


def corpus(upstream):
    sample = (upstream / "tests/fixtures/sample.sol").read_bytes()
    assert (
        sha256(sample).hexdigest()
        == "36e42f68c778c78112ec14455d2d47b7e6d30dead73e0ba289052864106e480d"
    )
    return {
        "sample.sol": sample.decode(),
        "extra/Util.sol": "library Util { function inc(uint n) internal pure returns (uint) { return n + 1; } }\n",
        "extra/Harness.sol": (
            'import "./Util.sol";\ncontract Harness {\n event Computed(uint n);\n modifier active(){ _; }\n'
            " function run(uint n) external active returns(uint) { emit Computed(n); return Util.inc(n); }\n}\n"
        ),
    }


def assertions(db, root):
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as conn:
        q = str(root / "sample.sol") + "::"
        for kind, src, dst in [
            ("CALLS", "StakingVault.stake", q + "Staked"),
            ("DEPENDS_ON", "StakingVault", "RewardMath"),
            ("INHERITS", "StakingVault", "IStakingPool"),
        ]:
            assert conn.execute(
                "SELECT 1 FROM edges WHERE kind=? AND source_qualified=? AND target_qualified=?",
                (kind, q + src, dst),
            ).fetchone() == (1,), (kind, src, dst)
        assert conn.execute(
            "SELECT return_type,modifiers,json_extract(extra,'$.solidity_kind') FROM nodes WHERE qualified_name=?",
            (q + "StakingVault.stakes",),
        ).fetchone() == ("mapping(address => uint256)", "public", "state_variable")
        assert (
            conn.execute("SELECT docstring FROM nodes WHERE qualified_name=?", (q + "BoostedPool",))
            .fetchone()[0]
            .startswith("@title BoostedPool")
        )


def update(root, files):
    (root / "extra/Util.sol").unlink()
    (root / "sample.sol").write_text(files["sample.sol"].replace("protocolFee", "vaultFee"))
    git(root, "commit", "-qam", "rename fee helper and remove imported Solidity library")


def proof(callers, command):
    name = "protocolFee" if command == "build" else "vaultFee"
    assert callers("sample.sol::" + name) == {"StakingVault.stake"}
    assert callers("sample.sol::BoostedPool.BonusClaimed") == {"BoostedPool.claimBonus"}


if __name__ == "__main__":
    sys.exit(compare("solidity", corpus, assertions, update, proof))
