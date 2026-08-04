//! Wire codec: the bytes that cross a [`transport`](crate::transport) stream between two
//! peers (`ARCHITECTURE.md` §6). Everything here is already end-to-end encrypted or
//! public handshake material — the transport (and any relay) only ever sees these bytes,
//! never plaintext or keys.
//!
//! Encoding is a versioned [`Envelope`] JSON (`{"v":2,"f":<frame>}`), wrapped by [`encode`] in a
//! 4-byte length prefix and **zero-padded to a fixed size bucket** so an observer of the (already
//! encrypted) transport can't use frame length to tell one frame from another — a short text, a
//! rename, and a control ack all look identical on the wire (`ARCHITECTURE.md` §6, metadata
//! protection). Binary fields inside the JSON are URL-safe base64.
//!
//! The envelope version lets two installed builds detect an incompatible protocol change and fail
//! **loudly** (a clear error) instead of silently misparsing a frame — see `TODO.md` #2. Bump
//! [`WIRE_VERSION`] on any change to `Frame`, the envelope, or the framing that older builds can't
//! read; the sealed relay blob carries the versioned frame inside it, so store-and-forward is
//! covered too.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use vodozemac::olm::OlmMessage;

use crate::Result;

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>> {
    Ok(URL_SAFE_NO_PAD.decode(s)?)
}

/// A serializable Olm ciphertext (a `PreKey` or `Normal` message). Carries only the
/// message type tag and the ciphertext body — no keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireOlm {
    /// Olm message type (0 = pre-key, 1 = normal), as returned by `to_parts`.
    pub message_type: u8,
    /// Ciphertext body (base64).
    pub body: String,
}

impl WireOlm {
    pub fn from_olm(message: &OlmMessage) -> Self {
        let (message_type, ciphertext) = message.to_parts();
        Self {
            message_type: message_type as u8,
            body: b64(&ciphertext),
        }
    }

    pub fn to_olm(&self) -> Result<OlmMessage> {
        let ciphertext = unb64(&self.body)?;
        OlmMessage::from_parts(self.message_type as usize, &ciphertext)
            .map_err(|e| anyhow::anyhow!("invalid olm message: {e}"))
    }
}

/// One framed unit on a peer-to-peer stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// Session opener: the initiator's long-term identity key, their reply address (an
    /// onion service hides the caller, so the recipient learns where to reply only from
    /// this field), and the first (pre-key) ciphertext from which the recipient derives
    /// the session (X3DH).
    Hello {
        identity_key: String,
        address: String,
        message: WireOlm,
    },
    /// One leg of the short-code PAKE bouncer (§5b). Opaque SPAKE2 bytes.
    Pake { data: String },
    /// A normal ratcheted message on an established session. `from` is the sender's
    /// long-term identity key (base64), so the recipient can route it to the right
    /// session whether it arrived directly or via the relay (no transport address needed).
    /// `id` is a random per-message token both sides remember, so a later
    /// [`Edit`](Frame::Edit) can name its target. Random → carries no metadata (and the
    /// whole frame is sealed when it sits on the relay).
    Message {
        from: String,
        #[serde(default)]
        id: String,
        message: WireOlm,
    },
    /// Replace the text of an earlier [`Message`](Frame::Message) (sender edit, ≤15 min).
    /// The encrypted payload packs `(target_msg_id, new_text)` — see `node::pack_edit` —
    /// so the relay/transport never learns which message changed or what it says now.
    Edit { from: String, message: WireOlm },
    /// Unsend ("delete for both") an earlier [`Message`](Frame::Message). The encrypted
    /// payload is just the target `msg_id` (see `node::pack_unsend`); the receiver replaces
    /// that message with a "deleted" tombstone. Same eligibility as an edit. A still-queued
    /// message is recalled from the relay instead, so the peer never receives it at all.
    Unsend { from: String, message: WireOlm },
    /// The sender toggled opt-in 24h server storage for this chat (§6). The new state
    /// ("on"/"off") is E2E-encrypted on the session; the receiver mirrors it so **both**
    /// parties see the persistent in-chat warning while it is active (invariant).
    Storage { from: String, message: WireOlm },
    /// The sender changed this chat's disappearing-messages timer. The new value (seconds as
    /// ASCII, `0` = off) is E2E-encrypted on the session; the receiver mirrors it so **both**
    /// devices expire messages on the same horizon.
    Disappearing { from: String, message: WireOlm },
    /// The recipient approved an inbound chat request (§5). Sent back to the joiner so
    /// their side learns the chat is live. `code` echoes the join/invite code used, so the
    /// joiner can correlate it with the invite they acted on. These are non-secret control
    /// signals (no plaintext), routed by the sender's identity key `from`.
    Approved { from: String, code: String },
    /// The recipient could not approve because an active chat already uses `code` (e.g. a
    /// re-used short code). Tells the joiner to request a fresh code.
    CodeInUse { from: String, code: String },
    /// The peer deleted this chat. The receiver shows a notice that the other person
    /// deleted the chat and a new one must be created. Carries an **E2E-encrypted marker** on the
    /// established session so the receiver can prove it came from the real peer — an unauthenticated
    /// plaintext `Closed` would let anyone who knows your identity key spoof a "chat deleted"
    /// notice or replay an old one. `message` decrypts to a fixed domain tag (`node::MARK_CLOSED`).
    Closed { from: String, message: WireOlm },
    /// Transparency signal (#7 / §11.6): the sender made a **Full** backup that includes this
    /// chat's history, so the receiver's messages now also live in the sender's backup. A
    /// non-secret control signal (no plaintext), routed by the sender's identity key `from`;
    /// the receiver shows a persistent "the other person keeps a backup of this chat" warning,
    /// mirroring the server-storage warning. Lite backups (identity/contacts only, no messages)
    /// do not send this. **E2E-authenticated** like [`Closed`](Frame::Closed): `message` decrypts
    /// to a fixed marker (`node::MARK_BACKEDUP`) on the session, so a stranger can't fake a "the
    /// other person is backing you up" scare or replay a stale one.
    BackedUp { from: String, message: WireOlm },
    /// **Informational** safety-number verification signal (§5b′): the sender tells us they marked
    /// (or unmarked) this chat verified. The UI reflects it as "the other person verified this
    /// chat" but it **never** sets our own `verified` flag — each side confirms the number itself.
    /// **E2E-authenticated**: `message` decrypts on the session to `node::MARK_VERIFIED` or
    /// `MARK_UNVERIFIED`, and *which one* carries the state — so it can't be forged, replayed, or
    /// have its state flipped in transit.
    Verified { from: String, message: WireOlm },
    /// Silent **mailbox ack** (§11.3): the receiver drained our mailbox. `from` is the acking
    /// peer's identity key. Never acked itself (no loops). Carries an **E2E-encrypted marker**
    /// (`node::MARK_ACK`) so it can't be forged or replayed.
    ///
    /// **Confirms nothing on its own, by design.** It names no message, so honouring it meant
    /// promoting every queued message — including ones the receiver dropped on arrival and ones
    /// queued after the drain — to "Delivered". Still sent, because peers on older builds
    /// understand only this; still received, as proof of life. What actually confirms a message is
    /// [`Delivered`](Self::Delivered), which names one.
    Ack { from: String, message: WireOlm },
    /// The sender changed their display name in this chat (§4). The new name is carried
    /// **E2E-encrypted** on the established session (so the relay sees only ciphertext); the
    /// receiver decrypts it and relabels every message from this contact.
    Name { from: String, message: WireOlm },
    /// The sender's Tor `.onion` address changed (e.g. a rebuilt keystore) — the new address
    /// is **E2E-encrypted** on the session; the receiver updates where it routes replies, so
    /// the contact survives a lost onion keystore (§5c). See `node::announce_address`.
    Address { from: String, message: WireOlm },
    /// The sender changed their advertised **extra relay set** (#17). The new comma-joined list
    /// of relay addresses is **E2E-encrypted** on the session; the receiver stores it as the
    /// contact's `peer_relays` and fans out future offline mail to those relays too. See
    /// `node::announce_relays_if_changed`.
    Relays { from: String, message: WireOlm },
    /// Onion client authorization (#22): the sender's **client descriptor-encryption public key**
    /// for *the receiver's* onion, so the receiver can authorize the sender to fetch its (restricted)
    /// descriptor and connect. Sent whenever the sender (re)learns the receiver's onion address —
    /// on pairing (`Hello`/join) and on address rotation (`Frame::Address`). `client_key` is a
    /// **public** key (`descriptor:x25519:…`), so it rides in the clear like the pairing `address`
    /// itself (an observer who learns it still can't decrypt our descriptor without the secret half,
    /// which never leaves the peer's keymgr); `from` routes it to the right contact. See
    /// `node::announce_client_key`.
    ClientKey { from: String, client_key: String },
    /// An image/video (or other) attachment, **E2E-encrypted** on the session. The encrypted
    /// payload is a packed envelope of (transfer_id, kind, mime, raw bytes) — see
    /// `node::pack_media` — so the relay/transport only ever sees ciphertext.
    Media { from: String, message: WireOlm },
    /// Sent just **before** a (slow) `Media` frame so the receiver immediately knows an
    /// attachment is on its way. Encrypted envelope of (transfer_id, kind, mime, size,
    /// thumbnail) — see `node::pack_media_incoming`. The matching `Media` frame carries the
    /// same `transfer_id`, letting the receiver replace the placeholder with the real media.
    MediaIncoming { from: String, message: WireOlm },
    /// Transparency signal (#1): the sender took a **screenshot** of this chat, so a copy of the
    /// visible messages now exists outside the app, in their device's gallery — beyond anything
    /// disappearing timers or remote-storage limits can reach.
    ///
    /// Deliberately an **event**, not a state flag like [`BackedUp`](Frame::BackedUp): each
    /// screenshot is reported and logged separately, since "they screenshotted this chat once,
    /// months ago" and "they are screenshotting it now" are different things to know.
    ///
    /// **E2E-authenticated** like the other control frames: `message` decrypts to a fixed marker
    /// (`node::MARK_SCREENSHOT`) on the session, so a stranger can't fake a screenshot accusation
    /// or replay a stale one. Carries no plaintext — the frame's arrival *is* the signal.
    ///
    /// **Not a guarantee.** Only Android 14+ can detect a screenshot at all (and never a photo of
    /// the screen), so absence of this frame does not mean no screenshot was taken. See
    /// `SECURITY.md`.
    Screenshot { from: String, message: WireOlm },
    /// Cover traffic (#4): a dummy post, addressed to **ourselves**, that exists only so the relay
    /// sees mailbox activity it cannot distinguish from real mail. Dropped on arrival — no history,
    /// no ack, no notification.
    ///
    /// Appended at the end because variant order is wire-visible. It never reaches a peer: cover is
    /// self-addressed, so an inbound `Cover` from anyone else is simply discarded like any other
    /// frame we have no use for.
    ///
    /// Carries no marker and no authentication, deliberately — there is nothing to authenticate.
    /// The bytes are random padding, and the fixed-size bucketing that already applies to every
    /// frame is what makes it indistinguishable from a real message on the wire.
    Cover { padding: Vec<u8> },
    /// Per-message **delivery receipt**: the receiver has actually processed the message whose id
    /// this frame carries, so the sender may finally call it delivered.
    ///
    /// Distinct from [`Ack`](Frame::Ack), which says only "I drained your mailbox" and names no
    /// message. That coarse signal cannot express what a device test on 2026-08-02 produced: a
    /// message was lost when the core was torn down mid-flight while the *next* one arrived
    /// normally. Anything meaning "everything up to now" would have marked the lost one delivered
    /// too. Olm decrypts out of order and a gap does not block later messages, so arrival of a
    /// later frame is genuinely no evidence about an earlier one — only a receipt naming the id is.
    ///
    /// The encrypted payload **is** the message id, sealed on the session like any control frame,
    /// so a forgery or replay cannot mark anything delivered. Never receipted itself (no loops).
    ///
    /// Appended at the end because variant order is wire-visible. A peer too old to know this
    /// variant simply ignores it and keeps working off `Ack` alone.
    Delivered { from: String, message: WireOlm },
}

impl Frame {
    /// A PAKE frame from raw SPAKE2 bytes.
    pub fn pake(data: &[u8]) -> Self {
        Frame::Pake { data: b64(data) }
    }

    /// Extract raw PAKE bytes from a `Pake` frame.
    pub fn pake_bytes(&self) -> Result<Vec<u8>> {
        match self {
            Frame::Pake { data } => unb64(data),
            _ => anyhow::bail!("not a PAKE frame"),
        }
    }
}

/// The wire protocol version, stamped into every [`Envelope`]. Bump this on any
/// incompatible change to [`Frame`] or the envelope shape so peers on an older build reject
/// the frame with a clear error instead of misparsing it. See `TODO.md` #2.
///
/// v2 added **length-prefixed, zero-padded framing** (see [`encode`]) and moved the
/// control frames (`Closed`/`Ack`/`BackedUp`/`Verified`) behind the ratchet.
pub const WIRE_VERSION: u8 = 2;

/// Size buckets a frame is padded up to, so an observer of the (already encrypted) transport
/// can't use length to tell frames apart — a short text, a rename, and a control signal all
/// look identical on the wire (`ARCHITECTURE.md` §6, metadata protection). Frames larger than
/// the top bucket round up to [`PAD_BLOCK`], bounding overhead for media to one block.
const PAD_BUCKETS: [usize; 3] = [1024, 4096, 16384];
const PAD_BLOCK: usize = 16 * 1024;

/// The padded on-wire length for a payload of `raw` bytes (which already includes the 4-byte
/// length prefix): the smallest bucket that fits, else the next [`PAD_BLOCK`] multiple.
fn padded_len(raw: usize) -> usize {
    for &bucket in &PAD_BUCKETS {
        if raw <= bucket {
            return bucket;
        }
    }
    raw.div_ceil(PAD_BLOCK) * PAD_BLOCK
}

/// The versioned envelope that actually crosses the wire. Serializing borrows the frame
/// (no clone on the hot send path); deserializing owns it.
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    v: u8,
    f: &'a Frame,
}

#[derive(Deserialize)]
struct Envelope {
    v: u8,
    f: Frame,
}

/// Encode a frame for the wire: a 4-byte big-endian length prefix, the versioned
/// [`Envelope`] JSON, then zero padding up to the next [`padded_len`] size bucket. The padding
/// hides the true frame length from anyone watching the transport (metadata protection); the
/// prefix lets [`decode`] recover the real bytes and ignore the trailing zeros.
pub fn encode(frame: &Frame) -> Vec<u8> {
    let json = serde_json::to_vec(&EnvelopeRef {
        v: WIRE_VERSION,
        f: frame,
    })
    .expect("Frame is always serializable");
    let total = padded_len(4 + json.len());
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out.resize(total, 0); // zero-pad to the bucket size
    out
}

/// Decode a frame produced by [`encode`]: read the length prefix, parse that many bytes as the
/// versioned envelope, ignore the padding. Rejects an envelope whose version this build does not
/// speak, so a future format change is a clean detectable break, not silent corruption.
pub fn decode(bytes: &[u8]) -> Result<Frame> {
    if bytes.len() < 4 {
        anyhow::bail!("bad frame: shorter than its length prefix");
    }
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = 4usize
        .checked_add(len)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| anyhow::anyhow!("bad frame: length prefix {len} exceeds buffer"))?;
    let env: Envelope =
        serde_json::from_slice(&bytes[4..end]).map_err(|e| anyhow::anyhow!("bad frame: {e}"))?;
    if env.v != WIRE_VERSION {
        anyhow::bail!(
            "unsupported wire version {} (this build speaks v{WIRE_VERSION}); \
             the peer is likely on a different app version",
            env.v
        );
    }
    Ok(env.f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;
    use crate::identity::LocalIdentity;

    #[test]
    fn olm_message_survives_the_wire_and_still_decrypts() {
        let alice = LocalIdentity::generate();
        let mut bob = LocalIdentity::generate();
        let bundle = bob.publish_prekey_bundle();
        let mut session = crypto::open_outbound(&alice, &bundle).unwrap();

        let original = crypto::encrypt(&mut session, b"over the wire");
        let wired = WireOlm::from_olm(&original);
        let bytes = serde_json::to_vec(&wired).unwrap();
        let back: WireOlm = serde_json::from_slice(&bytes).unwrap();
        let recovered = back.to_olm().unwrap();

        let accepted =
            crypto::accept_inbound(&mut bob, &alice.curve25519().to_base64(), &recovered).unwrap();
        assert_eq!(accepted.first_plaintext, b"over the wire");
    }

    #[test]
    fn frames_round_trip() {
        let hello = Frame::Hello {
            identity_key: "abc".to_string(),
            address: "peer.onion".to_string(),
            message: WireOlm {
                message_type: 0,
                body: "zzz".to_string(),
            },
        };
        assert_eq!(decode(&encode(&hello)).unwrap(), hello);

        let message = Frame::Message {
            from: "ikey".to_string(),
            id: "m1".to_string(),
            message: WireOlm {
                message_type: 1,
                body: "yyy".to_string(),
            },
        };
        assert_eq!(decode(&encode(&message)).unwrap(), message);

        let pake = Frame::pake(&[1, 2, 3, 4]);
        assert_eq!(decode(&encode(&pake)).unwrap(), pake);
        assert_eq!(pake.pake_bytes().unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn encoded_frame_carries_the_version() {
        let frame = Frame::Pake { data: "zz".into() };
        // The payload lives after the 4-byte length prefix, before the zero padding.
        let wire = encode(&frame);
        let len = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]) as usize;
        let json: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
        assert_eq!(json["v"], WIRE_VERSION);
        assert_eq!(json["f"]["t"], "pake");
    }

    /// Build a length-prefixed, padded frame carrying an arbitrary envelope JSON (test helper for
    /// forging a frame `encode` wouldn't normally produce).
    fn framed(json: &[u8]) -> Vec<u8> {
        let total = padded_len(4 + json.len());
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(json.len() as u32).to_be_bytes());
        out.extend_from_slice(json);
        out.resize(total, 0);
        out
    }

    #[test]
    fn a_future_version_is_rejected_not_misparsed() {
        // A frame from a build one version ahead must fail loudly, not decode as garbage.
        let future = serde_json::json!({
            "v": WIRE_VERSION + 1,
            "f": { "t": "pake", "data": "zz" },
        });
        let bytes = framed(&serde_json::to_vec(&future).unwrap());
        let err = decode(&bytes).unwrap_err().to_string();
        assert!(err.contains("unsupported wire version"), "{err}");
    }

    #[test]
    fn frames_are_padded_to_fixed_buckets_but_still_round_trip() {
        // A tiny control frame and a small text frame must be indistinguishable by length.
        let ack = encode(&Frame::Pake {
            data: String::new(),
        });
        let text = encode(&Frame::Message {
            from: "ikey".into(),
            id: "m1".into(),
            message: WireOlm {
                message_type: 1,
                body: "hello there".into(),
            },
        });
        assert_eq!(ack.len(), 1024, "small frames pad to the first bucket");
        assert_eq!(text.len(), ack.len(), "length leaks nothing between them");

        // A larger frame lands on a bigger bucket, and every frame still decodes.
        let big = encode(&Frame::Message {
            from: "ikey".into(),
            id: "m2".into(),
            message: WireOlm {
                message_type: 1,
                body: "x".repeat(2000),
            },
        });
        assert_eq!(big.len(), 4096);
        assert!(matches!(decode(&big).unwrap(), Frame::Message { .. }));

        // Padding bytes are not mistaken for content.
        assert!(matches!(decode(&ack).unwrap(), Frame::Pake { .. }));
    }

    #[test]
    fn a_truncated_length_prefix_is_rejected() {
        assert!(decode(&[0, 1]).is_err());
        // A prefix claiming more than the buffer holds must error, not panic.
        let mut bogus = 9999u32.to_be_bytes().to_vec();
        bogus.extend_from_slice(b"{}");
        assert!(decode(&bogus).is_err());
    }
}
