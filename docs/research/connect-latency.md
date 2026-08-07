# Research: where browser connect time goes, and how to get it back

Status: **measured + verified** — every constant cited was read in the tree
(agent-share working tree, fofoca at rev `e63a481`), and the headline numbers
come from the bench harness (`docs/perf/policy-browser.md`). Marking follows
RFC 02: **[verified]** means read in the code today, with the citation given.

The question: a browser peer takes ~30 s (sometimes more) to reach a share.
Where is the time, and what is the cheapest ordering of fixes?

## 1. The two regimes

There are two different "~30 s" experiences, and they have different causes:

- **Healthy origin, slow connect (~10–20 s).** The transport itself is slow.
  Measured: `browser-consume-webrtc` connect = **10,238 ms** vs
  `browser-consume-relay` = **123 ms** (`docs/perf/policy-browser.md:19-20`).
  The 10.1 s delta equals fofoca's `ICE_GATHERING_DEADLINE_MS = 10_000`
  **[verified]** (`fofoca-iroh-webrtc-transport/src/web/jsep.rs:32`). The
  `ORIGIN_DIAL_CAP_MS` doc comment records a live local dial at ~20 s
  **[verified]** (`crates/agent-share-wasm-client/src/lib.rs:1636-1647`).
- **Dead or unreachable origin ("sometimes more").** The client waits out the
  full `ORIGIN_DIAL_CAP_MS = 30_000` **[verified]** (`lib.rs:1648`) before the
  seeder fallback even starts, then polls gossip cards for up to
  `SEEDER_CARDS_DEADLINE_MS = 45_000` (`lib.rs:1512`).

## 2. The happy-path budget, itemized

Order of the critical path in `ShareClient::connect`
(`crates/agent-share-wasm-client/src/lib.rs:248`), with what each step costs:

| Step | Cost | Cause | Cite |
|---|---|---|---|
| wasm fetch + compile | ~0.3–3 s | 7.3 MB, no compression, fetched only after `<Session>` mounts | `web/src/wasm-asset.ts:111-119`, `web/src/App.tsx:175` |
| signal endpoint bind, then mount endpoint bind | ~0.1–1 s | sequential awaits, independent work | `lib.rs:2221-2241` |
| signal-ALPN dial over relay, **then** offer | ~0.3–1.5 s | serial although independent — offer takes only `local` and `ice` | `lib.rs:2394-2409` |
| **browser ICE gathering** | **~10 s** | `wait_ice_complete` waits for `iceGatheringState === 'complete'`; Chromium reports it only after *every* STUN server answers or exhausts its ~9.5 s retransmission ladder | `web/jsep.rs:473-487` |
| producer's answer | up to 4 s | 2 STUN servers probed **sequentially**, 2 s timeout each | `native/stun.rs:222-244` |
| data channel + QUIC-over-DC dial | ~0.3–1 s | genuine handshakes, no artificial waits found | `web/jsep.rs:408-423` |
| `settled_path_label` | 0.05–3 s | 3 s settle poll; only consumer is a UI label / `fallback_reason` | `lib.rs:2028, 2275` |
| `join_share` awaited inline | ~0.2–2 s | mesh setup (relay work) on the connect path despite being "strictly additive" | `lib.rs:335-342`, comment `:301-304` |
| `manifest()` after connect | 1 RTT | separate round trip before `state: ready` | `web/src/App.tsx:782` |

Both the browser **offerer** (`web/jsep.rs:255`) and a browser **answerer**
(`web/jsep.rs:367`, the mesh lane) pay the full ICE gathering wait — which is
why `SIGNAL_EXCHANGE_DEADLINE` is 20 s (`fofoca/src/transport/webrtc.rs:73`).

Why gathering hits the deadline at all: two STUN servers are configured from
two operators (`stun1.l.google.com`, `stun.cloudflare.com` — `web/jsep.rs:64-67`).
If either is unreachable on the current network (VPN, blocklist, firewalled
UDP), `complete` never fires early; meanwhile usable host + srflx candidates
from the healthy server arrived in the first few hundred ms. The browser then
proceeds with exactly the candidates it had at ~0.3 s — after paying 10 s.

## 3. The fix set (implemented 2026-08-06; fofoca half at rev `deac009`)

Ordered by leverage. None of the transport fixes change the wire format:
fewer `a=candidate` lines in a vanilla-ICE SDP is legal, so browser wasm and
native serve roll independently.

1. **ICE early-exit** (fofoca `web/jsep.rs`): exit `wait_ice_complete` once
   `srflx ≥ 1` AND `host+mdns ≥ 1` AND no new candidate for 400 ms; keep the
   10 s ceiling for the no-candidate case; waive the srflx requirement when the
   config has no ICE servers (`host_only()` — srflx can never appear). Chrome
   mDNS-obfuscates host candidates, so "host present" counts `host + mdns`.
   Since there is no TURN by policy, srflx is the only NAT rung — requiring one
   before early exit keeps the failure set identical; only the candidate count
   on partially-blocked networks changes. Saves ~9.5 s whenever any STUN server
   is slow or blocked; saves ~0 (correctly) when gathering completes fast.
2. **Parallel STUN on the native answer side** (fofoca `native/stun.rs`):
   send-all / single-recv-loop with a joint 2 s deadline. Both probes must
   share the one media socket, so this is *not* two racing `recv_from` futures
   (they would steal each other's datagrams) — one loop matching transaction
   ids. Worst case 4 s → 2 s; dead-first-server case ~2 s → ~50 ms.
3. **Parallelism in `connect_webrtc`** (wasm client): bind the two endpoints
   concurrently; run the signal-ALPN dial concurrently with offer creation.
4. **Off the critical path** (wasm client): `settled_path_label` and
   `join_share` move to one spawned task (settle → label → card → join), with
   a tri-state mesh slot so a client released mid-join still leaves cleanly.
   `data_path` is provisionally `"webrtc"` in dynamic mode — the mount
   endpoint is relay-free and IP-free, there is nothing else to settle on.
5. **Manifest prefetch** (wasm client + web): the seeder path already fetches
   and vets the manifest, then throws it away; keep it. The origin path
   prefetches it in the background as soon as the connection exists.
6. **Wasm delivery** (web): eager `loadWasm()` at module import and
   brotli/gzip negotiation while keeping `content-type: application/wasm` so
   `instantiateStreaming` engages. 7.3 MB → 1.8 MB. A `<link rel="preload">`
   was tried and removed: Safari does not match an `as="fetch"` preload to
   the glue's `fetch()` (measured — duplicate resource-timing entries), so a
   cold cache downloaded the binary twice, and the eager load starts within
   ~25 ms of the preload anyway.
7. **Race, don't sequence, the seeder fallback** (wasm client): in dynamic
   mode, start `connect_via_seeder` at T+3 s while the origin dial continues;
   `select_biased` preferring origin. Dead-origin connects stop paying the
   30 s wall. `WAITING_MESHES` (persistent per-share membership) makes the
   racing membership cheap and non-ghosting: a winning origin `leave()`s it.
   `ORIGIN_DIAL_CAP_MS` itself is *not* lowered until the healthy dial is
   re-measured — a 15 s cap once severed a healthy ~20 s dial mid-handshake
   (`lib.rs:1636-1647`).

Expected end state: transport connect ~0.8–1.5 s (123 ms relay signal hop +
≤700 ms gathering + ~50 ms native STUN + real handshakes); page-load to ready
~2–3 s on a healthy origin; dead-origin fallback bounded by the 3 s head start
plus card collection instead of the 30 s wall.

## 4. Rejected alternatives, and why

- **True trickle ICE.** Technically feasible: str0m exposes
  `add_remote_candidate` at any time, and the browser has `addIceCandidate`.
  But the JSEP envelope is one tagged-JSON message each way over a stream both
  sides close after one envelope, in **three** carriers (fofoca mesh lane,
  agent-share mount lane, wasm client), and serde's tagged enums reject
  unknown variants — so trickle needs capability negotiation plus stream
  re-framing across two repos, with a rollout matrix where every version pair
  must interop. Its ceiling over early-exit is a few hundred ms (the quiet
  period plus one STUN RTT). Not worth it; revisit only if measurement after
  the fix set shows gathering still dominating.
- **Parallel relay-rung walk.** Verified non-blocking on the connect path:
  startup takes rung 0 optimistically unprobed and confirmation runs detached
  (`fofoca/src/daemon/setup.rs:87,106`). The sequential 10 s/rung walk only
  hurts when rung 0 is actually down — a degraded-bootstrap concern, not a
  connect-latency one. If ever done, keep determinism by selecting the
  *lowest-index* reachable rung, not the first to answer.
- **Shrinking `SIGNAL_EXCHANGE_DEADLINE` / `JSEP_DEADLINE` (20 s each).**
  These bound the *peer's* behavior; shrinking them while old peers still take
  10 s to answer manufactures timeouts. Revisit a release after both sides in
  the field carry the early-exit.
- **Piggybacking the manifest on the mount handshake.** `OP_MANIFEST` is a
  plain request/response (`agent-share-proto/src/framing.rs:37,133-141`);
  changing the ALPN handshake is a protocol rev for one RTT. Prefetch gets the
  same latency without one.

## 5. Deferred

- **Endpoint identity reuse across reconnect attempts** (todo.md): fixes
  roster ghosts and double-counting, but `negotiate` handles the producer's
  "already holds a session" refusal only via the *local* hub
  (`lib.rs:2431-2440`) — reusing an id the producer still holds a dead session
  for would turn the refusal into a hard connect error. Blocked on verifying
  the native producer replaces a stale session for a returning id.
- **Dead-beacon failover** (todo.md): survivors re-probe a darkened rendezvous
  on `RIVAL_RECHECK_MESHED_SECS = 300` plus up to 300 s jitter — the 1–10 min
  failover window. A fofoca-side event-driven beacon failover is the real fix;
  the racing seeder fallback above only softens the client-side experience.
