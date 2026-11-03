#!/bin/sh
# Fast checks to run before every push (the full gates run in CI):
# formatting, the M0 parity audit snapshot (it indexes Rust function
# locations, so most Rust edits require `python3 oracle/audit_parity.py`)
# and the M0 Python tests.
set -e
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
python3 oracle/audit_parity.py --check
python3 -m unittest discover -s tests -p "test_m0_*.py"
python3 oracle/check_evidence.py --check
echo "quick checks OK"
