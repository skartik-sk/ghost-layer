# Draft issue for 0xaje/ShadeCheck- (not posted yet)

**Title:** SC-002 flags every broadcast regardless of connection or timing: proposal for a session-aware variant

Thanks for ShadeCheck; we used it as an independent check for [Ghost Layer](https://github.com/skartik-sk/ghost-layer).

Observation (`shadecheck/core.py` 90–114): SC-002 produces a MEDIUM finding for **every** non-empty `SendTransaction`
seen by the observer, and HIGH when the same bytes appear in a `GetMempoolStream` response within 30 s. It never compares
the send's session with the sync sessions. So a client that broadcasts on a fresh connection, after a random delay,
via a different network path, gets the same verdict as one that syncs and sends on one connection seconds apart.
(zcash-devtool never opens `GetMempoolStream`, so HIGH can't occur with it at all.)

Proposal: a session-aware rule (e.g. SC-002b) that flags a broadcast when (a) it arrives on a connection that also
synced, or (b) it arrives within N seconds of a session reaching the tip. Our before/after (5 + 5 real regtest
payments through your observer, reports unmodified in our repo) would then show the difference that SC-002 can't.

Also: SC-001 requires every range to equal `chunk_size`, so a grid cell capped at the chain tip is always flagged even
when aligned; an allowance for the tip cell would avoid that false positive.
