# MCP demo — real client (Claude Code 2.1.318 (Claude Code)) → `ghost mcp` (2026-11-04T14:33:51Z)

Config (`--mcp-config`):
```json
{"mcpServers": {"ghost-layer": {"command": "<repo>/target/debug/ghost", "args": ["mcp"]}}}
```

Prompt: *"What's my ghost status? … call ghost_status, then ghost_verify for txid 09f0f0b0…ce69 …"* (`claude -p … --strict-mcp-config --allowedTools mcp__ghost-layer__ghost_status mcp__ghost-layer__ghost_verify`)

Transcript (tool calls and results as recorded in Claude Code's stream-json output; results truncated to 200 chars):
```
init mcp_servers: [{'name': 'ghost-layer', 'status': 'connected', 'source': 'dynamic'}]
TOOL CALL: ToolSearch {"query": "select:mcp__ghost-layer__ghost_status,mcp__ghost-layer__ghost_verify", "max_results": 2}
TOOL RESULT: {"type": "tool_reference", "tool_name": "mcp__ghost-layer__ghost_status"} {"type": "tool_reference", "tool_name": "mcp__ghost-layer__ghost_verify"}
TOOL CALL: mcp__ghost-layer__ghost_status {}
TOOL RESULT: {"broadcasts":[{"delay_s":14.702,"explorer":["https://testnet.zcashexplorer.app/transactions/14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5","https://zexplorer.app/testnet/tx/14434aa
TOOL CALL: mcp__ghost-layer__ghost_verify {"txid": "09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69"}
TOOL RESULT: {"confirmations":223,"explorer":["https://testnet.zcashexplorer.app/transactions/09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69","https://zexplorer.app/testnet/tx/09f0f0b01942a658197
CLAUDE: You have 3 broadcasts, all sent over Nym: 2 accepted and 1 rejected.
- `14434aab…`: **rejected**. The node's reason was "transaction was committed to the best chain", meaning it was already on-chain when this broadcast arrived.
- `09f0f0b0…`: **accepted**
- `1483bfca…`: **accepted**

Tx `09f0f0b0…` is **mined** at height 4,582,118 with 223 confirmations (chain tip 4,582,340).
Explorer: https://testnet.zcashexplorer.app/transactions/09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69
result: success
```

## Raw JSON-RPC check (stdio, no LLM)

```
initialize: {'name': 'ghost-layer', 'version': '0.1.0'} | instructions: Ghost Layer: unlinks Zcash testnet payments from wallet sync (split br
tools: ['ghost_doctor', 'ghost_explain', 'ghost_ip', 'ghost_report', 'ghost_status', 'ghost_verify']
ghost_explain → isError=False :: {"leaks":[{"ghost_layer":"--nym: sync and broadcast leave through two different Nym exits; the server never sees your IP (fail closed).","leak":"ip","what":"Your face (IP address)","why":"The server sees where you connect from."},
ghost_status → isError=False :: {"broadcasts":[{"delay_s":14.702,"explorer":["https://testnet.zcashexplorer.app/transactions/14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5","https://zexplorer.app/testnet/tx/14434aabc6b892b7201e5f3f80ad0be12392b
ghost_verify {"txid": "09f0f0b01942a658197f65563a977c → isError=False :: {"confirmations":225,"explorer":["https://testnet.zcashexplorer.app/transactions/09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69","https://zexplorer.app/testnet/tx/09f0f0b01942a658197f65563a977c66fc19f8823268b0e54
ghost_verify {"txid": "bad"} → isError=True :: txid must be 64 hex characters
```

## ghost_pay safety caps (`--allow-pay --max-amount 0.001 --max-per-hour 2`), no funds spent

```
{"args":{},"outcome":"exit 0","tool":"ghost_explain","ts_ms":1793802831423}
{"args":{},"outcome":"exit 0","tool":"ghost_status","ts_ms":1793802831443}
{"args":{"txid":"09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69"},"outcome":"exit 0","tool":"ghost_verify","ts_ms":1793802833117}
```

Without caps: `ghost mcp --allow-pay` → exit 2, "--allow-pay needs --max-amount, --max-per-hour, --wallet and --identity".
