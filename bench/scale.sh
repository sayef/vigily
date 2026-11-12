#!/usr/bin/env bash
# Times vigily's server on a repository made by bench/scale.py.
#   bench/scale.sh /tmp/vigily-scale [port]   (BIN=target/release/vigily; UP_ARGS=--no-optimize to skip compaction)
set -euo pipefail
REPO="$1"; PORT="${2:-7601}"; BIN="${BIN:-target/release/vigily}"; URL="http://127.0.0.1:$PORT"
t0=$(date +%s%N)
TOKEN_FILE=$(mktemp); rm -f "$TOKEN_FILE"
"$BIN" up --repo "$REPO" --port "$PORT" --token-file "$TOKEN_FILE" ${UP_ARGS:-} >/dev/null 2>&1 &
PID=$!; trap 'kill $PID 2>/dev/null; rm -f "$TOKEN_FILE"' EXIT
until curl -sf -o /dev/null "$URL/api/v1/health"; do sleep 0.2; done
AUTH="Authorization: Bearer $(cat "$TOKEN_FILE")"
printf 'start-up (first catalog refresh)       %6.2f s\n' "$(awk "BEGIN{print ($(date +%s%N) - $t0)/1e9}")"
time_get() { curl -s -o /dev/null -w "%{time_total}" -H "$AUTH" "$URL$1"; }
time_post() { curl -s -o /dev/null -w "%{time_total}" -H "$AUTH" -H 'content-type: application/json' -d "$2" "$URL$1"; }
printf 'GET /api/v1/meta                       %6.3f s\n' "$(time_get /api/v1/meta)"
printf 'GET /api/v1/runs (all)                 %6.3f s\n' "$(time_get /api/v1/runs)"
printf 'GET /api/v1/runs filter + sort         %6.3f s\n' "$(time_get '/api/v1/runs?q=lr%3C0.01%20exp:sweep-1&sort=metric:min:loss%5Bval%5D')"
IDS=$(curl -s -H "$AUTH" "$URL/api/v1/runs" | python3 -I -c 'import sys,json; print(json.dumps([r["id"] for r in json.load(sys.stdin)["rows"][:200]]))')
BODY="{\"runs\": $IDS, \"metric\": \"loss[train]\", \"points\": 800}"
printf 'POST /api/v1/chart 200 runs (cold)     %6.3f s\n' "$(time_post /api/v1/chart "$BODY")"
printf 'POST /api/v1/chart 200 runs (cached)   %6.3f s\n' "$(time_post /api/v1/chart "$BODY")"
ZOOM="{\"runs\": $IDS, \"metric\": \"loss[train]\", \"points\": 800, \"range\": [100, 400]}"
printf 'POST /api/v1/chart 200 runs, zoomed    %6.3f s\n' "$(time_post /api/v1/chart "$ZOOM")"
echo "server RSS: $(ps -o rss= -p $PID | awk '{printf "%.0f MB", $1/1024}')"
