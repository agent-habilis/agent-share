# agent-share on the web

## Context

`agent-share` today is CLI-only: `agent-share serve <dir>` prints a ticket, and
`agent-share <ticket> <mnt>` mounts it via a loopback NFSv3 bridge. Both ends need the binary.

We want a browser to be a first-class consumer. User A shares a folder from the CLI and
sends a link; user B opens it, browses the tree in a macOS-Finder-style column view, and
downloads the whole folder — with no install. Every byte of file data crosses a true
peer-to-peer WebRTC data channel; the relay is a rendezvous for the SDP exchange and
carries no data.

This plan was cross-checked by two independent researchers over agent-gossip
(`<clasp-engulf>`, `<filter-scope>`); their reports are at
`/tmp/agent-share-research-clasp-engulf.md` and `/tmp/agent-share-research-filter-scope.md`.
Every fact below was verified by reading files or running commands — not inferred.

### Verified findings

**The rename and the ALPN fork are staged but NOT committed.** This is the most important
finding and it invalidates anything read from `HEAD`:

```
git status --short          → 21 index-modified files
git show HEAD:Cargo.toml    → name = "ahmo"  (:12, :77, :81)
git show HEAD:src/mount/mod.rs:13 → b"agent-habilis-swarm/mount/1"
grep -n "MOUNT_ALPN: " src/mount/mod.rs:18 → b"agent-share/mount/1"
```

- Working tree: package `agent-share`, lib `agent_share`, bin `agent-share`
  (`Cargo.toml:12,77,81`). ALPN `agent-share/mount/1` (`src/mount/mod.rs:18`), pinned by
  `wire_constants_are_pinned` (`src/mount/mod.rs:157-168`).
- Protocol: header `secret[32] ‖ op[1]`; `OP_MANIFEST=1`, `OP_READ=2`;
  `MAX_MANIFEST_BYTES` 64 MiB, `MAX_READ_LEN` 256 KiB (`src/mount/mod.rs:21-41`).
- Manifest wire (LE): `dir_count(u32)[len(u16)‖path‖mode(u32)‖mtime(i64)]… file_count(u32)…`
  (`src/mount/wire.rs:31-33`). A file's **index is its READ address**; no content hashes,
  deliberately (`src/mount/wire.rs:14-16`).
- Ticket: `secret(32) ‖ flags(1) ‖ lookups ‖ addr-json` as a Base58Check token, Mount =
  type byte 5 (`src/mount/ticket.rs:11-14`, `src/protocol/token.rs:44-46`). Self-contained —
  a browser can dial from the ticket alone.
- `RemoteClient` (`src/mount/consume.rs:105-112`) implements `ByteSource`
  (`consume.rs:233`), and the NFS layer reads only through that trait (`src/mount/nfs.rs:18`).
  **That is already the seam** the shared-crate work needs.

**The WebRTC experiment has zero NAT traversal.** `rg -in 'stun[:.]|turn:|iceServers|ice_servers|RtcIceServer'`
over the whole worktree returns no hits in WebRTC source. `driver.rs:62` adds exactly one
`Candidate::host`, advertising a LAN interface address (`driver.rs:45-59`);
`webrtc-browser/src/lib.rs:81` calls bare `RtcPeerConnection::new()`. Same-LAN only.
Its HTTP signaling binds `127.0.0.1` (`src/webrtc/mod.rs:63`) — unreachable across the
internet.

**`SignalEnvelope` and the transport id are already duplicated** between
`fofoca-iroh-webrtc-transport/src/signaling.rs:24-42` and `webrtc-browser/src/lib.rs:26-46`, with a
"keep in lockstep" comment. That is the drift hazard requirement 8 exists to kill.

**[str0m does no candidate gathering](https://docs.rs/str0m/latest/str0m/) and has no TURN
client.** It exposes `Candidate::server_reflexive(addr, base, proto)` and
`Candidate::relayed(...)`, but the STUN/TURN I/O is yours to write.

**[iroh's browser support is relay-only](https://www.iroh.computer/docs/wasm-browser-support)** —
browsers can't send UDP, so hole-punching is impossible from a tab. Custom transports
(iroh 0.97+, unstable) are the supported hook for WebRTC. Traffic stays E2E encrypted, so a
relay sees ciphertext.

**`relay.agent-habilis.com` is a live iroh relay** — `GET /` → 200, `/generate_204` → 204
(iroh's latency probe), `/relay` → `400 missing header: upgrade` (the WebSocket path).

**iroh pins diverge.** agent-share: `1.0.0-rc.0` rev `67cd78c9` (`Cargo.toml:89,133`).
agent-gossip + both WebRTC crates: `1.0.1` rev `195cb98d`. `CustomTransport` is an `iroh`
trait, so two `iroh` crates in one graph means the impl doesn't satisfy it. agent-share also
pins forked `noq`/`netdev` (`Cargo.toml:139-144`) that agent-gossip may not carry.
agent-share does **not** enable `unstable-custom-transports` (`Cargo.toml:89`).

**`src/webrtc/signal.rs:116-123`**: iroh only fans a connect's Initial out to candidate paths
**while the remote has no selected path**. A live relay connection cannot be upgraded to
WebRTC in place without an upstream iroh change.

**moonspace-ui**: `private: true`, `v0.0.0`, ships raw TS (`main: ./src/index.ts`), no build
step. `bun run typecheck` passes (run by both researchers). No Tree/List/ScrollArea/column
component — usable primitives are `Box`, `Stack`, `Text`, `Table`, `MiddleTruncate`,
`snapWidth`, and `theme/glyphs.ts`. Font stack leads with proprietary TX-02 and **ships no
font files** — the `@font-face` block in `src/styles/GlobalStyle.ts` is commented out.

> **Superseded:** moonspace-ui and visage (`visage-dom` / `visage-style` / `visage-router`)
> are now vendored under `web/vendor/`. The UI is rewritten on visage (no React /
> styled-components); see `web/vendor/moonspace-ui/src/global.css` for the former GlobalStyle.

## Architecture

**Use the iroh relay for signaling, WebRTC for data, two separate connections.** This
sidesteps the in-place-upgrade limitation, needs no bespoke rendezvous server, and makes
signaling authenticated and E2E-encrypted by iroh rather than a bearer-token mailbox.

```
Browser                                             CLI producer
  │ 1. decode the ticket from the URL                     │
  │    → EndpointAddr, 32-byte secret, relay ladder       │
  │ 2. iroh wasm endpoint, relay transport ────────────── relay.agent-habilis.com
  │    dial ALPN agent-share/webrtc-signal/1              │
  │    ── SDP offer ──────────────────────────────────►   │  str0m: host + srflx
  │    ◄──────────────────────────────── SDP answer ──    │  (STUN) candidates
  │    close  (short-lived, signaling only)               │
  │ 3. WebRTC data channel opens (direct, p2p) ═══════════╡
  │ 4. FRESH dial: ALPN agent-share/mount/1 over an       │
  │    EndpointAddr containing ONLY the WebRTC addr ──►   │
  │    → no selected path → Initial fans out             │
  │    OP_MANIFEST once, then OP_READ per chunk           │
  │    (relay carries SDP only — never a byte of file data)│
```

Step 4 is exactly the case that works, and is what
`fofoca-iroh-webrtc-transport/tests/loopback.rs` and `tests/webrtc.rs::swarm_runs_entirely_over_webrtc`
already prove.

**The relay is a rendezvous, not a transport.** It carries the SDP exchange and nothing
else — no file bytes ever cross it. That keeps the data path true peer-to-peer, and means
the relay operator sees only that two endpoint ids met, never how much they moved or for
how long.

The cost is that there is **no data fallback**. When ICE fails, the transfer fails; there is
no second path to fall back to. That makes STUN load-bearing rather than an optimisation,
and it leaves symmetric-NAT-to-symmetric-NAT pairs unreachable — the gap TURN exists to
close. The UI must say so plainly instead of hanging.

## Decisions

| Question | Decision |
|---|---|
| Transport | WebRTC for all data; iroh relay as signaling carrier only |
| URL | `share.agent-habilis.com/#<ticket>` — fragment, so the secret never reaches the server |
| Listing | Full manifest one-shot; no new op |
| Design system | `moonspace-ui` local path dep; column view built in `agent-share` *(superseded: vendored under `web/vendor/`, UI on visage)* |
| NAT fallback | STUN both sides. No TURN, and **no relay data fallback** — relay is rendezvous only |
| Web sharing | On by default; `--no-web` opts out |
| Relay default | `relay.agent-habilis.com` **prepended to the ladder**, n0 defaults retained |
| Code sharing | Shared crates; one signal/proto core, two thin backends |

## Workspace layout

As built. The root is a **virtual workspace**; every folder name is its crate name.

```
Cargo.toml    members = ["crates/*", "tasks"]
              resolver = "3"                          ← see note below
              default-members = ["crates/agent-share"]
              exclude = ["crates/agent-share-wasm-client"]

crates/agent-share/               the CLI: producer, NFS consumer, WebRTC lane
crates/agent-share-proto/         wire format — ticket, manifest, framing (wasm-safe)
crates/agent-share-wasm-client/   wasm cdylib, own [workspace], NOT a member
crates/agent-habilis-mesh/        vendored gossip engine, `host` feature gates wasm
crates/fofoca-iroh-webrtc-transport/          one crate: core + `host` (str0m) + `web` (web-sys)
crates/iroh-multihop-transport/   vendored with mesh
tasks/                            cargo task runner
web/                              React + Vite + Bun SPA
node/                             npx agent-share <ticket> receiver
```

Two keys on the root manifest are load-bearing and easy to lose:

- **`resolver = "3"`** — a root package on edition 2024 implies it, but a *virtual*
  workspace inherits nothing from its members and silently falls back to the edition-2015
  resolver 1, changing feature unification across the whole graph.
- **`exclude`** — `crates/agent-share-wasm-client` matches the `crates/*` glob but must
  not be a member: it is a wasm32-only cdylib (`web_sys::window()` does not exist off
  wasm32), so `cargo clippy --workspace --all-targets` and `cargo test --workspace`,
  which `cargo task ci` runs, would fail on it.

Being excluded, it keeps its own `[workspace]` and therefore its own duplicated
`[patch.crates-io]` — patches are not inherited across a workspace boundary. Path deps
*do* cross, so it still links the very same `agent-share-proto` and `fofoca-iroh-webrtc-transport`
the CLI does.

The single `.wasm` it produces feeds both front ends, differing only in wasm-bindgen glue:
`dist/web/` for the `web/` SPA, `dist/nodejs/` for `node/`.

### What can honestly be shared, and what cannot

"One crate for both" is not literally achievable, and planning around it would fail at the
first `cargo build`. str0m and tokio don't target `wasm32`; `web-sys` doesn't exist off
browser. Both researchers reached this independently.

**Shared** (the crate root of `fofoca-iroh-webrtc-transport`, always compiled, deps: `iroh-base` + serde only):
`WEBRTC_TRANSPORT_ID` / `custom_addr()`, `SignalEnvelope` / `SIGNAL_VERSION` /
`MAX_ENVELOPE_BYTES`, `DATA_CHANNEL_LABEL`, the signal ALPN and one-envelope-each-way
contract, and the offer/answer sequencing state machine.

**Not shared**: SDP generation and ICE/DTLS/SCTP (str0m sans-io + `tokio::net::UdpSocket`,
`driver.rs:74-145`, vs the browser's built-in stack); the datagram pump
(`tokio::select!` + mpsc, `driver.rs:179-213`, vs `spawn_local` + `buffered_amount`
backpressure, `webrtc-browser/src/lib.rs:155-162`).

The backend trait must be `#[async_trait(?Send)]` — wasm futures are `!Send`, and a `Send`
bound would force two signatures and defeat the sharing.

### One WebRTC path, two consumers

| | CLI producer | CLI consumer | Browser |
|---|---|---|---|
| Signal envelope + addr | `fofoca-iroh-webrtc-transport` root | `fofoca-iroh-webrtc-transport` root | `fofoca-iroh-webrtc-transport` root |
| Mount wire format | `proto` | `proto` | `proto` |
| WebRTC backend | `fofoca-iroh-webrtc-transport` (`host`) | `fofoca-iroh-webrtc-transport` (`host`) | `fofoca-iroh-webrtc-transport` (`web`) |

`agent-share <ticket> <mnt>` gains WebRTC by linking the same `fofoca-iroh-webrtc-transport` (`host`) the producer
does. Since `ByteSource` is already the seam, neither the NFS layer nor the mount logic
changes.

**Does WebRTC subsume IP/relay for CLI-to-CLI? No — keep IP/relay as the CLI default.**
Both researchers agreed independently. Without STUN, WebRTC traverses *less* NAT than iroh,
which already hole-punches; iroh's lookup layer already solves CLI discovery; DTLS+SCTP
under QUIC stacks two congestion controllers; and the experiment's own authors treat it as
additive — `ExclusivePreset` is documented as "for tests and single-purpose peers like the
browser demo" (`transport.rs:91-94,129-143`). WebRTC is the *browser* transport plus an
opportunistic upgrade, not a replacement.

## Phases

### Phase 0 — unblock (prerequisite, gates everything) ✅ DONE

1. ~~Commit the staged rename and ALPN fork.~~ Landed during the session as `0807c95`
   ("feat: rename to agent-share and fork the mount ALPN from ahsw"). Tree is clean.
2. ~~Reconcile iroh~~ — bumped to `1.0.1` + rev `195cb98d…`, added
   `unstable-custom-transports`, and bumped `iroh-{mdns,mainline}-address-lookup` 0.2 → 0.4.
3. ~~Verify the forked `noq`/`netdev` pins.~~ **Both dropped, not ported.** agent-gossip's
   `Cargo.toml:319-323` records that noq v1.0.1 bounds `abandoned_paths` upstream via
   `ArrayRangeSet` (noq#691), and the netdev CoreWLAN autorelease-leak fix merged upstream
   as netdev#165 (shipped 0.44.0, pulled as 0.45.x via netwatch 0.19.1). This was risk 1;
   it did not materialise.
4. ~~`cargo test`~~ — **67 passed, 1 ignored.** All three golden pins green, unmodified.

### Phase 1 — extract shared crates

`crates/agent-share-proto`, moving not rewriting: `src/mount/wire.rs`,
`src/mount/ticket.rs`, `src/protocol/{token,peer_addr}.rs`, `src/protocol/swarm/lookup.rs`,
and the ALPN/op constants from `src/mount/mod.rs:18-41`. Everything is `pub(crate)` /
`pub(super)` today, so this is a visibility-and-move exercise.

Add the framing helpers currently inline in `produce.rs:164-193` and `consume.rs:159-229`:

```rust
pub fn encode_manifest_request(secret: &[u8; 32]) -> Vec<u8>;
pub fn encode_read_request(secret: &[u8; 32], index: u32, offset: u64, len: u32) -> Vec<u8>;
pub fn decode_read_response(bytes: &[u8]) -> Result<&[u8]>;
```

`MountTicket` embeds `iroh::EndpointAddr` (`ticket.rs:16`); accept a `iroh-base` dep with
`default-features = false`, which is wasm-safe (`webrtc-browser/Cargo.toml:22` does exactly
this). No tokio, no `nfsserve`.

**The three golden tests must pass unmodified.** Add
`cargo check --target wasm32-unknown-unknown -p agent-share-proto` to CI as the guard
against someone re-adding a host-only dep.

### Phase 2 — split the WebRTC crates and add NAT traversal

Split `agent-gossip/webrtc/webrtc-transport/`: `addr.rs` + `signaling.rs` →
the crate root; the backends behind `host`/`web` features. Delete the duplicated
`SignalEnvelope` and `WEBRTC_TRANSPORT_ID` from the browser crate and depend on
the shared root instead — requirement 8 paying for itself immediately.

Then close the LAN-only gap:

1. **STUN on the host side.** In `driver.rs`, after `bind_ephemeral_udp()`, send an RFC 5389
   Binding Request from that same socket (the NAT mapping must match), parse
   `XOR-MAPPED-ADDRESS`, and add `Candidate::server_reflexive(mapped, base, "udp")`
   alongside the host candidate. ~120 lines, no new dependency, server list configurable.
2. **ICE servers in the browser.** Replace `RtcPeerConnection::new()`
   (`webrtc-browser/src/lib.rs:81`) with `new_with_configuration(&config)` carrying
   `iceServers`. The existing gathering-complete wait (`lib.rs:99-112`, `:261`) already
   handles the added latency.

### Phase 3 — CLI producer

- `produce.rs::bind` (`:109`) registers a second ALPN, `agent-share/webrtc-signal/1`.
- Signal handler modelled on `agent-gossip/webrtc/src/webrtc/signal.rs`: read one
  `Offer`, `answer(...)`, write the `Answer`, `attach()`. Key on
  `connection.remote_id()`, ignoring the envelope's claimed id — the iroh connection
  already authenticates it.
- WebRTC transport added *additively*, leaving CLI-to-CLI untouched.
- `announce()` (`src/mount/mod.rs:68`) prints a third line: the
  `https://share.agent-habilis.com/#<ticket>` URL. Tickets are bare ASCII
  Base58, so nothing needs percent-encoding.
- `--no-web` on `MountAction::Serve` (`src/cli/args/mount.rs`).
- `src/lookup/relay.rs:27` — `RelayChoice::Pinned` becomes a **ladder** with
  `relay.agent-habilis.com` first and iroh's defaults retained. **Not a single pin**: the
  comment at `relay.rs:12-16` records that pinning one relay made `bind()` block on that
  relay's handshake and dropped iroh's fallback.

> Wire format is unchanged (`Pinned` still encodes `0b0100`,
> `src/protocol/swarm/lookup.rs:90`), so no golden test breaks — but the *meaning* shifts,
> so a ticket from an older build points its consumer at a different ladder. Note it in the
> README.

### Phase 4 — browser wasm client

`crates/agent-share-wasm-client/`, from `webrtc-browser/` but with a real API:

```rust
#[wasm_bindgen]
impl ShareClient {
    /// `transport`: `webrtc` | `relay` | `dynamic` (omit ⇒ dynamic:
    /// both on, WebRTC preferred, iroh relay fallback).
    pub async fn connect(
        ticket: String,
        transport: Option<String>,
    ) -> Result<ShareClient, JsValue>;
    pub fn transport(&self) -> String; // "webrtc" or "relay" after connect
    pub async fn manifest(&self) -> Result<JsValue, JsValue>;
    pub async fn read(&self, index: u32, offset: u64, len: u32) -> Result<Uint8Array, JsValue>;
}
```

UI omits the mode (dynamic default). ICE lab passes `"webrtc"`. Node/npx
passes `"relay"` so it never pays an ICE timeout it cannot win.

**One library, two consumers.** `crates/agent-share-wasm-client/` is the single shared wasm target:
the browser UI and the `npx agent-share` CLI link the same `.wasm`, differing
only in wasm-bindgen glue. `cargo task web-wasm` emits both from one build —
`dist/web/` (`--target web`, for the `web/` SPA) and `dist/nodejs/`
(`--target nodejs`, for the npm package). Same crate, same exports.

Deliberately **not** on the surface, agreed with the npx work: no progress
callback (the CLI chunks `read()` itself and owns its own progress), and no
serve surface (npx is receive-only; producing stays on the native binary,
which needs filesystem access wasm does not have).

Under `dynamic` (default), signaling uses the ticket's iroh relay ladder; mount
prefers WebRTC, then falls back to dialing `MOUNT_ALPN` over that same relay/IP
path. `webrtc` keeps ICE failure fatal; `relay` skips WebRTC entirely.

Build via `cargo task web-wasm`, mirroring `tasks/src/webrtc_wasm.rs` (needs
`wasm-bindgen-cli` and a wasm-capable clang — Homebrew LLVM on macOS, since ring's C core
won't build under Apple clang). Expect ~2.6 MB before optimisation; `opt-level = "s"` + LTO
already configured.

### Phase 5 — the React app

`ui/` — Bun + Vite + React 19 + TypeScript, `"moonspace-ui": "file:../../../../personal/moonspace-ui"`.
Vite transpiles its raw `.ts` directly (`moduleResolution: "bundler"`,
`allowImportingTsExtensions`). Wrap in `<ThemeProvider theme={theme}><GlobalStyle/>` per
`.storybook/preview.tsx`. Read the ticket from `location.hash`; empty hash → a landing page
showing the `agent-share serve` command.

> **Superseded:** the app is now visage + vendored `moonspace-ui` under `web/vendor/`
> (Bun workspaces). No React / styled-components; `Theme(T)` + `global.css` replace
> ThemeProvider/GlobalStyle. Hash ticket routing is unchanged.

**Column view** (`web/ColumnView.tsx`) — Finder-style Miller columns from `Box`,
`Stack direction="row"`, `Text`, `MiddleTruncate`:

- Build the tree once from the manifest, reusing the shape of `nfs::build_tree`
  (`src/mount/nfs.rs:58`) and its `validate_rel_path` / `safe_component`
  (`src/file/walk.rs:13`) hardening — the manifest is attacker-controlled.
- One `Box border="line"` per level, fixed cell width, horizontal scroll auto-advancing right.
- Selection uses `bgSelected` + inversion, never a font-size change — moonspace-ui has no
  `fontSize` token by design, which is exactly the "one size only" requirement.
- Directory affordance is `glyphs.chevron` (`▸`); icons are characters, not SVG.
- Virtualize long directories: the manifest is one-shot, so "lazy" means lazy *bytes* and
  virtualized rendering, not lazy metadata.
- Keyboard: arrows, matching Finder.

**Download button**, top right: walk every file, `read()` in 256 KiB chunks, stream into a
ZIP with `client-zip` (~3 KB). Pipe to a `FileSystemWritableFileStream` via
`showSaveFilePicker` where available, Blob fallback otherwise — never buffer the tree in
memory. Progress via `ProgressBar`.

> Font caveat: the stack leads with TX-02 (Berkeley Mono), proprietary, **no font files
> ship**. Without a license the app falls back to the system monospace stack. Works
> identically; just won't match Storybook.

### Phase 6 — hosting

`share.agent-habilis.com` is a **pure static site** — no backend, no signaling server, no database.
The relay does rendezvous; the ticket never leaves the client. Deploy `web/dist` anywhere.

## Files touched

| Action | Path |
|---|---|
| commit | the 21 staged files (rename + ALPN fork) |
| new | `crates/agent-share-proto/` (from `src/mount/{wire,ticket}.rs`, `src/protocol/**`) |
| new | `crates/fofoca-iroh-webrtc-transport/` (protocol at the root, `host`/`web` backends) |
| new | `crates/agent-share-wasm-client/` (from `webrtc-browser/`, minus the duplicated types) |
| new | `web/` React app |
| edit | `Cargo.toml` — members, iroh bump, `unstable-custom-transports`, `exclude` `web/` |
| edit | `src/mount/produce.rs` — second ALPN, signal handler, WebRTC transport |
| edit | `src/mount/mod.rs` — `announce()` URL; consts move to proto |
| edit | `src/mount/consume.rs` — WebRTC as a third dial path; else `use` lines |
| edit | `src/lookup/relay.rs:27` — relay **ladder** |
| edit | `src/cli/args/mount.rs` — `--no-web` |
| edit | `tasks/src/main.rs` — `web-wasm`, `web` tasks |

## Verification

**Wire format unchanged** — the thing most likely to break silently:
```
cargo test                      # all three golden pins, UNMODIFIED
cargo test --test mount         # subprocess bridge round trip
cargo check --target wasm32-unknown-unknown -p agent-share-proto
```

**The constant is shared, not copied:**
```
grep -rn --include=*.rs "0x5752_5443" crates/   # exactly one hit, in fofoca-iroh-webrtc-transport/src/addr.rs
grep -rn --include=*.rs "enum SignalEnvelope" crates/  # exactly one hit
```

**Transport:**
```
cargo test -p fofoca-iroh-webrtc-transport --features host  # quic_echo_over_webrtc, detach_then_reattach,
                                          # plus a new srflx-in-SDP assertion
```

**Cross-implementation golden vectors** — encode a fixture manifest in Rust, decode it in
the wasm client, assert equality. Catches drift the Rust-only pins cannot see.

**CLI-to-CLI not regressed, and gains WebRTC:**
```
cargo test --test mount -- --ignored   # real OS mount, still on direct IP
```
Plus a test forcing the consumer down the WebRTC path (direct + relay disabled) and
mounting successfully — proving one crate serves both consumers.

**End to end, same machine:**
```
cargo task web-wasm && cd web && bun run dev
agent-share serve ./fixture      # open the printed URL against the dev server
```
Confirm the column view lists the fixture tree, a ranged read returns correct bytes, and
`transport()` reports `"webrtc"`.

**End to end across NATs** — the case the experiment has never passed. Serve from one
network, open from another. Confirm `transport() == "webrtc"` (STUN worked), then force a
WebRTC failure and confirm `"relay"` with the tree still usable. **Measure this rather than
assume it.**

**Download:** click download on a multi-file tree, unzip, diff against the source.

## Risks

1. **Phase 0 is bigger than a version bump.** agent-share pins forked `noq`/`netdev`
   alongside iroh; moving to rev `195cb98d` may surface API drift in the unstable
   `CustomTransport` traits. This gates everything.
2. **iroh 1.0-line wasm** — official guidance references 0.33. Needs
   `default-features = false` and both `getrandom` generations configured
   (`webrtc-browser/Cargo.toml` documents the exact incantation). Verify a bare wasm build
   early.
3. **Symmetric NAT on both ends fails outright.** With the relay restricted to rendezvous
   there is no data path left to fall back to, so such a pair simply cannot transfer. This
   is the one place the design trades reachability for the guarantee that no file byte
   touches a relay. Mitigations if it bites: add a TURN client (str0m has none — roughly
   500 lines of Allocate/Refresh/CreatePermission/ChannelBind, and a TURN server is still a
   relay, just one inside the WebRTC session), or relax the rule. Measure before choosing.
4. **The ported code is stranded** — commit `273c23a` is unpushed, unmerged, and 75 commits
   behind an agent-gossip `main` that reorganized into `crates/`. We copy rather than
   depend, so it's a one-time cost, but upstream fixes won't flow.
5. **Web-on-by-default.** `<clasp-engulf>` recommended reversing this to opt-in, reasoning
   that `serve` today opens no listening port and publishes nothing. Under this architecture
   the premise is weaker — there is no rendezvous, and the producer already sits on the relay
   by default, so `--web` adds only a second ALPN and a printed URL. Keeping the decision;
   flip to `--web` opt-in if the phone-home framing still bothers you.
6. **Huge manifests block first paint** — 64 MiB cap is ~1.5M entries. Virtualize; only
   consider a paged op if measurement demands it, since that would break decision #3.
7. **Bearer secret in browser history** — the fragment keeps it off the server, but local
   history retains it. Inherent to a shareable link; worth a note in the UI.
