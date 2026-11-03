#!/bin/sh
# Plan G-03: run every real-Chrome E2E suite against each given Chrome
# binary (headless suites directly, headed session reuse under xvfb-run).
#   scripts/chrome_e2e_matrix.sh /path/chrome-155 /path/chrome-156 ...
# Prints one summary line per binary and suite; exits non-zero on failure.
# Running as root needs a wrapper that adds --no-sandbox (Chrome refuses
# to start otherwise); CI runs as a normal user.
set -u
status=0
rounds=${RUSTSCRAPLING_REUSE_ROUNDS:-25}
for chrome in "$@"; do
  version=$("$chrome" --version 2>/dev/null | tr -d '\n')
  echo "== $chrome: ${version:-unknown}"
  export RUSTSCRAPLING_CHROME_FOR_TESTING="$chrome" SCRAPLING_EXECUTABLE_PATH="$chrome" \
    RUSTSCRAPLING_CHROME_PATH="$chrome" RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX=1 \
    RUSTSCRAPLING_REUSE_ROUNDS="$rounds"
  run() {
    label=$1; shift
    out=$("$@" 2>&1); code=$?
    passed=$(printf '%s\n' "$out" | grep -cE '\.\.\. ok$')
    failed=$(printf '%s\n' "$out" | grep -cE '\.\.\. FAILED$')
    echo "$label exit=$code passed=$passed failed=$failed"
    [ "$code" -eq 0 ] || { status=1; printf '%s\n' "$out" | grep -E 'panicked|left|right' | head -6; }
  }
  run browser-ignored cargo test --locked -p rustscrapling-browser --tests -- --ignored --test-threads=1
  run mcp-all cargo test --locked -p rustscrapling-mcp --tests -- --test-threads=1
  run mcp-session-reuse-headless cargo test --locked -p rustscrapling-mcp --test session_reuse_chrome_e2e headless_ -- --ignored
  if command -v xvfb-run >/dev/null 2>&1; then
    run mcp-session-reuse-headed xvfb-run -a cargo test --locked -p rustscrapling-mcp --test session_reuse_chrome_e2e headed_ -- --ignored
  else
    echo "mcp-session-reuse-headed BLOCKED (no xvfb-run)"
  fi
  run cli-shell-fetch cargo test --locked -p rustscrapling-cli --test shell_command -- --ignored
  run cli-extract-fetch cargo test --locked -p rustscrapling-cli --test extract_command -- --ignored
done
exit $status
