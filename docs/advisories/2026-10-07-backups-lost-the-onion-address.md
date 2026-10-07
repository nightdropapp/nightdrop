# Backups from 0.1.16 to 0.1.28 did not carry the onion address

**Date:** 2026-10-07
**Severity:** Medium. Nothing is lost and nothing is exposed, but a restored device came back on
a new `.onion` that its contacts were never told, so their direct connections to it stopped
working; messages kept flowing through the relay, slower. Two contacts who both restored could
only ever reach each other through the relay.
**Affected:** Restores of **file backups** made with 0.1.16 through 0.1.28. Server backups always
came back on a new address by design (they are fetched over Tor, after it starts) and announced
it; they had a smaller, separate defect, below.
**Fixed in:** 0.1.29.

## What happened

ARCHITECTURE.md §1a requires backups to bundle the onion identity: the `.onion` *is* that key,
and each contact stores the address it paired with, with nothing refreshing it afterwards.

Up to 0.1.15 a backup did that by copying arti's on-disk keystore (`collect_onion_keys`). 0.1.16
moved the identity out of that keystore and into a sealed file beside the state, keeping arti's
keystore in memory (`docs/design/onion-key-at-rest.md`). The backup code kept copying the on-disk
keystore — which no longer existed. Every backup from then on carried no identity at all.

On restore, arti therefore minted a fresh identity. Two more defects then made it worse:

- **The new address was never announced.** `restore_backup_tor` did not call
  `announce_address_if_changed`, and the save that followed recorded the new address as ours, so
  no later start ever saw a change either. Contacts kept dialling the old onion indefinitely;
  their messages fell back to the relay, and ours to them still went direct.
- **A server-backup restore was not sealed.** `restore_server_backup_tor` left its fresh identity
  only in memory, so the *next* start minted yet another address (that one was announced).

Nothing checked the restored address against the backed-up one, so no test noticed. It was found
in a code review, and confirmed with a live Tor test that failed on the restore itself.

## What changed in 0.1.29

- A backup now carries the identity itself (`PersistedState::onion_identity`, read from the live
  transport), and a file restore hands it to arti at bootstrap: same address, sealed at once.
  Backups from 0.1.15 and earlier still restore from their keystore files. Older apps ignore the
  new field, so a 0.1.29 backup still restores on them — on a new address, as before.
- Every restore path seals the identity immediately and announces a changed address.
- Every start announces our address to each contact once per run (`Node::reannounce_address`).
  A contact that already holds it ignores it silently; one holding a stale address is repaired.
  That is what heals installs already affected: after updating, their contacts learn the address
  within one launch, with no re-pairing.
- `core/tests/tor_smoke.rs` `a_file_backup_restore_keeps_the_onion_across_the_next_restart`
  restores over real Tor and checks the address on the restore and on the start after it.

## What to do

Nothing, once both sides run 0.1.29: the first launch re-announces the address. Backups made
before 0.1.29 still lack the identity, so make a new backup after updating.
