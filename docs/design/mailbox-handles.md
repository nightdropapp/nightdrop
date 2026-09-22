# Design — Per-pair, epoch-rotating mailbox handles

**Status:** design agreed 2026-09-22, not yet implemented. Targets 0.1.23.
**Relates to:** `ARCHITECTURE.md` §6 (relay, store-and-forward) and §11.2, `multi-relay-mailboxes.md`
(#17), `cover-traffic.md` (#4). Group chat (0.3) depends on this but does not block it.

## 1. The leak

`core/src/node.rs`:

```rust
fn mailbox_handle(recipient_identity_key: &str) -> String {
    h.update(b"nightdrop/mailbox/v1");
    h.update(recipient_identity_key.as_bytes());
    format!("mbx:{}", base64_handle(&h.finalize()[..15]))
}
```

A **static** truncated SHA-256 of a **long-term** identity key. Its docstring calls it unlinkable,
and it is — *to the identity*. The relay cannot reverse it to an onion address or a public key.

But it never changes. `mbx:ABC` is the same person for the life of that identity, so a relay (or
whoever seizes one) can accumulate:

* **a contact graph**, from which handles receive deposits close together in time;
* **a per-mailbox behavioural profile** — activity, volume, hours — which `cover-traffic.md` §1
  already names as the threat cover traffic exists to blunt.

`ARCHITECTURE.md` §11.2 currently describes these as *"ephemeral, unlinkable handles"*. They are
neither. **That line is wrong and must be corrected in the same change** — an overclaim in the
authoritative document is worse than the leak, because it is what a reader relies on.

This is **not** a group-chat problem. It exists in 1:1 today. Groups only make it louder: a
fan-out to nine mailboxes is nine correlated deposits at one instant.

## 2. Rejected: padding the fan-out

The obvious mitigation — always deposit to 10 mailboxes regardless of real group size — fails.
Dummy handles are **never drained**, while real ones are collected; a relay separates them after a
few rounds. Padding with extra blobs to the *same* real handles hides the member count but leaves
the co-occurrence set intact, and membership is the more valuable leak. Scratched.

## 3. Rejected: epoch rotation alone

`H(ik_recipient ‖ epoch)` keeps one handle per person and breaks linkage across days. It looks
sufficient and is not: a **stable group re-links itself by its own co-occurrence pattern**. Nine
handles that appear together every morning are a fingerprint whatever they are called today.

## 4. The scheme

```
handle = "mbx:" ‖ b64( HKDF(mailbox_secret_AB, "nightdrop/mailbox/v2" ‖ epoch_day)[..15] )
```

* **Per-pair.** Alice→Bob and Carol→Bob produce *different* handles, so the relay cannot tell two
  deposits are even for the same recipient. This is what kills the co-occurrence graph, in 1:1 and
  in groups alike.
* **Per-epoch.** One UTC day. Readers accept the **neighbouring epoch** on either side, the same
  tolerance Tor uses for blinded onion keys, so clock skew needs no negotiation.
* **Computable by both ends** with no extra round trip: each side holds the pair secret.

**Cost:** a recipient polls one handle per contact instead of one in total — and with #17, one per
contact *per relay*. At the ≤10 contacts a small trusted circle implies that is trivial; it grows
linearly, so a cap should be stated rather than discovered.

### 4.1 Where the secret comes from

**Not `Session::session_id()`.** vodozemac computes it as
`SHA256(identity_key ‖ base_key ‖ one_time_key)` (`olm/session_keys.rs`) — all three public, and
the base key and one-time key **travel in the pre-key message**. Anyone who observed session setup
could recompute the handle, which hands back exactly the linkage being removed. Nothing else on
`Session` is both stable and secret: the root key ratchets forward.

So the secret is **ours to derive and persist**: at `create_outbound_session` /
`create_inbound_session`, take a value both sides compute identically, run it through HKDF with a
`nightdrop/mailbox/v2` label, and store it in the contact record beside the session. It never
ratchets, so the handle is stable for the life of the contact.

### 4.2 Contacts that already exist

Their ratchets have moved on; no such secret can be recovered after the fact. Rather than strand
them on `v1` for ever, the pair **agrees one over the channel they already have** — they are
mutually authenticated and confidential, so a small key-agreement frame is safe and needs no user
action. Migration then proceeds per contact rather than as a flag day.

## 5. Migration — the part that can lose messages

A 0.1.22 sender posts to a `v1` handle. A 0.1.23 recipient polling only `v2` never sees it, and
with a 24-hour TTL the blob expires unnoticed. **Silent message loss is far worse than the leak
being fixed**, so the transition is the design, not an afterthought.

1. **Announce the capability**, exactly as `Frame::Captures` announces screenshot reporting. Peers
   record per contact whether `mailbox-v2` is supported.
2. **Post `v2` only to contacts known to support it**; `v1` to everyone else.
3. **Poll both** `v1` and every `v2` handle throughout the transition.
4. **Tell the user.** A chat with a `v1`-only peer shows a persistent notice — the same shape as
   the existing "this device cannot report screenshots" warning — saying the other person is on an
   older version, that their messages are addressed less privately, and that support will be
   removed in a future release. The person who can act is the one on the old version, so the
   notice must be visible on **both** sides.
5. **Drop `v1` after two releases**, and say so in the changelog of each.

## 6. What this does not fix

* **The fan-out burst.** N deposits at one instant is still N deposits. Jitter and #17 spread it;
  they do not remove it. Handles being unrelated means the relay learns *that* a burst happened,
  not *who* it binds together.
* **The hot path is untouched — and does not need touching.** Onion-to-onion delivery never
  reaches a relay (`cover-traffic.md` §2), and an onion address is not geolocatable the way an
  IPv4 address is.
* **Cover traffic remains opt-in.** It blurs per-mailbox volume and timing, which is exactly the
  residual profile this scheme cannot erase, so 0.1.23 also offers it at onboarding beside the
  background-delivery prompt — **before** `createIdentity`, because `_Root` swaps the screen out
  from under a dialog awaited after it (see the comment on `_offerBackgroundDelivery`). Showing
  both prompts *during* identity creation is a worthwhile but separate change: it alters `_Root`'s
  routing condition, which was hardened on 2026-09-21 after a raced relaunch offered onboarding
  over a live identity.

## 7. Consequences elsewhere

`multi-relay-mailboxes.md` §2 states handles are relay-agnostic — still true, a pair's handle is
the same on every relay. What changes is §4.2's `drain_all(handle)`: a recipient now drains **one
handle per contact** on each relay, so the poll count becomes contacts × relays.
