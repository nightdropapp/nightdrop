# "bad API usage (bug) … Stream not connected" on relay takes

Status: **open**, being traced on the live relay (2026-10-06). Notes toward an upstream arti
report, in the shape of [arti-blockagekind-report.md](arti-blockagekind-report.md).

## What clients see

A mailbox take through our relay occasionally fails with:

```
relay connect <onion>: tor: bad API usage (bug): Protocol error while launching a data stream:
Problem building circuit 4.172, while begin stream: Stream not connected
```

The next round always succeeds. Seen on arti 0.43 (the phone, 0.1.27) and 0.47 (the 0.1.28 test
builds). On 2026-10-05 the phone and the Windows test app failed in the **same second**
(22:51:04 UTC), which points at our relay's side rather than one client.

## What the error actually means (tor-proto 0.47)

- `Error::NotConnected` is raised when the stream's receive channel ends **without an END cell**
  while the client waits for CONNECTED (`src/stream/raw.rs`, the `Poll::Ready(None)` arm). Had
  the relay refused the stream, the client would get `Error::EndReceived(reason)` instead.
- The channel ends that way when the stream map is dropped, i.e. **the whole circuit closed**
  under the stream.
- `NotConnected` maps to `ErrorKind::BadApiUsage` (`src/util/err.rs`, the `E::NotConnected` arm).
  That is the misclassification: here it is a circuit dying in the network, not caller misuse.

So: the rendezvous circuit to our onion collapses at the moment of BEGIN. Two clients failing in
the same second fits something on the relay's half of those circuits dying (its guard or the
channel to it), which would collapse every rendezvous circuit routed through it at once.

## Evidence on the relay so far

- Stream outcomes are traced under `nightdrop_relay::streams` since commit d88c39e (counts and
  error text only: no handles, nothing identity-linked). The relay's own view of the same
  symptom appeared once: "stream ended with an error: Stream not connected" (2026-10-06
  11:44:44 UTC). No client failed at that second, so it is not yet a correlated event.
- arti 0.43 on the relay warned "Questionable guard: 52.9% of circuits died under mysterious
  circumstances" for its guard. Weak evidence on its own: 0.43 warns above 50% and disables
  above 70%, while 0.47 raised the warning to 91% and never disables (arti#2752, proposal 344)
  because the old thresholds fired too readily.

## What is running to settle it

Since 2026-10-06 12:36 UTC the relay runs arti 0.47 with a temporary drop-in,
`~/.config/systemd/user/nightdrop-relay.service.d/diag-streams.conf`, adding
`tor_proto::channel::reactor=trace`: DESTROY cells and channel control messages (circuit ids
and cell commands; relay cells are never logged there). Do **not** widen `tor_proto` to debug:
`client::reactor::circuit` debug-prints relay message contents, which would put mailbox handles
in the journal.

The question for the next client failure: does the relay log a DESTROY (and with what reason) or
a channel close at that second? Yes: a guard/path problem, and the arti report is only about
the error kind. No: look at the client's own path next.

When done: remove the drop-in, restart the relay once (a restart rotates introduction points,
so not casually), and record the outcome here.
