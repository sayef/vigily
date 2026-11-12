#!/usr/bin/env bash
# Compares ingest through the SDK (bench/ingest.py) between a baseline engine and this checkout's.
#   bench/compare.sh [base-ref]        (default origin/main; ARGS="--procs 8 --runs 4 --points 100000")
#
# The baseline is vigily-engine built from base-ref in a temporary worktree (cached in
# target/bench-base), or BASE_ENGINE when it names a binary. Before 0.1.0-beta.3 the SDK wrote
# to storage in-process, but no release published that SDK as a downloadable wheel, so there is
# no embedded baseline to fetch: both sides use the same pure SDK from this checkout and only the
# engine differs (for example before and after kv0's track_many).
set -euo pipefail
cd "$(dirname "$0")/.."
BASE_REF="${1:-origin/main}"
ARGS="${ARGS:---procs 8 --runs 4 --points 100000}"
OUT=$(mktemp -d)
cleanup() {
  if [ -n "${WT:-}" ]; then git worktree remove --force "$WT" >/dev/null 2>&1 || true; fi
  rm -rf "$OUT"
  git worktree prune
}
trap cleanup EXIT

cargo build --release --locked -q -p vigily-server --bin vigily-engine
if [ -n "${BASE_ENGINE:-}" ]; then
  base="$BASE_ENGINE"
  label="$BASE_ENGINE"
else
  WT="$OUT/base"
  git worktree add --detach -q "$WT" "$BASE_REF"
  label="$BASE_REF ($(git -C "$WT" rev-parse --short HEAD))"
  CARGO_TARGET_DIR="$PWD/target/bench-base" \
    cargo build --release --locked -q --manifest-path "$WT/Cargo.toml" -p vigily-server --bin vigily-engine
  base="$PWD/target/bench-base/release/vigily-engine"
fi

echo "baseline: engine from $label"
# shellcheck disable=SC2086 # ARGS is a list of options
uv run --no-sync python bench/ingest.py $ARGS --engine "$base" --json "$OUT/base.json"
echo
echo "this checkout: engine from $(git rev-parse --short HEAD)$(git diff --quiet || echo ' with changes')"
# shellcheck disable=SC2086
uv run --no-sync python bench/ingest.py $ARGS --engine target/release/vigily-engine --json "$OUT/head.json"
echo
python3 -I - "$OUT/base.json" "$OUT/head.json" <<'PY'
import json, sys
base, head = (json.load(open(p)) for p in sys.argv[1:3])
print(f"{'':<34}{'baseline':>14}{'this checkout':>16}{'change':>10}")
for key, unit in [
    ("track_p50_us", "µs"), ("track_p99_us", "µs"), ("per_process_points_s", "/s"),
    ("engine_points_s", "/s"), ("engine_cpu_s", "s"), ("engine_peak_rss_mb", "MB"), ("visibility_s", "s"),
]:
    b, h = base.get(key), head.get(key)
    if b is None or h is None:
        continue
    change = f"{(h - b) / b * 100:+.0f} %" if b else ""
    print(f"{key + ' (' + unit + ')':<34}{b:>14,.2f}{h:>16,.2f}{change:>10}")
PY
