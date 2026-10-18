# Trust boundaries — every value that enters from outside

Made 2026-10-13 after six rounds of self-audit kept finding the same kind of bug: a value from a
contact, an invite, a backup, a relay or the network, used without a check. This is the list of
every such value, where it is used, and what bounds it. **A change that adds a field crossing one of
these boundaries adds a row here**, with its check, in the same commit.

Who is on the other side of each boundary:

* **Contact** — anyone paired with us. Authenticated (Olm), but not trusted: they may be hostile.
* **Stranger** — anyone holding an invite (QR or short code) or able to reach our onion or mailbox.
* **Relay** — the operator of any relay in either side's set. Sees handles, sizes and timing; can drop,
  replay or inject blobs.
* **Site** — whoever controls the onion website host. Authenticated by the onion address only.
* **Backup** — a backup file and its password, which may have been handed over by someone else.
* **Local** — another user or app on the same device. Out of scope for anything readable as us.

**Tested from the hostile side** in `core/src/node/tests_hostile.rs`: replays, forged senders,
oversized fields, cross-contact edits, 3,000 seeded mutations of genuine frames, and 1,500 hostile
plaintexts sealed on a real contact's session across every content-bearing frame, each checked
against the bounds below. Relay-side abuse is in `relay_client` and `relay/src/main.rs` tests and
the `relay_handle_line` fuzz target.

## Frames from contacts and strangers (`core/src/node/frames.rs`)

All but four frames carry an Olm message and act only on a chat whose session decrypts it.

| Value | From | Used for | Bound |
|---|---|---|---|
| Frame length | anyone reaching us | allocation | `MAX_FRAME_BYTES`, checked before reading (`wire::frame_len`) |
| Inbound streams to our onion | anyone reaching us | frames | 16 streams per circuit; 300 s with no frame closes the stream, 60 s without a byte mid-frame closes it and frees its budget (`tor::read_frame`); 64 MiB inbound budget (`InboundBudget`) |
| `from` in an Olm frame | contact | which session decrypts | must name a chat; the decrypt authenticates it |
| `Hello.identity_key` + prekey message | stranger with an invite | new chat | must decrypt with one of our one-time keys (single use) and its pass; becomes a *request* until accepted |
| `Hello.address` | stranger | kept, dialed | `limits::sane_address` (≤128, safe chars), else the transport's view; dialed only if a v3 onion (`tor::require_onion`) |
| Hello body: relay set / client key / pass / mailbox seed | stranger | routing, onion access | `sanitize_peer_set` (≤4, ≤128 each, deduped); `client_auth::validate_key`; 32-byte secrets via `decode_secret` |
| `Message.id`, `Burn.id` | contact | kept with the message, matched by receipts | `limits::sane_id` (≤128 printable), else dropped before decrypting |
| Message / burn / edit text | contact | stored, shown | `limits::clamp_text` (64 KiB, marked when cut); sending refuses more |
| `Burn` timer | contact | delete schedule | parsed u64; 0 refuses to store as permanent |
| Edit / unsend / viewed / delivered target id | contact | matched against our history | compared only, never stored; scoped to that contact's own (or our own) messages in that chat |
| Media `kind`, `mime` | contact | rendering; how a file is opened | `media_kind::sanitize` (image/video, fixed mime list) |
| Media / thumbnail bytes | contact | stored sealed; shown; opened externally | `MAX_FRAME_BYTES`; drawn in the app only as a plain JPEG (Huffman baseline/progressive, 1 or 3 components, ≤8192 px a side, ≤40 MP: `media_kind::displayable_image`, checked in `media_bytes`); opened outside the app only as a sniffed, self-contained video (`media_kind::video_ext`) under a name from those bytes |
| Media `transfer_id` | contact | kept, matched | `limits::sane_id` in every unpacker |
| `MediaIncoming.size` | contact | shown on a placeholder | display only |
| `MediaIncoming` preview | contact | stored, drawn | ≤256 KiB (`limits::MAX_THUMB_BYTES`), else dropped; then the JPEG gate when drawn |
| How much one contact adds | contact | history, media store | per rolling day 5,000 messages and 1 GiB of attachments (`limits::Allowance`); past that dropped, receipted, one notice |
| `Name` | contact | shown as their name | `clean_display_name` (no control/bidi/invisible, 64 chars) |
| `Address` | contact | kept, dialed | `limits::sane_address`; dial needs a v3 onion |
| `RelaySet` | contact | where their mail is posted | `sanitize_peer_set`; dial needs a v3 onion |
| `Disappearing` / `Storage` / `Captures` / `Burns` / `Verified` / `BackedUp` / `Closed` / `Screenshot` / `Version` / `Ack` | contact | settings and notices | Olm + exact marker or parsed value (`verify_control`, `is_plausible_version`); `Screenshot` carries one of four markers (screenshot, screen recording, capture block overridden here, and back on), and `Captures` either one of two exact markers (can report captures or not) or the capture-block prefix followed by `0|1:<seq>` (an older or unparsable sequence is ignored; a renewed block in a chat the user overrode is answered with the override notice again, at most once an hour), each decrypted once, anything else ignored — a contact's "blocks captures" makes this app block captures in their chat, which is their wish, honoured cooperatively; every one that leaves a notice counts against the contact's daily allowance (`limits::Allowance`), and a repeat of the current state leaves none |
| `MailboxKey` | contact | pair mailbox secret | Olm; 32 bytes |
| `ClientKeySealed` | contact | their key for our onion | Olm + marker; `validate_key`; installed only once accepted |
| `ClientKey` (unsealed) | anyone | — | ignored |
| `CodeInUse` | anyone | closes a waiting request | only if our session has never received anything |
| `Approved` | anyone | clears the "waiting" notice | changes only that notice and re-sends our own relay set on our session |
| `Pake`, `Cover` | anyone | — | ignored |

## Invites and short codes (`core/src/api.rs`, `node/pairing.rs`)

| Value | From | Used for | Bound |
|---|---|---|---|
| QR `addr=` | whoever made the QR | kept, dialed | `sane_address`; dial needs a v3 onion |
| QR `ik=`, `otk=` | QR | Olm session | must parse as keys; the Hello must decrypt |
| QR `rs=`, `ck=` | QR | relay set, pass | `sanitize_peer_set`; 32-byte `decode_secret` |
| Typed short code | user (from a stranger) | slot + SPAKE2 secret | trimmed, lowercased; three openers per invite; wrong code fails the AEAD |
| Short-code opener / answer | stranger, relay | SPAKE2 + ML-KEM | `two_fields` / `three_fields` length-checked (`take_field`, checked arithmetic) |

## Relay (`core/src/relay_client/mod.rs`, `relay/src/main.rs`)

| Value | From | Used for | Bound |
|---|---|---|---|
| Request line (relay side) | anyone reaching the relay | parsed | `max_line_bytes`; 512 MiB in flight across streams; 60 s idle / 256 B/s after 120 s; 30 req/s per stream; 16 streams per circuit; 512 streams |
| Post handle | anyone | store key | `valid_handle` (1–128, safe chars) |
| Post blob | anyone | stored ≤24 h | `max_blob_bytes`; each entry charged blob + handle + 512 B; per-mailbox and total caps; large blobs leave a 128 MiB reserve |
| Response line (client side) | relay | parsed | `MAX_RESPONSE_LINE_BYTES`, `MAX_TAKE_MANY` |
| Taken blobs | relay (or anyone who can post) | frames | `relay_unwrap` (AEAD under our key), then the frame rules above |

## Signed documents

| Value | From | Used for | Bound |
|---|---|---|---|
| `update.json` | site | update notice, download | ≤8 KiB; Ed25519 over the payload with the current update key (`keyroll`); refused past its signed `expires` or without one; download only if strictly newer; build bytes checked against the signed SHA-256 |
| `relays.json` / relay `GetDirectory` | site, relay | shared relay set | ≤16 KiB; Ed25519 with the current directory key; version monotonic |
| Key rotation | site, relay | which key signs | recovery-key signature binding purpose and sequence; sequence remembered, older keys refused |
| Remembered signing key (`keyroll::Trust`) | state file, backup | which key signs | kept with its rotation signature and re-verified on every use; unsigned or tampered is ignored (baked key) |

## Backups (`core/src/node/backup.rs`)

| Value | From | Used for | Bound |
|---|---|---|---|
| Backup file | backup | everything below | ChaCha20-Poly1305 under Argon2(password) |
| Onion keystore file paths | backup | written to disk | `keystore_dest`: plain relative components only |
| Media ids | backup | file names | `is_media_id` before any path is built |
| Contact names | backup | shown | `clean_display_name` |
| Learned update / directory keys | backup | which key signs | re-verified against the recovery key (row under Signed documents) |
| Everything else (history, relay sets, queued frames) | backup | the restored identity | trusted as the user's own data: restoring a backup someone else made means taking on their identity. Noted, not bounded |

## Network and files

| Value | From | Used for | Bound |
|---|---|---|---|
| Bridge lines (typed or fetched) | user, moat over a CDN | Tor bridges | `check_bridge_line` (the bootstrap's own parser); moat over verified TLS |
| WebTunnel TLS | network | bridge transport | WebPKI, or a pin/name from the bridge line; the bridge fingerprint is checked by Tor |
| Lock file, attempts file | local | unlock | Argon2 params from the file; a damaged file reads as locked out, never as open |
| Decrypted media for a player | — | written to `nightdrop-open/` | `0600` in a `0700` dir; removed with the message (`forget_media`), on logout and at start |
