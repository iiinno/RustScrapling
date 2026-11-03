"""Conservative Rust source symbol candidate index, not a proof of public API.

Uses Python stdlib only, so the inventory can run on Windows without Cargo.
The regex intentionally misses macro-generated or trait-impl symbols and may
include functions inside private modules. Every match remains unverified.
"""

from __future__ import annotations

import re
from pathlib import Path


PUBLIC_FUNCTION = re.compile(
    r"(?m)^\s*pub(?:\([^\n)]*\))?\s+(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(?:r#)?([A-Za-z_][A-Za-z_0-9]*)\s*(?:<|\()"
)


def index_rust_functions(root: Path) -> dict[str, list[dict]]:
    crates_dir = root / "crates"
    if not crates_dir.is_dir():
        raise FileNotFoundError("Missing Rust crates/ directory")
    candidates: dict[str, list[dict]] = {}
    for file in sorted(crates_dir.glob("*/src/**/*.rs")):
        relative = file.relative_to(root).as_posix()
        crate = relative.split("/")[1]
        content = file.read_text(encoding="utf-8")
        for match in PUBLIC_FUNCTION.finditer(content):
            symbol = match.group(1)
            candidates.setdefault(symbol, []).append({
                "crate": crate,
                "source": relative,
                "line": content.count("\n", 0, match.start(1)) + 1,
                "qualified_symbol_unknown": True,
            })
    for rows in candidates.values():
        rows.sort(key=lambda row: (row["crate"], row["source"], row["line"]))
    return dict(sorted(candidates.items()))


def same_name_candidates(method: dict, symbols: dict[str, list[dict]], preferred_crate: str | None) -> list[dict]:
    # Includes @overload signatures but no new implementation should be
    # inferred for each overload. Rank candidate modules by expected owner.
    member = method["id"].rsplit(".", 1)[-1].split("@overload:", 1)[0]
    rows = symbols.get(member, [])
    return sorted(rows, key=lambda row: (row["crate"] != preferred_crate, row["crate"], row["source"], row["line"]))
