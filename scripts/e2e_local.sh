#!/usr/bin/env bash
# Full local experiment: regtest backend → ShadeCheck observer → "before" payments → Ghost Layer →
# "after" payments → ShadeCheck SC-001/SC-002 (strict) + attacker linkage reports.
#
# Env:
#   SHADECHECK_DIR          ShadeCheck checkout with .venv (github.com/0xaje/ShadeCheck-)
#   GHOST_REGTEST_WALLETS   dir with funded regtest wallets (sender/, receiver.ua) — see README
#   ZCASH_DEVTOOL           zcash-devtool built with --features regtest_support
#   N                       payments per run (default 5)
# Output: evidence/runs/<timestamp>/{before,after}/ and the two markdown reports there.
# Pass --publish to also copy the reports to evidence/BEFORE_AFTER.md and evidence/ATTACK_RESULTS.md.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
N="${N:-5}"
: "${SHADECHECK_DIR:?set SHADECHECK_DIR}" "${GHOST_REGTEST_WALLETS:?set GHOST_REGTEST_WALLETS}"
export SHADECHECK_DIR GHOST_REGTEST_WALLETS ZCASH_DEVTOOL="${ZCASH_DEVTOOL:-zcash-devtool}"
PY="$SHADECHECK_DIR/.venv/bin/python"
RUN="$ROOT/evidence/runs/$(date -u +%Y%m%dT%H%M%SZ)"
GHOST="$ROOT/target/release/ghost"
BACKEND=127.0.0.1:29067 OBS_PORT=9068 GW=127.0.0.1:9067
PIDS=()
RESULTS=()

ok()   { RESULTS+=("✅ $*"); echo "✅ $*"; }
fail() { RESULTS+=("❌ $*"); echo "❌ $*" >&2; summary; exit 1; }
summary() { echo; echo "== e2e_local summary"; printf '%s\n' "${RESULTS[@]}"; }
cleanup() { for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT
wait_port() { for _ in $(seq 1 60); do nc -z 127.0.0.1 "$1" 2>/dev/null && return 0; sleep 1; done; return 1; }
port_free() { ! nc -z 127.0.0.1 "$1" 2>/dev/null; }

start_observer() { # $1 = output events file
  (cd "$SHADECHECK_DIR" && exec "$PY" -m shadecheck.cli observe --upstream "$BACKEND" --plaintext-upstream \
    --port "$OBS_PORT" --output "$1") >"$(dirname "$1")/observer.log" 2>&1 &
  OBS_PID=$!; PIDS+=("$OBS_PID")
  wait_port "$OBS_PORT" || fail "observer did not start (see $(dirname "$1")/observer.log)"
}
stop() { kill "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; }

payments() { # $1 = server, $2 = label
  for i in $(seq 1 "$N"); do
    out=$(scripts/regtest_payment.sh "$1" 2>&1) || { echo "$out" | tail -5; fail "$2 payment $i failed"; }
    echo "  $2 payment $i: $(echo "$out" | grep -o 'OK txid=[0-9a-f]*')"
    echo "$out" | grep -o 'OK txid=[0-9a-f]*' >> "$RUN/$2/txids.txt"
  done
  ok "$N real regtest payments ($2)"
}

# 0. prerequisites
cargo build --release -q -p ghost-cli || fail "cargo build"
ok "ghost built ($("$GHOST" --version))"
nc -z 127.0.0.1 29067 || (cd "$SHADECHECK_DIR" && docker compose -f integration/modern/compose.yml up -d zebra zaino) \
  || fail "start regtest backend"
wait_port 29067 || fail "Zaino not reachable on $BACKEND"
ok "regtest backend up (Zebra + Zaino on $BACKEND)"
port_free "$OBS_PORT" || fail "port $OBS_PORT busy"
port_free 9067 || fail "port 9067 busy (stop other gateways)"
mkdir -p "$RUN/before" "$RUN/after"

# 1. before: wallet → observer → backend
start_observer "$RUN/before/events.jsonl"
payments "127.0.0.1:$OBS_PORT" before
stop "$OBS_PID"

# 2. after: wallet → Ghost Layer (direct) → observer → backend
start_observer "$RUN/after/events.jsonl"
GH="$(mktemp -d)"
GHOST_HOME="$GH" "$GHOST" up --server "127.0.0.1:$OBS_PORT" --profile demo >"$RUN/after/gateway.out" 2>"$RUN/after/gateway.log" &
GW_PID=$!; PIDS+=("$GW_PID")
wait_port 9067 || fail "ghost up did not start (see $RUN/after/gateway.log)"
ok "ghost-gateway up in front of the observer"
payments "$GW" after
stop "$GW_PID"; stop "$OBS_PID"
UNALIGNED=$(grep -c 'aligned=false' "$RUN/after/gateway.log" || true)
[[ "$UNALIGNED" == 0 ]] || fail "$UNALIGNED unaligned upstream ranges with Ghost Layer"
ok "0 unaligned upstream GetBlockRange requests with Ghost Layer"

# 3. ShadeCheck (strict). FAIL verdicts are results, not script errors.
echo '{"schema_version":1,"sync":{"chunk_size":1152,"alignment_height":0}}' > "$RUN/policy.json"
for r in before after; do
  (cd "$SHADECHECK_DIR" && "$PY" -m shadecheck.cli test --events "$RUN/$r/events.jsonl" --rules SC-002 --policy strict \
     --output "$RUN/$r/sc-002.json" || [[ $? -le 2 ]]) >/dev/null || fail "ShadeCheck SC-002 ($r)"
  (cd "$SHADECHECK_DIR" && "$PY" -m shadecheck.cli test --events "$RUN/$r/events.jsonl" --rules SC-001 --config "$RUN/policy.json" \
     --policy strict --output "$RUN/$r/sc-001.json" || [[ $? -le 2 ]]) >/dev/null || fail "ShadeCheck SC-001 ($r)"
done
ok "ShadeCheck SC-001 + SC-002 evaluated (strict) for both runs"

# 4. reports
"$GHOST" report --before "$RUN/before" --after "$RUN/after" --out "$RUN/BEFORE_AFTER.md" >/dev/null || fail "ghost report"
"$GHOST" report --attack --before "$RUN/before/events.jsonl" --after "$RUN/after/events.jsonl" --out "$RUN/ATTACK_RESULTS.md" >/dev/null \
  || fail "ghost report --attack"
B=$("$GHOST" --json attack --events "$RUN/before/events.jsonl" --single-ip | python3 -I -c 'import json,sys;a=json.load(sys.stdin);print(sum(s["linked"] for s in a["sends"]))')
A=$("$GHOST" --json attack --events "$RUN/after/events.jsonl" --single-ip | python3 -I -c 'import json,sys;a=json.load(sys.stdin);print(sum(s["linked"] for s in a["sends"]))')
echo "  linked payments (same-connection or sync-then-send): before $B/$N, after $A/$N"
[[ "$A" -lt "$B" || "$B" == 0 ]] || fail "Ghost Layer did not reduce linkage (before $B, after $A)"
ok "linkage before $B/$N → after $A/$N"
if [[ "${1:-}" == "--publish" ]]; then
  cp "$RUN/ATTACK_RESULTS.md" evidence/ATTACK_RESULTS.md
  ok "published ATTACK_RESULTS.md (BEFORE_AFTER.md keeps its hand-written interpretation section)"
fi
summary
echo "reports: $RUN"
