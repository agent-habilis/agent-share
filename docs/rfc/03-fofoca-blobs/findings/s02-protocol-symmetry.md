# S0.2 — is the mount protocol symmetric?

**Verdict: holds.** Regression guards live in
`crates/agent-share/src/mount/mod.rs` and run in the normal test suite.

## Why it mattered

[`../../01-every-peer-a-seeder.md`](../../01-every-peer-a-seeder.md) rests on
the claim that four of the five pieces of a swarm already exist, and the
load-bearing one is authentication: *"Any peer can authenticate any other peer's
READ with **zero new code**."* If that were wrong, every subsequent phase gets
more expensive, because a re-seeder would need a new auth path.

RFC 01 asserts it from reading code. Half a day to test it for real, which made
it the cheapest high-impact check available.

## What was established

**[verified]** The static half needed no test at all:

- `produce.rs:247` is a bare `&header[..SECRET_LEN] != secret`, with **no
  binding to the serving endpoint's identity**.
- `serve_established` takes `secret: &[u8; SECRET_LEN]` as a **parameter**
  (`produce.rs:216`), so two producers can share one secret with **zero
  production changes**.

**[measured]** The runtime half is now a permanent test:

`mount::tests::a_non_origin_peer_serves_the_origins_ticket_secret` mints one
secret, stands up two independent producers under it, and drives a
`RemoteClient` at the *second* one. It fetches the manifest and reads bytes.
A re-seeder needs no new auth code — confirmed, not inferred.

## The corollary: the danger is real too

`mount::tests::a_diverged_peer_answers_plausibly_and_wrongly` is the other half,
and arguably the more useful one. A peer serving a *diverged* tree under the
same secret **answers successfully with different bytes for the same index**.
No error, no short read — just wrong data.

That is exactly the silent-corruption class RFC 01's guard #1 (a manifest
fingerprint on every card) exists to prevent. The test pins it so the guard
cannot be quietly dropped later as unnecessary: the failure it prevents is now
demonstrable in fifteen seconds.

The test also asserts that the two manifests encode differently, i.e. that a
fingerprint can actually tell the trees apart.

## Why this is a permanent test, not a throwaway spike

Symmetry is an *invariant the design depends on*, not a one-time question. If
someone later binds the secret check to the serving endpoint's id — a
reasonable-looking hardening change — the swarm silently stops working. The
test converts that from a debugging session into a failing build.

To confirm it is genuinely load-bearing, make that change to `produce.rs:247`
and watch the first test fail.

## Still assumed

- That a re-seeder serving the origin's manifest **verbatim** keeps index
  authority coherent under `OP_WATCH` deltas. Untested — the tests here use
  static trees. This is Stage 1's kill-gate.
- That the browser mirror handle is re-servable at all. Also Stage 1.
