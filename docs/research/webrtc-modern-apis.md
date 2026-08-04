# Research: modern WebRTC features and APIs

Status: **survey** — code + spec only. **Nothing here is measured.**

This document answers one question: *are we using the modern WebRTC feature
set?* It answers it by reading the current source and comparing against the
shipped specs. It contains no throughput, latency, or connect-time numbers of
its own, and it does not draw on `docs/research/iroh-webrtc/`, which
[RFC 02](../rfc/02-performance.md) deleted as misleading and forbids citing.
Where a gap that directory once listed is still real, it is re-derived below
from the tree as it stands.

Marking convention follows RFC 02: **[verified]** means read in the code today,
with the citation given. Claims about browser behaviour cite a standard or a
vendor status entry. Anything unmarked is survey-grade and says so.

## 1. The framing that decides every answer

**This is not a WebRTC application.** It is a QUIC-over-datachannel tunnel.

**[verified]** One data channel per peer, label `"iroh"`
(`crates/fofoca-iroh-webrtc-transport/src/lib.rs:74`), negotiated **unreliable
and unordered** on both backends — `ordered: false` with
`Reliability::MaxRetransmits { retransmits: 0 }` on the host
(`src/host/jsep.rs:104-105`) and `set_ordered(false)` /
`set_max_retransmits(0)` in the browser (`src/web/jsep.rs:236-237`). One QUIC
datagram becomes one SCTP message with no application framing; the only
splitting undoes iroh's GSO batching (`src/host/sender.rs:52-57`,
`src/web/transport.rs:615`).

So the stack is:

```
    iroh / QUIC          ← congestion control, loss recovery, encryption (TLS 1.3)
      ↓ datagrams
    SCTP                 ← congestion control, flow control, fragmentation
      ↓
    DTLS                 ← encryption (again)
      ↓
    UDP  ← ICE / STUN
```

The data channel is a **UDP substitute**, not a transport in its own right.
Every verdict below follows from that. It is why a large part of the
modern-WebRTC checklist is not "missing" but *inapplicable*, and why the gaps
that remain cluster almost entirely around **connection establishment** rather
than data transfer.

Establishment is also where the cost is. **[verified]** Every session costs a
JSEP round with a 20 s deadline (`JSEP_DEADLINE`,
`crates/agent-habilis-mesh/src/transport/webrtc.rs:56`) drawn against
`MAX_DIRECT_PEERS = 16` slots (`:451`) shared between the mesh and, per
[RFC 01](../rfc/01-every-peer-a-seeder.md) and
[RFC 03](../rfc/03-fofoca-blobs/README.md), the swarm source set. RFC 03 already
rules out `MOUNT_ALPN` for pre-connect chunk availability *because* of that ICE
cost (`docs/rfc/03-fofoca-blobs/README.md:226-233`). Setup latency is not polish
here; it is the scarcest budget in the system.

## 2. Structurally not applicable

Each of these is a standard item on a modern-WebRTC review. Each has a reason it
does not apply, recorded so the question gets a permanent answer instead of
being re-asked.

| Feature | Why it does not apply |
| --- | --- |
| **Unified Plan vs Plan B** | No media. The SDP carries a single `m=application` (SCTP) section — **[verified]** str0m is driven only through `sdp_api().add_channel_with_config` (`src/host/jsep.rs:102`), and no `MediaStream`, `addTrack`, or `addTransceiver` appears in the crate. Track-to-transceiver mapping has nothing to map. |
| **Perfect negotiation** | Needs renegotiation, which the API forbids by construction: `offer`/`answer` return typestate handles that are consumed on completion (**[verified]** `src/host/jsep.rs:133`, `src/web/jsep.rs:278`), with no path back into `createOffer`. Glare is impossible anyway — **[verified]** the lower `EndpointId` offers and the higher returns early (`crates/agent-habilis-mesh/src/transport/webrtc.rs:489-491`), and a duplicate attach is refused by the registry (`src/registry.rs:71-90`). Adopting the pattern would mean building renegotiation first, for a collision that cannot occur. |
| **Encoded transforms / insertable streams / E2EE** | The payload is already QUIC with TLS 1.3, inside DTLS. A third encryption layer over the same bytes buys nothing. **[verified]** no `RTCRtpScriptTransform` or `createEncodedStreams` anywhere, and none of the RTP `web-sys` features are enabled. |
| **GCC / TWCC / bandwidth estimation** | RTP media mechanisms. Data channels get SCTP's own congestion control instead. The load-bearing version of this concern is §5, and it is a real one. |
| **`bundlePolicy` / `rtcpMuxPolicy`** | One m-section: nothing to bundle, no RTCP to mux. |
| **`iceTransportPolicy: "relay"`** | Meaningless without TURN, and **[verified]** TURN is refused structurally, not merely unconfigured: `accept_ice_uri` accepts only `stun`/`stuns` (`src/ice_uri.rs:49`) and `IceServer` carries no `username`/`credential` fields at all (`src/web/jsep.rs:38-40`), so a credentialed server cannot be expressed. This project relays through its own iroh relay rather than through a second relay at the ICE layer. **Treat as a closed question.** |

One consequence of the last row is worth stating plainly rather than leaving
implicit: **peers behind symmetric NAT never obtain a direct path** and fall back
to the iroh relay. That is a deliberate and defensible trade — one relay instead
of two — but nobody has measured what fraction of real peers it affects, and this
document cannot tell you.

## 3. Already modern — what a reviewer would otherwise flag

- **Unreliable + unordered data channel.** The correct choice for a QUIC
  substrate, and the non-obvious one: the default is reliable-ordered, which
  would put SCTP retransmission and head-of-line blocking *underneath* QUIC's own
  loss recovery. The code says so at the decision point
  (`src/host/jsep.rs:97-98`).
- **`bufferedAmount` gate with drop-on-full as backpressure** (**[verified]**
  `BUFFER_CAP = 1 << 20` at `src/web/transport.rs:34`, checked at `:256`; host
  equivalent drops on a full queue rather than returning `Pending`, with the
  reason recorded at `src/host/sender.rs:59-64`). A review will flag the absence
  of `bufferedAmountLowThreshold` / `onbufferedamountlow`. **That flag is wrong
  here.** Those exist to pace a *reliable, ordered* file transfer whose sender
  must not overrun a receiver. This channel is a datagram substrate: dropping is
  what UDP does, and QUIC above is built to retransmit. Blocking instead of
  dropping would stall iroh's shared send loop for every transport. Recorded so
  the critique has a standing answer.
- **SDP is never munged.** **[verified]** Generated by the stack on both sides —
  `to_sdp_string()` from str0m (`src/host/jsep.rs:114`, `:209`) and read back from
  `local_description()` in the browser (`src/web/jsep.rs:253`, `:365`). The only
  SDP *reading* is a diagnostic candidate count (`src/web/jsep.rs:504-527`). Hand-
  editing SDP is the single most common source of cross-browser breakage.
- **`binaryType = "arraybuffer"` on both channels.** **[verified]** set on the
  created channel (`src/web/jsep.rs:240`) *and* on the inbound one inside
  `ondatachannel` (`:343`). The inbound one is routinely missed, and missing it
  yields `Blob` deliveries and an async read on the hot path.
- **STUN from the socket the data will use.** A NAT maps per source port, so a
  binding discovered on a different socket is the wrong mapping. The host backend
  does this deliberately (`src/host/stun.rs`).
- **No vendor prefixes, no legacy callback APIs.** Promise-based `web-sys`
  throughout.
- **STUN server choice is reasoned, not copy-pasted.** Two servers from two
  *operators*, and `stun1.l.google.com` rather than the bare `stun.l.google.com`
  because blocklists null-route the latter to `0.0.0.0` — worse than NXDOMAIN,
  since the agent waits out a timeout instead of failing fast
  (`src/web/jsep.rs:50-64`).

## 4. Real gaps, ranked

### 4.1 Trickle ICE — absent, and not currently compilable

**[verified]** The browser waits for gathering to complete before the envelope
goes out: `wait_ice_complete` polls `iceGatheringState` every 50 ms against a
10 s deadline (`src/web/jsep.rs:469-483`, constants at `:29-30`). The module
header states the choice: *"Vanilla ICE: candidates ride inside the SDP, so
gathering must complete before the envelope goes out"* (`:7-9`).

This is more than an unused API. **[verified]** `RtcIceCandidate` and
`RtcPeerConnectionIceEvent` are absent from the enabled `web-sys` features
(`crates/fofoca-iroh-webrtc-transport/Cargo.toml:96-117`), so `addIceCandidate`
and `onicecandidate` **cannot be called without a manifest change**. And the
carrier has no room for them: `SignalEnvelope` has exactly three variants —
`Offer`, `Answer`, `Error` (`src/signaling.rs:24-36`) — and the transport is one
envelope each way on a stream that then closes
(`crates/agent-habilis-mesh/src/transport/webrtc.rs:11`). Trickle needs a
candidate variant *and* a carrier that stays open.

Consequence: setup serialises on the slowest STUN round trip for every session,
against the 16-slot budget of §1. Trickle parallelises gathering with
connectivity checks; the specification is
[`draft-ietf-ice-trickle`](https://datatracker.ietf.org/doc/html/draft-ietf-ice-trickle-00),
and browsers make trickle support mandatory, so there is no negotiation risk from
a browser peer.

Worth recording as the cheap middle: **half trickle**, defined in that draft. The
offerer gathers its first generation before offering — exactly what we do now, so
today's answerers keep working — while the answerer trickles. It needs one new
envelope variant and a bidirectional carrier, not a rewrite, and per the draft it
can reach ICE completion nearly as early as full trickle.

**Dependency:** a bidirectional signalling carrier. That is the actual work; the
`web-sys` features are a one-line change.

### 4.2 `connectionState` is never consulted; the code reads legacy `iceConnectionState`

**[verified]** `wait_channel_open` branches on `RtcIceConnectionState::Failed`
(`src/web/jsep.rs:572-577`), and `RtcPeerConnectionState` is not among the
enabled `web-sys` features (`Cargo.toml:96-117`).

`iceConnectionState` reports the ICE agent only. `connectionState` is the
aggregate that also covers DTLS. The gap is concrete: **a DTLS handshake failure
is not an ICE failure.** ICE reaches `connected`, DTLS fails, the `Failed` branch
never fires, and the loop runs to `CHANNEL_OPEN_DEADLINE_MS` = 60 s (`:25`) before
reporting the generic *"data channel never opened"*. `pc.connectionState ===
"failed"` would surface it immediately.

Two stale strings travel with this and should be fixed whenever this area is
touched — both blame TURN, which no longer exists in this codebase (§2):

- **[verified]** `src/web/jsep.rs:575` — *"ICE failed — no route between the peers
  (host/mDNS blocked and TURN did not connect)"*.
- **[verified]** `web/src/App.tsx:284` — *"LAN/mDNS and TURN both failed"* in the
  macOS Local Network hint.

`docs/web-plan.md` is stale in the same direction and cites source paths that no
longer exist.

### 4.3 Everything is polled; no peer-connection event handlers

**[verified]** Two 50 ms polling loops, both on the critical path:
`wait_ice_complete` for up to 10 s (`src/web/jsep.rs:469-483`) and
`wait_channel_open` for up to 60 s (`:566-600`). `POLL_MS = 50` (`:30`).

The modern equivalents are all events — `icegatheringstatechange`,
`connectionstatechange`, and the data channel's `open`. This is not a
capability gap: **[verified]** `Event` is already an enabled feature
(`Cargo.toml:111`), and the crate already installs `onmessage`, `onclose`, and
`onerror` on the channel (`src/web/transport.rs:196`, `:210-227`) and
`ondatachannel` on the peer connection (`src/web/jsep.rs:348`). The in-tree
pattern for a handler plus `spawn_local` exists; the state transitions simply do
not use it.

Cost: up to 50 ms of added latency per transition on a path with at least two
transitions, plus a wasm task woken 20×/s for as long as 70 s per negotiation.

### 4.4 `pc.sctp` / `maxMessageSize` is never read, and the QUIC MTU is left at quinn's default

**[verified]** `RtcSctpTransport` is not an enabled feature (`Cargo.toml:96-117`);
nothing anywhere reads `maxMessageSize`. **[verified]** the transport imposes no
MTU of its own — `poll_send` splits only on `transmit.segment_size` to undo GSO
(`src/host/sender.rs:52-56`, `src/web/transport.rs:614-626`) and an oversized
inbound datagram is silently dropped with UDP semantics
(`src/host/endpoint.rs:80-84`).

So QUIC runs at whatever DPLPMTUD settles on — quinn's default cap is ~1452 B —
over a substrate that is *message*-oriented, reassembles fragments for us, and
advertises its ceiling in the SDP via the `max-message-size` attribute
([RFC 8841](https://datatracker.ietf.org/doc/html/rfc8841); absent means 64 KiB,
`0` means memory-bound). `RTCPeerConnection.sctp.maxMessageSize` exposes that
value and has been available across browsers since May 2023
([MDN](https://developer.mozilla.org/en-US/docs/Web/API/RTCSctpTransport/maxMessageSize)).

The tempting inference is that larger QUIC datagrams would cut per-packet crypto
and ACK overhead. **State it as a hypothesis, because it has a named falsifier:**
with `maxRetransmits: 0`, a jumbo datagram is fragmented into many SCTP fragments
and **any single lost fragment discards the entire QUIC packet**. Loss
amplification grows with datagram size. The prediction is therefore a win on
loopback and LAN and a loss on a lossy WAN, with a crossover nobody has located.
**This must not land on the strength of the reasoning.**

The cheap, risk-free first step is separable from all of that: **read and log
`pc.sctp.maxMessageSize`**, so we learn what each browser actually offers instead
of assuming. That costs one `web-sys` feature and a log line.

### 4.5 Transferable `RTCDataChannel` to a Worker — the most relevant genuinely-new API, unused

`RTCDataChannel` is a transferable object: `worker.postMessage(channel,
[channel])` moves ownership to a worker, after which no further events fire on
the sender's copy. It is specified in
[WebRTC Extensions](https://w3c.github.io/webrtc-extensions/), not yet in
WebRTC-PC.

Support is **asymmetric**, which is what makes this "prototype and watch" rather
than "adopt": WebKit implemented and shipped it first, and Chromium's own Intent
to Prototype cites that prior shipping as the reason interoperability risk is low
([blink-dev intent](https://groups.google.com/a/chromium.org/g/blink-dev/c/64yIg0Ya3No)).
Firefox tracks it at
[bug 1209163](https://bugzilla.mozilla.org/show_bug.cgi?id=1209163).

Relevant here because the whole iroh/QUIC stack currently runs on the main
thread (`web/src/wasm.ts`), so send/receive contends with rendering and with GC
pauses — and because a worker story already exists in this project
(`docs/rfc/03-fofoca-blobs/findings/s05-opfs-worker.md`). Those two would compose:
a worker owning both the data channel and OPFS writes could take the byte path off
the main thread end to end.

### 4.6 `iceCandidatePoolSize` is never set

**[verified]** `to_configuration` sets `iceServers` and nothing else
(`src/web/jsep.rs:75-96`). `iceCandidatePoolSize` makes the browser gather at
configuration time rather than at `setLocalDescription` — which is precisely the
wait that vanilla ICE (§4.1) serialises on, and the cheapest partial mitigation
available without touching the signalling carrier.

Caveat to record with it: pooling costs a STUN binding per interface per pooled
candidate **whether or not a session ever happens**, and this code already
reasons carefully about srflx cost per interface — it declines to add
`stun2/3/4` partly because *"every extra server costs a srflx candidate per local
interface"* (`src/web/jsep.rs:59-61`). Pre-warm only where a dial is likely, not
unconditionally.

### 4.7 No ICE restart on either side

**[verified]** `restartIce()` never appears; no str0m ICE restart is invoked. A
dead session is removed, not restarted (`src/host/driver.rs:262-275`), and
recovery is a **full JSEP round** re-run by `retry_sessions`
(`crates/agent-habilis-mesh/src/transport/webrtc.rs:757`) — another 20 s deadline
and another admission slot.

`restartIce()` re-gathers on the *existing* peer connection and keeps the data
channel, which is the standard answer to a network change (Wi-Fi → cellular).
**Honest dependency:** it triggers renegotiation, which the one-shot typestate
forbids (§2). It is gated behind the same work as trickle ICE, and should not be
costed as an independent item.

Note the mesh does not depend on this for correctness: iroh keeps the relay path
demoted rather than closed (`src/selector.rs:22-26`), so a lost WebRTC session
degrades to relay rather than dropping the peer.

### 4.8 `getStats()` is read for display only, never fed back

**[verified]** The browser side calls `get_stats()` (`src/web/transport.rs:412`)
and `selected_pair_stats` already returns `currentRoundTripTime` (`:339-357`),
which the client differences into `rtt_ms` (`agent-share-wasm-client/src/lib.rs:82-115`)
purely for the UI.

Meanwhile `selector.rs` exists to work around iroh's default
`BiasedRttPathSelector`, which *"skips any path"* lacking an RTT sample, by
treating an unmeasured path as a live candidate (`src/selector.rs:15-20`, `:85`).
**The RTT the selector lacks is being read one layer below and thrown away after
rendering.** Whether feeding it back is worth the coupling is a design question
this document does not answer — but the data is already in hand, which is not
obvious from either file alone.

Also unread from the report:

- `candidate-pair.availableOutgoingBitrate` — the transport's own view of
  capacity.
- `data-channel` stats (`messagesSent` / `bytesSent`) — these would separate
  application bytes from wire bytes. The UI currently annotates `bytes_sent` as
  *"Wire bytes on the selected ICE pair — includes SCTP/DTLS/STUN framing"*
  (`web/src/TechInfo.tsx:41-42`) precisely because it cannot make that split
  today.

**[verified]** The host side calls `getStats` not at all: `pump_outputs` matches
`ChannelData`, `ChannelClose`, and `IceConnectionStateChange(Disconnected)`, and
discards every other str0m event (`src/host/driver.rs:288-310`). Note the
narrowness of that last arm — only `Disconnected` ends a session; other ICE state
transitions fall into the catch-all.

### 4.9 `canTrickleIceCandidates` is never read

**[verified]** absent from the crate. A consequence of §4.1, listed for
completeness; it only becomes meaningful if trickle lands.

## 5. The cross-cutting finding: two congestion controllers in series

This is the load-bearing version of *"are we using TWCC?"*, and the most
important item in this document.

QUIC-inside-SCTP places **two independent congestion controllers in series**.
SCTP's is TCP-like — slow start, congestion avoidance, fast retransmit — and it
additionally imposes a per-stream *and* a per-association flow-control window
that the inner QUIC cannot observe. QUIC runs its own congestion control above
and cannot distinguish queuing in the substrate from congestion in the network.

**`maxRetransmits: 0` removes SCTP retransmission. It does not remove SCTP
congestion control or flow control.** That distinction matters, because the
retransmission behaviour is the part this project already reasoned about and
fixed; the windowing part is untouched and unexamined.

There is empirical work on nested WebRTC/QUIC congestion control finding the
interaction to be regime-dependent rather than uniformly bad — the outer layer
hiding losses from the inner one helps at low latency and hurts at high
([Assessing the Interplay between WebRTC and QUIC Congestion Control
Algorithms](https://cnrs.hal.science/I3S/hal-04231048v1)). It studies QUIC *under*
WebRTC media rather than QUIC *inside* a data channel, so it is suggestive, not
transferable.

**This connects to the one unresolved measurement in the tree.**
[S04](../rfc/03-fofoca-blobs/findings/s04-multi-source-throughput.md) records
K=2 → 1.61× but K=4 → 0.61×, verdict *"holds at K=2 as a floor; K=4 unresolved
and gating"* (`:3`, `:25-26`). Nested congestion control across four associations
sharing one path is a **candidate mechanism** for that collapse.

**Named falsifier, and it is a strong one:** S04 itself names CPU contention at
~29 MiB/s per connection as the likely cause (`:82-84`), and all producers share
one host and one CPU. CPU saturation predicts the same shape. **The two must be
separated before either is believed.** Offer this as a hypothesis to test — it
must not inherit authority from the fact that it superficially resembles the
retracted 128-KiB-window claim that RFC 02 already had to walk back.

## 6. Horizon: what would replace this stack

Kept short deliberately, so this document ages rather than rots.

The ecosystem's answer to the nesting in §5 has been to **remove** it, not tune
it. The IETF work is
[QUIC Data Channels](https://www.ietf.org/archive/id/draft-engelbart-quic-data-channels-00.html),
which specifies data channels directly over a QUIC connection so that encryption
context, congestion control, and prioritisation are shared rather than
duplicated.

Neither it nor WebTransport is browser-P2P-capable today. The blocker is
structural: QUIC has no ICE/STUN story, and its packet format conflicts with
STUN's, which is why the original WebRTC data channel is SCTP-based at all.
WebTransport with `serverCertificateHashes` is client-server and therefore cannot
replace this lane — it is worth watching only as a possible upgrade for a
browser↔relay hop, not for the peer↔peer one.

**Track. Do not plan around.**

## 7. Summary

| Feature | Verdict | Note |
| --- | --- | --- |
| Unreliable + unordered data channel | **done** | Correct for a QUIC substrate |
| Unmunged, stack-generated SDP | **done** | Both backends |
| `binaryType = "arraybuffer"` | **done** | Including the inbound channel |
| `bufferedAmount` backpressure | **done** | Drop-on-full is right here; `bufferedAmountLowThreshold` is the wrong tool |
| Unified Plan | **N/A** | No media |
| Perfect negotiation / rollback | **N/A** | No renegotiation; glare impossible by construction |
| Encoded transforms / E2EE | **N/A** | Already QUIC-in-DTLS |
| GCC / TWCC | **N/A** | RTP-only; see §5 for the real question |
| `bundlePolicy` / `rtcpMuxPolicy` | **N/A** | One m-section |
| `iceTransportPolicy: "relay"` | **N/A** | TURN refused structurally — closed question |
| Trickle ICE | **gap 1** | Needs a bidirectional carrier + envelope variant + `web-sys` features |
| `connectionState` | **gap 2** | DTLS failure currently costs the full 60 s deadline |
| Event-driven state | **gap 3** | Two 50 ms polling loops on the critical path |
| `pc.sctp.maxMessageSize` | **gap 4** | Read-and-log is free; larger MTU is a hypothesis with a falsifier |
| Transferable datachannel → Worker | **gap 5** | WebKit shipped, Chromium prototyping — watch |
| `iceCandidatePoolSize` | **gap 6** | Cheapest partial mitigation for gap 1 |
| `restartIce()` | **gap 7** | Blocked on renegotiation, same as gap 1 |
| `getStats()` feedback | **gap 8** | RTT already read and discarded |
| `canTrickleIceCandidates` | **gap 9** | Only meaningful once gap 1 lands |

Two dependencies dominate. **Gaps 1, 7, and 9 all reduce to one piece of work:
a signalling carrier that stays open, plus renegotiation.** Gaps 2, 3, 4, and 6
are independent and individually small. Gap 5 is a prototype gated on Chromium.

## Follow-ups noted but not done here

- Stale TURN references: `src/web/jsep.rs:575`, `web/src/App.tsx:284`.
- `docs/web-plan.md` cites source paths that no longer exist and a "no relay data
  fallback" design that `TransportMode::Dynamic` superseded.
- No headless-browser test exists anywhere; the browser backend is validated by
  hand against real Chrome via `web/lab/index.html`, as recorded in
  `crates/agent-habilis-mesh/tests/wasm_runtime.rs:34-43`. Every automated WebRTC
  test is native str0m↔str0m on loopback with host-only candidates. Several gaps
  above are browser-only and therefore cannot regress a test today.
