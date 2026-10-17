//! A hostile peer against a real paired node (self-audit 2026-10-13, step 2 of the plan in
//! `docs/trust-boundaries.md`). Everything else in the suite runs trusted code against itself; here
//! the other side replays, forges, oversizes and mutates, and the victim must stay sound: no panic, no
//! new contacts, no change to another contact's chat, nothing kept past its bound, and the genuine
//! conversation still working afterwards.
use super::*;
use crate::transport::{MemoryNetwork, Transport};
use rand::{Rng, SeedableRng};

/// Alice, with Bob and Carol paired to her. Returns the network, the three nodes, and the ids each
/// side uses for the other.
struct Scene {
    net: MemoryNetwork,
    alice: Node,
    bob: Node,
    carol: Node,
    /// Alice's id as Bob and Carol know it.
    alice_id: String,
    /// Bob's and Carol's ids as Alice knows them.
    bob_id: String,
    carol_id: String,
}

fn scene() -> Scene {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let mut carol = Node::new(Box::new(net.endpoint("carol")));
    let bob_id = bob.identity_key();
    let carol_id = carol.identity_key();
    let bundle = alice.publish_bundle();
    let alice_id = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bundle = alice.publish_bundle();
    carol.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    bob.pump().unwrap();
    carol.pump().unwrap();
    Scene {
        net,
        alice,
        bob,
        carol,
        alice_id,
        bob_id,
        carol_id,
    }
}

/// Take the next raw frame waiting for Alice, as an attacker on the path would see it.
fn intercept(alice: &Node) -> Vec<u8> {
    alice.transport.try_recv().expect("a frame for alice").1
}

/// Hand Alice raw bytes from an endpoint she has never paired with.
fn inject(net: &MemoryNetwork, bytes: &[u8]) {
    net.endpoint("mallory").send("alice", bytes).unwrap();
}

/// A frame from Bob to Alice sealed on Bob's real session, with whatever plaintext the attacker
/// chooses — what a malicious but genuinely paired contact can send.
fn sealed_by_bob(s: &mut Scene, plaintext: &[u8]) -> WireOlm {
    let session = &mut s.bob.chats.get_mut(&s.alice_id).unwrap().session;
    WireOlm::from_olm(&crypto::encrypt(session, plaintext).unwrap())
}

fn texts(n: &Node, id: &str) -> Vec<String> {
    n.messages(id)
        .into_iter()
        .filter(|m| !m.system)
        .map(|m| m.text)
        .collect()
}

#[test]
fn a_replayed_message_is_shown_once() {
    let mut s = scene();
    s.bob.send(&s.alice_id, "only once").unwrap();
    let frame = intercept(&s.alice);
    for _ in 0..3 {
        inject(&s.net, &frame);
    }
    s.alice.pump().unwrap();
    let got = texts(&s.alice, &s.bob_id);
    assert_eq!(
        got.iter().filter(|t| *t == "only once").count(),
        1,
        "{got:?}"
    );
}

#[test]
fn a_frame_claiming_someone_elses_name_changes_nothing() {
    let mut s = scene();
    s.bob.send(&s.alice_id, "for alice").unwrap();
    let genuine = intercept(&s.alice);
    let Ok(Frame::Message { id, message, .. }) = wire::decode(&genuine) else {
        panic!("a message frame");
    };
    // Bob's ciphertext, claimed to be from Carol, and from someone Alice has never met.
    for from in [s.carol_id.clone(), "nobody-at-all".to_string()] {
        let forged = Frame::Message {
            from,
            id: id.clone(),
            message: message.clone(),
        };
        inject(&s.net, &wire::encode(&forged));
    }
    s.alice.pump().unwrap();
    assert_eq!(
        s.alice.contacts().len(),
        2,
        "no new contact from a forged sender"
    );
    assert!(
        texts(&s.alice, &s.carol_id).is_empty(),
        "nothing landed in Carol's chat"
    );
}

#[test]
fn oversized_fields_from_a_real_contact_are_bounded() {
    let mut s = scene();
    // An id far past the bound: the frame is dropped.
    let m = sealed_by_bob(&mut s, b"long id");
    let frame = Frame::Message {
        from: s.bob_id.clone(),
        id: "x".repeat(limits::MAX_ID_BYTES + 1),
        message: m,
    };
    inject(&s.net, &wire::encode(&frame));
    // A 200 KB text: kept, cut to the bound.
    let big = "a".repeat(200 * 1024);
    let m = sealed_by_bob(&mut s, big.as_bytes());
    let frame = Frame::Message {
        from: s.bob_id.clone(),
        id: "okid".into(),
        message: m,
    };
    inject(&s.net, &wire::encode(&frame));
    // A 1000-character address: ignored.
    let before = s.alice.chats[&s.bob_id].peer_address.clone();
    let m = sealed_by_bob(&mut s, "b".repeat(1000).as_bytes());
    inject(
        &s.net,
        &wire::encode(&Frame::Address {
            from: s.bob_id.clone(),
            message: m,
        }),
    );
    s.alice.pump().unwrap();
    let got = texts(&s.alice, &s.bob_id);
    assert!(
        !got.iter().any(|t| t == "long id"),
        "the long-id frame was dropped"
    );
    let kept = got
        .iter()
        .find(|t| t.starts_with("aaa"))
        .expect("the big text arrives");
    assert!(kept.len() <= limits::MAX_TEXT_BYTES + 8 && kept.ends_with(" […]"));
    assert_eq!(s.alice.chats[&s.bob_id].peer_address, before);
}

#[test]
fn one_contact_cannot_edit_another_contacts_message() {
    let mut s = scene();
    s.bob.send(&s.alice_id, "bob wrote this").unwrap();
    s.alice.pump().unwrap();
    let target = s
        .alice
        .messages(&s.bob_id)
        .into_iter()
        .find(|m| m.text == "bob wrote this")
        .unwrap()
        .msg_id;
    // Carol, a genuine contact, edits and unsends Bob's message by id, on her own session.
    let carol_alice = s.alice_id.clone();
    let session = &mut s.carol.chats.get_mut(&carol_alice).unwrap().session;
    let edit = WireOlm::from_olm(
        &crypto::encrypt(session, &pack_edit(&target, "carol rewrote this")).unwrap(),
    );
    let unsend = WireOlm::from_olm(&crypto::encrypt(session, &pack_unsend(&target)).unwrap());
    for frame in [
        Frame::Edit {
            from: s.carol_id.clone(),
            message: edit,
        },
        Frame::Unsend {
            from: s.carol_id.clone(),
            message: unsend,
        },
    ] {
        inject(&s.net, &wire::encode(&frame));
    }
    s.alice.pump().unwrap();
    assert!(texts(&s.alice, &s.bob_id).contains(&"bob wrote this".to_string()));
}

#[test]
fn thousands_of_mutated_frames_never_panic_or_change_who_is_paired() {
    // Seeded, so a failure reproduces: mutate genuine frames of every common kind (flip, cut, extend,
    // splice) and feed them through the real receive path.
    let mut s = scene();
    let key: StoreKey = [5u8; 32];
    let base = std::env::temp_dir().join(format!("nightdrop-hostile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    s.alice.set_media_store(
        base.join("a/nightdrop-media")
            .to_string_lossy()
            .into_owned(),
        key,
    );
    s.bob.set_media_store(
        base.join("b/nightdrop-media")
            .to_string_lossy()
            .into_owned(),
        key,
    );
    let mut corpus = Vec::new();
    let alice_id = s.alice_id.clone();
    s.bob.send(&alice_id, "hello").unwrap();
    corpus.push(intercept(&s.alice));
    s.bob.set_my_name(&alice_id, "Bob").ok();
    s.bob
        .send_media(
            &alice_id,
            super::media_scrub::TEST_JPEG,
            "image/jpeg",
            "image",
            &[],
        )
        .unwrap();
    s.bob.send_burn(&alice_id, "burn", 30).ok();
    s.bob.send(&alice_id, "edit me").unwrap();
    while let Some((_, f)) = s.alice.transport.try_recv() {
        corpus.push(f);
    }
    assert!(
        corpus.len() >= 3,
        "a varied corpus ({} frames)",
        corpus.len()
    );

    let mut rng = rand::rngs::StdRng::seed_from_u64(0x0d_2026_1013);
    for _ in 0..3000 {
        let mut f = corpus[rng.gen_range(0..corpus.len())].clone();
        match rng.gen_range(0..4) {
            0 => {
                for _ in 0..rng.gen_range(1..8) {
                    let i = rng.gen_range(0..f.len());
                    f[i] ^= 1 << rng.gen_range(0..8);
                }
            }
            1 => f.truncate(rng.gen_range(0..f.len())),
            2 => f.extend((0..rng.gen_range(1..64)).map(|_| rng.gen::<u8>())),
            _ => {
                let other = &corpus[rng.gen_range(0..corpus.len())];
                let cut = rng.gen_range(0..f.len().min(other.len()).max(1));
                f.truncate(cut);
                f.extend_from_slice(&other[cut.min(other.len())..]);
            }
        }
        inject(&s.net, &f);
        s.alice.pump().unwrap(); // never an error and never a panic, whatever arrives
    }
    assert_eq!(
        s.alice.contacts().len(),
        2,
        "mutations created or removed a contact"
    );
    // The genuine conversation still works afterwards.
    s.bob.send(&alice_id, "still talking").unwrap();
    s.alice.pump().unwrap();
    assert!(texts(&s.alice, &s.bob_id).contains(&"still talking".to_string()));
    // Nothing was written outside the media store.
    let stray: Vec<_> = std::fs::read_dir(base.join("a"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "nightdrop-media" && n != "nightdrop-open")
        .collect();
    assert!(stray.is_empty(), "files outside the store: {stray:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn hostile_plaintexts_sealed_by_a_real_contact_stay_within_every_bound() {
    // The mutation test above mostly stops at the Olm check. This one gets past it: a genuinely
    // paired contact seals garbage of every shape on its real session, for every frame kind that
    // carries content, and what Alice keeps must stay within every bound in docs/trust-boundaries.md.
    let mut s = scene();
    let key: StoreKey = [6u8; 32];
    let base = std::env::temp_dir().join(format!("nightdrop-hostile-pt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    s.alice.set_media_store(
        base.join("a/nightdrop-media")
            .to_string_lossy()
            .into_owned(),
        key,
    );
    let mut rng = rand::rngs::StdRng::seed_from_u64(0x0d_2026_1014);
    let shapes: [fn(&mut rand::rngs::StdRng) -> Vec<u8>; 5] = [
        |r| (0..r.gen_range(0..64)).map(|_| r.gen::<u8>()).collect(),
        |r| vec![b'A'; r.gen_range(0..300_000)],
        |r| {
            // A field-packed envelope with lying lengths (what take_field parses).
            let mut v = (r.gen::<u32>()).to_be_bytes().to_vec();
            v.extend((0..r.gen_range(0..32)).map(|_| r.gen::<u8>()));
            v
        },
        |r| {
            pack_media(
                "tid",
                "video",
                "video/x\\..\\evil.bat",
                &vec![0u8; r.gen_range(0..64)],
            )
        },
        |r| pack_edit(&"z".repeat(r.gen_range(0..400)), "edited"),
    ];
    for i in 0..1500 {
        let pt = shapes[rng.gen_range(0..shapes.len())](&mut rng);
        let message = sealed_by_bob(&mut s, &pt);
        let from = s.bob_id.clone();
        let frame = match rng.gen_range(0..9) {
            0 => Frame::Message {
                from,
                id: format!("m{i}"),
                message,
            },
            1 => Frame::Name { from, message },
            2 => Frame::Address { from, message },
            3 => Frame::RelaySet { from, message },
            4 => Frame::Edit { from, message },
            5 => Frame::Media { from, message },
            6 => Frame::MediaIncoming { from, message },
            7 => Frame::Burn {
                from,
                id: format!("b{i}"),
                message,
            },
            _ => Frame::Disappearing { from, message },
        };
        inject(&s.net, &wire::encode(&frame));
        s.alice.pump().unwrap();
    }
    assert_eq!(s.alice.contacts().len(), 2);
    let chat = &s.alice.chats[&s.bob_id];
    assert!(limits::sane_address(&chat.peer_address) || chat.peer_address.is_empty());
    assert!(chat.contact.their_name.chars().count() <= MAX_NAME_CHARS);
    for m in &chat.history {
        assert!(
            m.text.len() <= limits::MAX_TEXT_BYTES + 8,
            "text of {} bytes kept",
            m.text.len()
        );
        assert!(
            m.msg_id.is_empty() || limits::sane_id(&m.msg_id),
            "id {:?}",
            m.msg_id
        );
        assert!(
            m.transfer_id.is_empty() || limits::sane_id(&m.transfer_id),
            "transfer id {:?}",
            m.transfer_id
        );
        assert!(
            !m.mime.contains('\\') && !m.mime.contains(".."),
            "mime {:?}",
            m.mime
        );
    }
    let relays = s.alice.peer_relay_set(&s.bob_id).unwrap_or_default();
    assert!(relays.relays.len() <= MAX_RELAYS);
    // And the conversation still works.
    let alice_id = s.alice_id.clone();
    s.bob.send(&alice_id, "after the storm").unwrap();
    s.alice.pump().unwrap();
    assert!(texts(&s.alice, &s.bob_id).contains(&"after the storm".to_string()));
    let _ = std::fs::remove_dir_all(&base);
}

/// A contact can add only so much in a day (`limits::Allowance`): past the allowance their
/// messages are dropped and the chat gets one notice (self-audit 2026-10-17, P10). Dropped ones are
/// queued for a receipt by the same line as kept ones, so the sender stops re-sending them.
#[test]
fn a_flooding_contact_is_cut_off_at_the_daily_allowance_with_one_notice() {
    let mut s = scene();
    let max = super::limits::MAX_MESSAGES_PER_DAY as usize;
    for i in 0..max + 5 {
        let message = sealed_by_bob(&mut s, format!("flood {i}").as_bytes());
        let frame = wire::encode(&Frame::Message {
            from: s.bob_id.clone(),
            id: format!("m{i:07}"),
            message,
        });
        s.net.endpoint("bob").send("alice", &frame).unwrap();
    }
    s.alice.pump().unwrap();
    let history = s.alice.messages(&s.bob_id);
    let kept = history.iter().filter(|m| !m.system && !m.from_me).count();
    let notices = history
        .iter()
        .filter(|m| m.system && m.text == super::limits::OVER_ALLOWANCE_NOTICE)
        .count();
    assert_eq!(kept, max);
    assert_eq!(notices, 1);
    // Carol is a different contact with her own allowance.
    s.carol.send(&s.alice_id.clone(), "still here").unwrap();
    s.alice.pump().unwrap();
    assert!(texts(&s.alice, &s.carol_id).contains(&"still here".to_string()));
}

/// The preview sent ahead of a video is a small JPEG; anything bigger is not kept (it used to be
/// stored at any size up to a frame).
#[test]
fn an_oversized_video_preview_is_not_stored() {
    let mut s = scene();
    let big = vec![0xAB; super::limits::MAX_THUMB_BYTES + 1];
    let envelope = pack_media_incoming("T1", "video", "video/mp4", 1000, &big);
    let message = sealed_by_bob(&mut s, &envelope);
    let frame = wire::encode(&Frame::MediaIncoming {
        from: s.bob_id.clone(),
        message,
    });
    s.net.endpoint("bob").send("alice", &frame).unwrap();
    s.alice.pump().unwrap();
    let placeholder = s
        .alice
        .messages(&s.bob_id)
        .into_iter()
        .find(|m| m.transfer_id == "T1")
        .expect("the placeholder is still shown");
    assert!(placeholder.thumb_id.is_empty(), "no preview stored");
}
