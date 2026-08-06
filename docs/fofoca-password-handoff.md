# Handoff: fofoca changes password-protected shares need

Written 2026-08-06, after shipping password-protected shares end to end.
Companion to [`fofoca-defects-handoff.md`](./fofoca-defects-handoff.md), and
the same rules apply: everything below lives in the
[`fofoca-network/fofoca`](https://github.com/fofoca-network/fofoca) workspace,
this repo pins it **by git rev**, so each fix means a fofoca change plus a rev
bump here (and a matching bump in
`crates/agent-share-wasm-client/Cargo.toml`, which restates the rev because it
is a separate workspace).

**None of these block the feature.** Password-protected shares work on native
and in the browser today, with 21 green e2e cells. What follows is the debt the
workarounds carry. Change 1 is the only one with user-visible impact; the rest
are correctness-of-contract.

## Background you need first

agent-share protects a share with **two** fofoca primitives, and it matters
which does what:

- **The mount credential** is `TicketAuth::derive(secret, password, label)`
  (`crates/fofoca-protocol/src/crypto.rs`). It produces the 32 bytes on the
  wire — the ticket secret verbatim with no password, an Argon2id stretch with
  one. Used by `agent-share-proto/src/auth.rs`. **Nothing below concerns it**;
  it fits the need exactly.
- **The share's mesh** is a topic mesh derived from the ticket secret, with a
  password applied on top via `Mesh::set_password` / `Mesh::apply_password`
  (`crates/fofoca-protocol/src/mesh/mod.rs:194,209`). Applying a password
  switches every derivation — gossip topic, rendezvous keypair, port ladder —
  onto the stretched key (`effective_seed`, `mesh/mod.rs:154`), and the daemon
  keys the state/meta/broadcast documents off the same stretch. This is where
  all four items live.

The producer mints the mesh id and puts it in the ticket
(`MountTicket.mesh_id`, only on a protected share). A consumer hands that id to
`JoinParams::resolve`, which decodes it, stretches the password, and compares
against the 16-byte verifier the id carries — **locally, with no producer**.
That last property is the whole design: a share is built to outlive its
producer (RFC 01, "every peer a seeder"), so a check that needs one to answer
is a check that usually cannot run.

---

## Change 1 — a peer that proved the password once cannot re-join later

**The only item here with user-visible cost.**

### Mechanism

`Mesh::stretched_key` (`mesh/mod.rs:74`) is private and settable through
exactly two doors, `set_password` and `apply_password`, and **both require the
`Password` itself**. There is no way to hand a `Mesh` a stretched key that was
derived earlier.

That breaks unattended re-seeding. `agent-share mirror` deliberately does not
store the password — it stores the *token* it derived (`origin.auth`, 0600) and
the origin's mesh id (`origin.mesh`), which is enough to serve reads but not to
compute `effective_seed`. So a mirror of a protected share, re-served with
`agent-share serve <dir>` and no `--password`:

- **serves** every byte it holds to anyone who dials it (the mount protocol
  authenticates the token, and the token is on disk), but
- **cannot join the share's mesh** — it never appears on anyone's roster, and
  it cannot be *found* by a peer that has only the link.

`crates/agent-share/src/mount/produce.rs::mint_share_mesh` handles this by
warning and serving without a mesh. Serving nothing would be the worse trade,
but the result is that a protected share's seeders are invisible exactly when
the producer is gone — which is the case seeding exists for.

### Suggested fix (in fofoca)

A create/join-side door for a key that was already proven:

```rust
/// Adopt a stretched key a peer derived earlier, having proved the password
/// then. For a re-seeder that holds the derived material but not the secret
/// a human typed.
pub fn adopt_stretched_key(&mut self, key: [u8; SEED_LEN], verifier: [u8; PASSWORD_VERIFIER_LEN])
```

Taking the verifier alongside keeps it checkable rather than a blind setter —
`password_verifier(&key)` must equal the id's, so a wrong key is refused the
same way a wrong password is, and the door cannot be used to bypass the
password.

### Acceptance

`agent-share mirror <protected-ticket> ./copy --password X`, then
`agent-share serve ./copy` with **no** `--password`, then kill the origin: a
fresh consumer holding only the ticket and the password finds the copy over the
mesh and reads from it. Today the last step fails — the copy is reachable only
by direct dial.

---

## Change 2 — password failures are only distinguishable by their prose

### Mechanism

Two ways a password can be wrong, neither typed across the crate boundary:

- `apply_password` fails with a bare `bail!("wrong password")`
  (`mesh/mod.rs:194`).
- `PasswordRequired` is **`pub(crate)`**
  (`crates/fofoca/src/daemon/params.rs:57`) — while its own doc at
  `params.rs:119` calls it *"typed for the frontends"*, which is precisely what
  it cannot be.

So agent-share matches on the message. Both copies:

- `crates/agent-share/src/mount/mesh.rs::explain_password_error`
- `crates/agent-share-wasm-client/src/mesh.rs::explain_password_error`

They look for the substring `wrong password`, and fall back to keeping the
original error rather than guessing. A reword upstream does not break the
build, does not fail a test in fofoca, and silently turns agent-share's
"that password does not open this share" back into a generic connection
failure — the exact regression the whole feature exists to prevent.

### Suggested fix (in fofoca)

Export a typed error and return it from both sites:

```rust
pub enum PasswordError {
    /// The id carries a verifier and no password was given.
    Required,
    /// The password does not match the verifier.
    Wrong,
    /// A password was given for an id that carries none.
    NotPassworded,
}
```

`JoinParams::resolve` already distinguishes all three
(`params.rs:126-134`); they just cannot be named from outside.

### Acceptance

Both `explain_password_error` copies become a `downcast_ref::<PasswordError>()`
match with no string comparison, and `cargo task e2e --cells password` stays
green.

---

## Change 3 — agent-share hardcodes fofoca's verifier constants

### Mechanism

`PASSWORD_VERIFIER_LEN` and `password_verifier` are `pub(crate)`
(`crates/fofoca-protocol/src/crypto.rs:218,225`). agent-share needs the length
to parse a swarm id's optional password field, so it vendors the numbers as
literals: `crates/agent-share/src/protocol/swarm/mod.rs` carries its own
`FEATURE_PASSWORD = 0b0001` and `PASSWORD_VERIFIER_LEN = 16`.

Those are wire format. A copy of a wire constant in a second repo is a copy
that can drift, and the drift shows up as a parse that silently reads the wrong
bytes rather than as a compile error.

### Suggested fix (in fofoca)

Export `PASSWORD_VERIFIER_LEN` and the `FEATURE_PASSWORD` bit from
`fofoca-protocol`. `password_verifier` itself can stay private — agent-share
reads the verifier out of `MeshConfig.password` rather than computing one.

### Acceptance

`crates/agent-share/src/protocol/swarm/mod.rs` imports both instead of
declaring them, and `swarm_id_wire_format_is_pinned` still passes.

---

## Change 4 — a passworded topic mesh takes two steps and a contradiction

Lowest priority: ergonomics, and the workaround is honest.

### Mechanism

`derive_topic_mesh_with` hardcodes `password: None`
(`crates/fofoca/src/daemon/params.rs:217-228`), and `setup_join`'s
`SetupKind::Topic` arm hardcodes `mesh_password = None` under the comment
*"A topic mesh is always passwordless"* (`crates/fofoca/src/daemon/setup.rs`).

**Be precise about what is and is not possible today**, because the comment
overstates it:

- A passworded **topic-derived mesh** works fine. `from_topic` derives
  `seed = topic_seed(topic)` from the string alone (`mesh/mod.rs:127`) — the
  config is stored, not mixed in — so `set_password` afterwards is sound: the
  stretch is salted by that seed, the verifier lands in the config the id
  encodes, and `effective_seed` switches every derivation onto the stretched
  key. `crates/agent-share/src/mount/mesh.rs::mint` does exactly this, for
  every protected share, and all eleven password cells exercise it.
- What is *not* reachable is a passworded mesh through **`SetupKind::Topic`**,
  because that arm drops the password.

Neither costs anything here. agent-share never takes the `Topic` arm: `join()`
stringifies the mesh and re-parses it, which is a `JoinTarget::Mesh`, so
`resolve` returns `SetupKind::Join`. And the `mesh_password` that arm zeroes is
not what keys document encryption — `mesh_key` comes from
`mesh.stretched_key()`, read off the `Mesh` regardless of kind — it is retained
for blob-ticket protection, which agent-share does not use.

So this item is **ergonomics and comment accuracy only**: two steps where one
would do, beside a comment that says the thing is impossible while the code
supports it. It will cost something the first time someone reads the comment
and believes it.

### Suggested fix (in fofoca)

Give `derive_topic_mesh_with` an `Option<&Password>`, apply it inside, and
carry it through the `Topic` arm instead of dropping it. Then correct or delete
the comment.

### Acceptance

`mesh::mint` becomes one call instead of derive-then-`set_password`, and
`cargo task e2e --cells password` stays green.

---

## Workarounds in this repo to unwind after the fixes

| Workaround | Where | Unwind when |
|---|---|---|
| Mirror of a protected share serves but does not join the mesh, with a `tracing::warn!` | `crates/agent-share/src/mount/produce.rs::mint_share_mesh` | Change 1 → adopt the stretched key from the sidecar and join normally |
| `origin.auth` + `origin.mesh` sidecars | `crates/agent-share/src/mount/mirror.rs` | Change 1 → add the stretched key beside them (0600, same class of secret) |
| String-matching `"wrong password"` | `mount/mesh.rs` + wasm `mesh.rs`, both `explain_password_error` | Change 2 → downcast to `PasswordError` |
| Vendored `FEATURE_PASSWORD` / `PASSWORD_VERIFIER_LEN` | `crates/agent-share/src/protocol/swarm/mod.rs` | Change 3 → import from `fofoca-protocol` |
| Derive-then-`set_password` | `crates/agent-share/src/mount/mesh.rs::mint` | Change 4 → one call |

## What is *not* a fofoca problem

Recorded so the next agent does not go looking:

- **`TicketAuth`** fits exactly. No change wanted.
- **Blob passwords are host-only** (`blob = ["host"]`) and so absent from wasm
  builds. agent-share does not use blob tickets; irrelevant here.
- **Argon2id costs ~100 ms and 19 MiB, synchronously, on every target.**
  Deliberate, and a frozen network-wide contract
  (`fofoca-util/src/consts.rs:195-211`). agent-share pays it once per share,
  never per request. Not a defect.
- **The browser producer used to drop `OP_HASH`**, which made
  `agent-share mirror` unable to copy a browser-produced share. That was
  agent-share's bug, fixed here in
  `crates/agent-share-wasm-client/src/produce.rs` (answer `BadIndex` —
  "cannot vouch" — as a native producer with no hash cache does).

## How to verify any of this

```sh
cargo task e2e --cells password   # 11 rows: native, browser, node, legacy tickets
cargo task ci
```

The rows that matter most for changes 1 and 2 are
`password-native-dead-wrong` (a wrong password named in under 5 s with no
producer running — the regression guard) and `password-native-mirror-reserve`
(the degradation Change 1 removes). `password-node-cli` skips until
`cd node && npm install` builds the `node-datachannel` addon.
