#!/usr/bin/env bash
# Real testnet payment through Ghost Layer, verified on-chain.
#   ghost doctor → wallet sync through the gateway → send → poll `ghost verify` until mined →
#   explorer links → append to evidence/testnet_tx.md
#
# Env:
#   WALLET_DIR      zcash-devtool wallet dir (funded, testnet)
#   IDENTITY        its age identity file
#   TO_ADDRESS      recipient address (utest1…)
#   AMOUNT_ZAT      default 100000 (0.001 TAZ)
#   ZCASH_DEVTOOL   default zcash-devtool
#   MODE            nym (default; needs scripts/nym_setup.sh up) | direct | before (no Ghost Layer)
#   EVENTS          optional: upstream event log path for `ghost up --event-log` (attacker analysis)
set -euo pipefail
cd "$(dirname "$0")/.."
: "${WALLET_DIR:?}" "${IDENTITY:?}" "${TO_ADDRESS:?}"
AMOUNT_ZAT="${AMOUNT_ZAT:-100000}"
DT="${ZCASH_DEVTOOL:-zcash-devtool}"
MODE="${MODE:-nym}"
GHOST="$PWD/target/release/ghost"
SERVER=testnet.zec.rocks:443
RESULTS=()
ok()   { RESULTS+=("✅ $*"); echo "✅ $*"; }
fail() { RESULTS+=("❌ $*"); echo "❌ $*" >&2; summary; exit 1; }
summary() { echo; echo "== e2e_testnet summary ($MODE)"; printf '%s\n' "${RESULTS[@]}"; }
GW_PID=""
trap '[[ -n "$GW_PID" ]] && kill "$GW_PID" 2>/dev/null || true' EXIT

cargo build --release -q -p ghost-cli || fail "cargo build"
NYMFLAG=(); [[ "$MODE" == nym ]] && NYMFLAG=(--nym)
"$GHOST" doctor ${NYMFLAG[@]+"${NYMFLAG[@]}"} --listen 127.0.0.1:19067 || fail "ghost doctor"
ok "ghost doctor green ($MODE)"

LOG="evidence/logs/testnet_${MODE}_$(date -u +%Y%m%dT%H%M%SZ).log"
mkdir -p evidence/logs
if [[ "$MODE" == before ]]; then
  WS="$SERVER"
else
  ! nc -z 127.0.0.1 9067 2>/dev/null || fail "port 9067 busy (stop the other gateway)"
  EVFLAG=(); [[ -n "${EVENTS:-}" ]] && EVFLAG=(--event-log "$EVENTS")
  "$GHOST" up ${NYMFLAG[@]+"${NYMFLAG[@]}"} ${EVFLAG[@]+"${EVFLAG[@]}"} --server "$SERVER" --profile demo >"$LOG.out" 2>"$LOG" &
  GW_PID=$!
  for _ in $(seq 1 240); do nc -z 127.0.0.1 9067 2>/dev/null && break; sleep 1; done
  nc -z 127.0.0.1 9067 || fail "ghost up did not start (see $LOG)"
  ok "ghost up $MODE on 127.0.0.1:9067"
  WS=127.0.0.1:9067
fi

"$DT" wallet -w "$WALLET_DIR" sync -s "$WS" >/dev/null 2>&1 || fail "wallet sync via $WS"
ok "wallet synced via $WS ($("$DT" wallet -w "$WALLET_DIR" balance 2>&1 | grep -o 'Height: [0-9]*'))"

T0=$(date +%s)
OUT=$("$DT" wallet -w "$WALLET_DIR" send -i "$IDENTITY" --address "$TO_ADDRESS" --value "$AMOUNT_ZAT" -s "$WS" 2>&1) \
  || { echo "$OUT" | tail -5; fail "send"; }
TXID=$(echo "$OUT" | sed $'s/\x1b\\[[0-9;]*m//g' | grep -E '^[0-9a-f]{64}$' | tail -1)
[[ -n "$TXID" ]] || fail "no txid in wallet output"
ok "sent $AMOUNT_ZAT zat in $(( $(date +%s) - T0 )) s, txid $TXID"

for _ in $(seq 1 60); do
  if "$GHOST" verify ${NYMFLAG[@]+"${NYMFLAG[@]}"} "$TXID" >/dev/null 2>&1; then break; fi
  sleep 15
done
VERIFY=$("$GHOST" verify ${NYMFLAG[@]+"${NYMFLAG[@]}"} "$TXID") || fail "not mined after 15 min: $TXID"
ok "mined: $(echo "$VERIFY" | head -1)"

{
  echo
  echo "## $MODE — $(date -u +%FT%TZ)"
  echo
  echo "- txid: \`$TXID\`"
  echo "- explorer: https://testnet.zcashexplorer.app/transactions/$TXID"
  echo "- wallet → $( [[ $MODE == before ]] && echo "testnet.zec.rocks directly (no Ghost Layer)" || echo "Ghost Layer ($MODE) → testnet.zec.rocks")"
  echo
  echo '`ghost verify`:'; echo '```'; echo "$VERIFY"; echo '```'
  if [[ "$MODE" != before ]]; then
    echo; echo 'Ghost Layer broadcast log:'; echo '```'
    grep -E "new sync|queued|fresh connection|broadcast done" "$LOG" | cut -c1-400 || true
    echo '```'
    echo; echo '`ghost ip`:'; echo '```'; "$GHOST" ip ${NYMFLAG[@]+"${NYMFLAG[@]}"} || true; echo '```'
  fi
} >> evidence/testnet_tx.md
ok "appended to evidence/testnet_tx.md"
summary
