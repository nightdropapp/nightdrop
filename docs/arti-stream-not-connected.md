# "bad API usage (bug) … Stream not connected" on relay takes

Status: **explained** (2026-10-07). It is the Tor network closing circuits, which arti misreports
as an API bug. The client now retries the stream open once (`circuit_closed_under_stream` in
`core/src/transport/tor.rs`); an upstream report is drafted below.

## What clients see

A mailbox take through our relay occasionally fails with:

```
relay connect <onion>: tor: bad API usage (bug): Protocol error while launching a data stream:
Problem building circuit 4.172, while begin stream: Stream not connected
```

The next round always succeeded. Seen on arti 0.43 (the phone, 0.1.27) and 0.47 (the 0.1.28 test
builds). On 2026-10-05 the phone and the Windows test app failed in the **same second**
(22:51:04 UTC), which pointed at our relay's side rather than one client.

## What the error means (tor-proto 0.47)

- `Error::NotConnected` is raised when the stream's receive channel ends **without an END cell**
  while the client waits for CONNECTED (`src/stream/raw.rs`, the `Poll::Ready(None)` arm). Had
  the far side refused the stream, the client would get `Error::EndReceived(reason)` instead.
- The channel ends that way when the stream map is dropped, i.e. **the whole circuit closed**
  under the stream.
- `NotConnected` maps to `ErrorKind::BadApiUsage` (`src/util/err.rs`, the `E::NotConnected` arm).
  That is the misclassification: here it is a circuit dying in the network, not caller misuse.

## What the relay showed (2026-10-06 12:36 → 2026-10-07 12:36 UTC, arti 0.47)

The relay ran with stream-outcome tracing (`nightdrop_relay::streams`, counts and error text only)
and `tor_proto::channel::reactor=trace` (DESTROY cells and channel control messages: circuit ids
and cell commands, never relay-cell contents).

**One failure captured end to end.** The relay's own watchdog self-dial — the relay as client
*and* service, so both ends in one log — failed with the clients' error:

```
16:18:05.816  DESTROY received on channel 0
16:18:06.034  channel 4 stopped: "peer closed connection without sending TLS close_notify"
16:18:06.036  two streams the relay was serving → "Stream not connected"
16:18:06.154  the relay's own self-dial → "Stream not connected"
```

Channel 4 carried the relay's own circuit builds (a `CREATED2` 30 s earlier): a connection to one
of its guards. The relay at the far end dropped it, every circuit through it died at that instant,
and every stream on those circuits failed together, on both ends, with no END. That is the
2026-10-05 pattern exactly: two clients whose rendezvous circuits met the relay through the same
guard fail in the same second.

**Over 24 hours** (172 "Stream not connected" on the relay side, 37 channel deaths, 6,572 DESTROY
cells received):

| Within … of the failure | Observed | Random baseline |
|---|---|---|
| a channel death (2 s) | 14 / 172 | 0.1 |
| a DESTROY received (0.3 s) | 162 / 172 | 3.1 |

The self-dial reached the relay 279 times and failed 4 times (3 ordinary timeouts and the event
above).

**What is not separated.** For the DESTROY-only majority the logs do not say who tore the circuit
down: a hop, the rendezvous point, or a client simply leaving (a phone backgrounded mid-request).
The DESTROY *reason* would tell them apart, but arti logs it only under
`tor_proto::client::reactor::circuit=debug`, whose other lines debug-print relay message contents —
mailbox handles, on the relay — so that target stays off.

## What changed

- **Client:** `relay_dialer_with` retries the stream open once when the error is exactly this
  (kind `BadApiUsage` *and* "Stream not connected" in the chain), within the same 30 s dial
  timeout. Nothing has been sent at that point, and arti picks or builds another circuit. A genuine
  API misuse of the same kind still fails at once (`circuit_closed_tests`). Not yet observed live:
  the trigger is network-side and cannot be produced on demand; a diagnostic build logs
  `relay: circuit closed while opening a stream — retrying once` when it fires.
- **Relay:** the temporary tracing was removed on 2026-10-07 (one restart).

## Draft upstream report (arti, gitlab.torproject.org/tpo/core/arti)

> **Title:** A circuit closed under a pending BEGIN is reported as `ErrorKind::BadApiUsage`
>
> When a circuit is torn down (DESTROY from the guard, or the guard's channel dropping without TLS
> close_notify) while a data stream on it is waiting for CONNECTED, `connect()` fails with
> `Protocol error while launching a data stream … while begin stream: Stream not connected`, and
> `HasKind::kind()` is `BadApiUsage`, rendered as "bad API usage (bug)".
>
> The cause is `tor_proto::Error::NotConnected` (raised in `stream/raw.rs` when the stream's
> receiver ends without an END cell), which `util/err.rs` maps to `EK::BadApiUsage`. On this path
> it is not caller misuse but a network failure, and callers that branch on the kind — retry
> transient failures, surface bugs — get it backwards.
>
> Observed with arti 0.43 and 0.47, from both sides of an onion service: over 24 h, 162 of 172
> occurrences on the service side followed a received DESTROY within 0.3 s, and one occurrence on a
> client co-located with the service followed its guard channel closing ("peer closed connection
> without sending TLS close_notify") by 120 ms.
>
> Suggested: report a stream whose circuit closed before CONNECTED as `ErrorKind::CircuitCollapse`,
> keeping `NotConnected` → `BadApiUsage` only for a caller using an already-closed stream.
