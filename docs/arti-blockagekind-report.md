# arti bug report — `BlockageKind::CantReachTor` is unreachable

Draft for <https://gitlab.torproject.org/tpo/core/arti/-/issues>. Found while building a guard-health
check for Night Drop, 2026-08-04, against `arti-client 0.43.0` / `tor-chanmgr 0.43.0`.

---

**Title:** `BlockageKind::CantReachTor` can never be produced (dead match arm in `status.rs`)

**Component:** arti-client / tor-chanmgr
**Version:** 0.43.0

### Summary

`arti_client::status::BlockageKind::CantReachTor` is unreachable. Nothing in the crate can produce
it, so a caller matching on it has a branch that never runs. It is part of the public API and
documented as a distinct condition ("We have some other kind of problem connecting to Tor"), so a
caller reasonably expects it to occur.

### Detail

`BootstrapStatus::blocked()` derives its `BlockageKind` from `tor_chanmgr::ConnBlockage`, in
`arti-client/src/status.rs`:

```rust
match conn_blockage {
    ConnBlockage::NoTcp        => BlockageKind::Offline,
    ConnBlockage::NoHandshake  => BlockageKind::Filtering,
    ConnBlockage::CertsExpired => BlockageKind::ClockSkewed,
    _                          => BlockageKind::CantReachTor,
}
```

`ConnBlockage` (in `tor-chanmgr/src/event.rs`) has exactly three variants, and all three are matched
explicitly above the wildcard:

```rust
#[non_exhaustive]
pub enum ConnBlockage {
    NoTcp,
    NoHandshake,
    CertsExpired,
}
```

The `_` arm exists only to satisfy `#[non_exhaustive]`. Since no fourth variant exists,
`CantReachTor` is dead. The only other producer of a `BlockageKind` in the crate is
`BlockageKind::CantBootstrap`, from the directory-manager path, which is separate.

### Why it matters to a caller

We were trying to answer "is this client stuck in a way that rotating entry guards would fix?", so
as to avoid rotating guards for any other reason — guard rotation costs anonymity margin, and we did
not want to do it because the user walked into a lift.

`CantReachTor` reads like exactly the right signal: connectivity exists, but Tor specifically is not
reachable. It is the only kind for which a guard reset is defensible. Discovering it was dead cost
some time, and the fallback options are all wrong for the question:

- `Offline` conflates "no network" with "every guard I have is unreachable". `ConnStatus::online` is
  derived from `last_tcp_success`, i.e. successful TCP to *relays*, and the relays a client dials
  first are its guards. So a wholly unreachable guard set and a wholly unreachable network look
  identical from outside.
- `Filtering` covers a censor MITM, where the correct response is bridges rather than new guards.

We ended up removing our check entirely, which is arguably the right outcome anyway — see the note
below — but the dead arm is still worth fixing or documenting.

### Suggested resolutions

Any of:

1. Remove the `CantReachTor` variant, or mark it deprecated, if it is not intended to be produced.
2. Document on the variant that it is currently unreachable and reserved for future `ConnBlockage`
   variants, so callers do not build logic on it.
3. If the intended meaning is "TCP works to some relays but the Tor protocol does not", produce it
   from a `ConnBlockage` variant that actually distinguishes that case.

### Possibly related

`tor-hsservice/src/status.rs` carries `TODO (#1270)` about splitting `State::Recovering` into
reachable and unreachable cases. That is a different conflation with a similar shape: a caller
cannot tell "degraded but serving" from "degraded and dark". We hit that one too — an onion service
with its descriptor on 8/8 HSDirs for both time periods, 4/4 introduction points and zero upload
failures reported `Bootstrapping`, so `is_fully_reachable()` was `false` for a service that was
serving normally.

### Incidental data point, offered in case it is useful

While testing the above we blackholed all four of a client's *confirmed* guards at the router
(`action=drop`, rest of the internet reachable and verified reachable), then cold-started arti. It
bootstrapped, sampled a replacement guard, reached `Running` and published its onion descriptor in
about 80 seconds. Separately, a single guard becoming unreachable mid-session was replaced in 79 s
without discarding the persisted set.

Guard recovery works well, which is the reason we concluded an application-level "reset the guards"
heuristic was unnecessary in the first place.

We also noticed that rewriting the `orports` in `guards.json` to unroutable addresses has no effect
while a live consensus is available — arti resolves guard addresses from the netdir by relay
identity. That is sensible behaviour; noting it only because it makes `guards.json` address
corruption a non-issue, which was not obvious to us from outside.
