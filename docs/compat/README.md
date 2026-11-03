# Python Scrapling v0.5 → RustScrapling: M0 inventory

This is the **first, deliberately conservative** pass of the Plan.md v3 M0
compatibility inventory. It does not mean M0 or the GA requirements are done.

## Source of truth

- Upstream: `https://github.com/D4Vinci/Scrapling`
- Pin: `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`
- Rust baseline at first generation: `bb23bbf` (update the audit after merging)
- Upstream code is **not vendored**. The tools only use Python's standard library
  and parse syntax without importing or executing untrusted Scraping modules.

## Regeneration

First create a separate local, clean Git checkout of the exact upstream commit.
Do not replace the RustScrapling repository or check a moving branch without
verifying the full commit hash.

```sh
git clone --depth 1 https://github.com/D4Vinci/Scrapling.git /tmp/scrapling-v05-reference
git -C /tmp/scrapling-v05-reference rev-parse HEAD
python3 oracle/inventory_upstream.py --upstream /tmp/scrapling-v05-reference --out docs/compat
python3 oracle/audit_parity.py
python3 -m unittest discover -s tests -p test_m0_inventory.py -v
python3 oracle/inventory_upstream.py --upstream /tmp/scrapling-v05-reference --out docs/compat --check
python3 oracle/audit_parity.py --check
```

The SHA is checked **by the generator itself**, not trusted from the clone
command. Run from the RustScrapling project root. The unit tests require no
Rust build, Python-Scrapling dependencies, or external network after checkout.

## Artifacts

| File | What it proves / does not prove |
| --- | --- |
| `UPSTREAM_API_V05.json` | Direct method signatures of selected public classes, expanded known Click decorator options, TypedDict definitions and export names; **not** every inherited/module-level API or runtime behavior |
| `MCP_ACTIONS_V05.json` | Exactly 16 registered upstream tool signatures/descriptions and the 12 action-discriminated TypedDict definitions |
| `PARITY_AUDIT.json` | Structured static comparison against Rust `tools.json` plus browser action match-arm detection in `backend.rs`; never reports runtime parity as verified |
| `PARITY_MATRIX.md` | Human-readable audit, worklist and each scoped method's source location and tentative owner crate |

The public method listing includes `@overload` declarations as individual
source rows (`@overload:LINE`), distinct from runtime implementations.

## Critical findings and next steps

- The Rust MCP catalog has all 16 tool names, argument names, visible defaults,
  descriptions and all 12 action schema variants without a **static** difference
  detected by this pass. This is weaker than runtime JSON Schema parity.
- The `browser_actions` executor at the audited SHA has only four match-arm
  variants: `click`, `textbox`, `press_key`, `wait_time`; even these are limited
  to some argument values. The other eight variants return unsupported.
- Continue M0 by inspecting public module-level exports, inheritance, dynamic
  Click schemas, `Unpack[TypedDict]` runtime behavior and Rust method mapping.
- Add `rust_entrypoint`, expected input/error/result fixtures and `test_ids` to
  a separately reviewed evidence layer. **Do not** set `verified` simply because
  a name, JSON field or match-arm exists.
- Keep the generated files reproducible. If `tools.json` or `backend.rs` changes,
  regenerate the audit and inspect the difference; snapshot tests fail on drift.
