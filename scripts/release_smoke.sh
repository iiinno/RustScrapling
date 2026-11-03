#!/bin/sh
# Plan G-12/G-13: release binary smoke test and artifact checksums.
#   scripts/release_smoke.sh [target/release/rustscrapling] [outdir]
# Builds (if needed) the locked release binary, checks --version and every
# subcommand's --help, runs the localhost E2E suite and the shell/install
# dry-run against the release binary, and writes SHA256SUMS plus a JSON
# manifest (git SHA, rustc, size) to outdir.
set -eu
cd "$(dirname "$0")/.."
binary=${1:-target/release/rustscrapling}
outdir=${2:-target/release-smoke}
if [ ! -x "$binary" ]; then
  cargo build --release --locked -p rustscrapling-cli
fi
mkdir -p "$outdir"
"$binary" --version
commands=$("$binary" --help | awk '/^Commands:/{on=1;next} /^Options:/{on=0} on && $1!="help" {print $1}')
count=0
for command in $commands; do
  "$binary" "$command" --help >/dev/null
  count=$((count + 1))
done
echo "help OK for $count subcommands"
python3 tests/e2e_local.py "$binary"
printf 'load %s\ncss h1::text\n' "$PWD/fixtures/oracle/convertor/markdown.html" \
  | "$binary" shell >/dev/null
"$binary" install --dry-run --dir "$outdir/browsers" >/dev/null
cp "$binary" "$outdir/rustscrapling"
(cd "$outdir" && sha256sum rustscrapling > SHA256SUMS && cat SHA256SUMS)
python3 - "$outdir" "$binary" <<'PY'
import json, os, platform, subprocess, sys
out, binary = sys.argv[1], sys.argv[2]
manifest = {
    "git_sha": subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip(),
    "dirty": bool(subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], capture_output=True, text=True).stdout.strip()),
    "rustc": subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip(),
    "platform": platform.platform(),
    "version": subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip(),
    "size_bytes": os.path.getsize(binary),
    "sha256": open(os.path.join(out, "SHA256SUMS")).read().split()[0],
}
json.dump(manifest, open(os.path.join(out, "manifest.json"), "w"), indent=1)
print(json.dumps(manifest))
PY
echo "release smoke OK"
