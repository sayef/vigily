#!/usr/bin/env bash
# Regenerate docs/assets/screenshots from the synthetic demo data: scripts/screenshots.sh
#
# Builds the web app, the release binary and the Python SDK (SKIP_BUILD=1 reuses what is built;
# BIN=path/to/vigily picks the binary), logs the demo runs and evaluations into a fresh repository,
# serves it, takes every screen in both themes with `npm run qa` (target/qa), and keeps the ones the
# README and the docs use (assets/src/screenshots.py). Needs Node, uv and a Chromium for Playwright.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
PORT="${PORT:-7600}"
BIN="${BIN:-$ROOT/target/release/vigily}"

# A short, neutral location: the repository's path shows in the screenshots.
WORK="${TMPDIR:-/tmp}/vigily"
if [ -e "$WORK" ]; then
  echo "$WORK already exists; remove it first" >&2
  exit 1
fi

if [ "${SKIP_BUILD:-}" != 1 ]; then
  (cd crates/vigily-ui && npm ci --silent --no-audit --no-fund && npm run build --silent)
  cargo build --release -p vigily-cli
  uv sync --no-install-project --quiet
  uv run --no-sync maturin develop --release --quiet
fi
(cd crates/vigily-ui && [ -x node_modules/.bin/playwright ] || npm ci --silent --no-audit --no-fund)

mkdir -p "$WORK"
cleanup() {
  if [ -n "${SERVER:-}" ]; then kill "$SERVER" 2>/dev/null || true; fi
  rm -rf -- "$WORK"
}
trap cleanup EXIT

uv run --no-sync python examples/demo.py --repo "$WORK/demo"
uv run --no-sync python examples/eval_demo.py --repo "$WORK/demo"
"$BIN" up --repo "$WORK/demo" --config examples/vigily.toml --port "$PORT" > "$WORK/up.log" 2>&1 &
SERVER=$!
for _ in $(seq 60); do
  curl -fsS -o /dev/null "http://127.0.0.1:$PORT/api/health" && break
  sleep 0.5
done
curl -fsS -o /dev/null "http://127.0.0.1:$PORT/api/health" || { cat "$WORK/up.log" >&2; exit 1; }

(cd crates/vigily-ui && VIGILY_URL="http://127.0.0.1:$PORT" npm run qa -- "$ROOT/target/qa")
uv run --no-project --with pillow python assets/src/screenshots.py "$ROOT/target/qa"
