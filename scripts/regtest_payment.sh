#!/usr/bin/env bash
# One regtest payment run: sender sync -> send to receiver -> mine 4 -> sender sync.
# Usage: regtest_payment.sh SERVER   (env: GHOST_REGTEST_WALLETS, SHADECHECK_DIR, ZCASH_DEVTOOL)
# Exits non-zero on any failure. Mining goes through Zebra RPC inside the ShadeCheck
# docker network (no host RPC port). Never prints seeds or mnemonics.
set -euo pipefail

SERVER="${1:?usage: run_payment.sh SERVER (host:port, e.g. 127.0.0.1:29067)}"
if [[ ! "$SERVER" =~ ^[A-Za-z0-9.-]+:[0-9]+$ ]]; then
  echo "run_payment: SERVER must be host:port, got '$SERVER'" >&2; exit 2
fi

WALLETS="${GHOST_REGTEST_WALLETS:?set GHOST_REGTEST_WALLETS (dir with sender/, receiver.ua)}"
DEVTOOL="${ZCASH_DEVTOOL:-zcash-devtool}"
SHADE="${SHADECHECK_DIR:?set SHADECHECK_DIR (ShadeCheck checkout)}"
COMPOSE=(docker compose -f "$SHADE/integration/modern/compose.yml")
AMOUNT_ZAT=1000000   # 0.01 REG per payment
MINE_BLOCKS=4
LOGDIR="$WALLETS/logs"
mkdir -p "$LOGDIR"
LOG="$LOGDIR/run-$(date +%Y%m%d-%H%M%S)-$$.log"

die() { echo "run_payment: FAILED: $*" >&2; echo "log: $LOG" >&2; exit 1; }

# Zebra RPC inside the docker network. The miner env var only needs to be set
# for compose interpolation; exec does not recreate the container.
zrpc() {
  local body
  body=$(printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "$1" "${2:-[]}")
  (cd "$SHADE" && SHADECHECK_MODERN_MINER=tmSRd1r8gs77Ja67Fw1JcdoXytxsyrLTPJm \
    "${COMPOSE[@]}" exec -T zebra curl -s --fail --max-time 120 \
    -H 'Content-Type: application/json' --data-binary "$body" http://127.0.0.1:18232) \
    || die "Zebra RPC $1 failed"
}
# zjson KEY: print result[KEY] from a JSON-RPC reply on stdin (missing key -> 0).
zjson() { python3 -I -c '
import json, sys
v = json.load(sys.stdin)
if v.get("error"):
    sys.exit("zebra error: %s" % v["error"])
r = v["result"]
for k in sys.argv[1].split("."):
    r = r.get(k, 0) if isinstance(r, dict) else r
print(r)' "$1"; }
tip() { zrpc getblockchaininfo | zjson blocks; }

wallet() { # wallet NAME <devtool args...>
  local name="$1"; shift
  "$DEVTOOL" wallet -w "$WALLETS/$name" "$@" 2>&1 | sed -E $'s/\x1b\\[[0-9;]*m//g' | tee -a "$LOG"
}

# Sync a wallet until its reported height equals the validator tip.
sync_to_tip() { # sync_to_tip NAME
  local name="$1" want got attempt
  for attempt in $(seq 1 60); do
    want=$(tip) || die "cannot read chain tip"
    wallet "$name" sync -s "$SERVER" >/dev/null 2>&1 || true
    got=$(wallet "$name" balance 2>/dev/null | grep -o 'Height: [0-9]*' | head -1 | awk '{print $2}') || true
    if [[ -n "$got" && "$got" -eq "$want" ]]; then
      echo "sync $name: height $got == tip $want (attempt $attempt)"
      return 0
    fi
    sleep 3
  done
  die "sync $name did not reach tip $want (last height '${got:-none}')"
}

echo "== run_payment server=$SERVER amount_zat=$AMOUNT_ZAT"
echo "== sender sync"
sync_to_tip sender

RECV_UA=$(cat "$WALLETS/receiver.ua") || die "missing $WALLETS/receiver.ua"
echo "== send $AMOUNT_ZAT zat sender -> receiver"
SEND_OUT=$(wallet sender send -i "$WALLETS/sender/identity.age" --address "$RECV_UA" \
  --value "$AMOUNT_ZAT" -s "$SERVER") || die "send command failed"
TXID=$(printf '%s\n' "$SEND_OUT" | grep -E '^[0-9a-f]{64}$' | tail -1 || true)
[[ -n "$TXID" ]] || die "no txid in send output"
echo "txid: $TXID"

echo "== confirm txid is in Zebra mempool"
zrpc getrawmempool | grep -qF "\"$TXID\"" || die "txid $TXID not in Zebra mempool"

echo "== mine $MINE_BLOCKS blocks"
zrpc generate "[$MINE_BLOCKS]" >/dev/null
echo "tip now: $(tip)"

echo "== verify txid mined"
CONF=$(zrpc getrawtransaction "[\"$TXID\",1]" | zjson confirmations) || die "getrawtransaction failed"
[[ "$CONF" -ge 1 ]] || die "txid $TXID has $CONF confirmations"
echo "confirmations: $CONF"

echo "== sender sync after payment"
sync_to_tip sender
echo "OK txid=$TXID"
