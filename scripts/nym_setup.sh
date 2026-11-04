#!/usr/bin/env bash
# Start two official nym-socks5-client instances with DIFFERENT, randomly chosen exits:
#   SYNC      → 127.0.0.1:1080 (id ghost-sync)
#   BROADCAST → 127.0.0.1:1081 (id ghost-bcast)
# Reply SURBs on. Usage: scripts/nym_setup.sh [up|down|status] [--reinit]
# Binary: $NYM_SOCKS5, else `nym-socks5-client` on PATH (build: cargo build --release -p nym-socks5-client
# in github.com/nymtech/nym; no macOS arm64 release binary exists as of v2026.21-hanoi).
set -euo pipefail

CMD="${1:-up}"
REINIT="${2:-}"
BIN="${NYM_SOCKS5:-$(command -v nym-socks5-client || true)}"
DIR="${GHOST_HOME:-$HOME/.ghost-layer}/nym"
API="https://validator.nymtech.net/api/v1"
mkdir -p "$DIR"

die() { echo "❌ $*" >&2; exit 1; }
port_up() { nc -z 127.0.0.1 "$1" 2>/dev/null; }

stop() {
  for id in ghost-sync ghost-bcast; do
    if [[ -f "$DIR/$id.pid" ]] && kill "$(cat "$DIR/$id.pid")" 2>/dev/null; then echo "stopped $id"; fi
    rm -f "$DIR/$id.pid"
  done
}

status() {
  for pair in ghost-sync:1080 ghost-bcast:1081; do
    id=${pair%%:*}; port=${pair##*:}
    if port_up "$port"; then echo "✅ $id on 127.0.0.1:$port (provider $(cat "$DIR/$id.provider" 2>/dev/null || echo '?'))"
    else echo "❌ $id not listening on $port (log: $DIR/$id.log)"; fi
  done
}

case "$CMD" in
  down) stop; exit 0 ;;
  status) status; exit 0 ;;
  up) ;;
  *) die "usage: $0 [up|down|status] [--reinit]" ;;
esac

[[ -x "$BIN" ]] || die "nym-socks5-client not found; set NYM_SOCKS5=/path/to/nym-socks5-client"
"$BIN" --version 2>/dev/null | grep -E "Build Version|Commit SHA" || true

# Pick two different healthy exits that run a network requester with an exit policy.
pick_providers() {
  local tmp; tmp="$(mktemp -d)"
  curl -fsS --max-time 60 "$API/unstable/nym-nodes/skimmed/exit-gateways/all" -o "$tmp/exits.json"
  curl -fsS --max-time 60 "$API/nym-nodes/described?per_page=5000" -o "$tmp/described.json"
  python3 -I - "$tmp/exits.json" "$tmp/described.json" <<'PY'
import json, random, sys
exits = json.load(open(sys.argv[1]))["nodes"]
exits = exits["data"] if isinstance(exits, dict) else exits
desc = json.load(open(sys.argv[2]))
desc = desc["data"] if isinstance(desc, dict) else desc
nr = {}
for d in desc:
    r = (d.get("description") or {}).get("network_requester") or {}
    if r.get("address") and r.get("uses_exit_policy"):
        nr[d["node_id"]] = r["address"]
good = [e for e in exits
        if e.get("supported_roles", {}).get("exit_nr") and float(e.get("performance", 0)) >= 0.95
        and e["node_id"] in nr and e.get("ip_addresses")]
random.SystemRandom().shuffle(good)
chosen, ips = [], set()
for e in good:
    if e["ip_addresses"][0] in ips:
        continue
    ips.add(e["ip_addresses"][0]); chosen.append(e)
    if len(chosen) == 2:
        break
if len(chosen) < 2:
    sys.exit("fewer than two healthy exits found")
for e in chosen:
    print(e["node_id"], e["ip_addresses"][0], nr[e["node_id"]])
PY
  rm -rf "$tmp"
}

stop
if [[ "$REINIT" == "--reinit" ]]; then rm -rf "$HOME/.nym/socks5-clients/ghost-sync" "$HOME/.nym/socks5-clients/ghost-bcast"; fi

PICKS=""
for pair in ghost-sync:1080 ghost-bcast:1081; do
  id=${pair%%:*}; port=${pair##*:}
  if [[ ! -d "$HOME/.nym/socks5-clients/$id" ]]; then
    [[ -n "$PICKS" ]] || PICKS="$(pick_providers)" || die "could not choose exits from $API"
    line=$([[ $id == ghost-sync ]] && echo "$PICKS" | sed -n 1p || echo "$PICKS" | sed -n 2p)
    read -r node ip provider <<<"$line"
    echo "→ init $id: exit node $node ($ip)"
    "$BIN" init --id "$id" --provider "$provider" --use-reply-surbs true >"$DIR/$id.init.log" 2>&1 \
      || { tail -5 "$DIR/$id.init.log"; die "init $id failed"; }
    echo "$node $ip $provider" > "$DIR/$id.provider"
  fi
  nohup "$BIN" run --id "$id" -p "$port" --host 127.0.0.1 >"$DIR/$id.log" 2>&1 &
  echo $! > "$DIR/$id.pid"
done

for i in $(seq 1 90); do
  port_up 1080 && port_up 1081 && break
  for id in ghost-sync ghost-bcast; do
    if ! kill -0 "$(cat "$DIR/$id.pid")" 2>/dev/null; then
      tail -20 "$DIR/$id.log" >&2
      # Known 1.1.86 quirk (zecmix): init writes a config `run` cannot load.
      grep -q "credential_requests_database" "$DIR/$id.log" && echo "hint: add credential_requests_database to the [storage_paths] section of ~/.nym/socks5-clients/$id/config/config.toml" >&2
      die "$id exited"
    fi
  done
  sleep 1
done
status
port_up 1080 && port_up 1081 || die "clients did not start listening within 90 s"
echo "✅ Nym ready: ghost up --nym  (sync via :1080, broadcast via :1081)"
