//! Nearby in the node (`docs/design/nearby.md`): the per-chat switch, its announcement on
//! `Frame::Captures`, the peer's, and the routes handed to the transport.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};

use super::frames::within_allowance;
use super::{Announce, ChatMessage, Frame, Node, OutboundDone, MARK_NEARBY_PREFIX};
use crate::transport::nearby::{is_private, valid_port, valid_token, NearbyRoute};

/// What the peer announced, parsed (§4).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PeerNearby {
    pub seq: u64,
    pub on: bool,
    pub at: Option<(SocketAddrV4, String)>,
}

/// Parse the body after [`MARK_NEARBY_PREFIX`]. `None` for anything malformed, or an address that
/// is not private: a public one would have us connect to it outside Tor.
pub(super) fn parse_body(body: &[u8]) -> Option<PeerNearby> {
    let text = std::str::from_utf8(body).ok()?;
    let parts: Vec<&str> = text.split(':').collect();
    let seq = parts.first()?.parse::<u64>().ok()?;
    match parts.as_slice() {
        [_, "0"] => Some(PeerNearby {
            seq,
            on: false,
            at: None,
        }),
        [_, "1"] => Some(PeerNearby {
            seq,
            on: true,
            at: None,
        }),
        [_, "1", ip, port, token] => {
            let ip: Ipv4Addr = ip.parse().ok()?;
            let port: u16 = port.parse().ok().filter(|p| valid_port(*p))?;
            // Loopback stands in for a local network in tests only (`cfg!(test)` is false in the
            // app), so a contact can never point us at this device's own services.
            let local = is_private(ip) || (cfg!(test) && ip.is_loopback());
            (local && valid_token(token)).then(|| PeerNearby {
                seq,
                on: true,
                at: Some((SocketAddrV4::new(ip, port), token.to_string())),
            })
        }
        _ => None,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn fresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut bytes);
    super::encode_secret(&bytes)
}

impl Node {
    /// Turn "Also connect on the same Wi-Fi" on or off for one chat. The contact is told; the local
    /// path is used only once they have turned it on too. False for an unknown or closed chat.
    pub fn set_nearby(&mut self, contact_id: &str, on: bool) -> bool {
        let Some(chat) = self.chats.get_mut(contact_id) else {
            return false;
        };
        if chat.closed || !chat.authorized {
            return false;
        }
        if chat.nearby_token.is_some() == on {
            return true;
        }
        // A fresh token each time it goes on: one held from before stops working.
        chat.nearby_token = on.then(fresh_token);
        chat.nearby_seq = now_ms().max(chat.nearby_seq + 1);
        let note = if on {
            "📶 You turned on \"Also connect on the same Wi-Fi\". They were told; it is used once \
             they turn it on too and you are on the same network."
        } else {
            "📶 You turned off \"Also connect on the same Wi-Fi\". They were told."
        };
        chat.history.push(ChatMessage::system(note.to_string()));
        self.nearby_announced.remove(contact_id);
        self.dirty = true;
        self.nearby_tick();
        true
    }

    /// Run every tick: hand the transport the current routes (which starts or stops listening),
    /// notice a new local address, and announce wherever the contact has not confirmed ours.
    pub fn nearby_tick(&mut self) {
        self.push_nearby_routes();
        self.transport.nearby_probe();
        let address = self.transport.nearby_address();
        if address != self.nearby_address {
            self.nearby_address = address;
            let seq = now_ms();
            for chat in self.chats.values_mut().filter(|c| c.nearby_token.is_some()) {
                chat.nearby_seq = seq.max(chat.nearby_seq + 1);
            }
            self.nearby_announced.clear();
            self.dirty = true;
        }
        let now = crate::api::now_secs();
        let due: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| {
                c.authorized
                    && !c.closed
                    && c.nearby_seq > c.told_nearby_seq
                    && !self.nearby_announced.contains_key(id.as_str())
                    && self.may_retry(id, now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            self.announce_nearby_to(&id);
        }
    }

    fn push_nearby_routes(&self) {
        let routes: HashMap<String, NearbyRoute> = self
            .chats
            .values()
            .filter(|c| !c.closed)
            .filter_map(|c| {
                let our_token = c.nearby_token.clone()?;
                Some((
                    c.peer_address.clone(),
                    NearbyRoute {
                        our_token,
                        peer: c.peer_nearby.clone(),
                    },
                ))
            })
            .collect();
        self.transport.set_nearby(routes);
    }

    fn nearby_body(&self, contact_id: &str) -> Option<(u64, String)> {
        let chat = self.chats.get(contact_id)?;
        let body = match (&chat.nearby_token, self.nearby_address) {
            (None, _) => format!("{}:0", chat.nearby_seq),
            (Some(_), None) => format!("{}:1", chat.nearby_seq),
            (Some(token), Some(at)) => {
                format!("{}:1:{}:{}:{token}", chat.nearby_seq, at.ip(), at.port())
            }
        };
        Some((chat.nearby_seq, body))
    }

    fn announce_nearby_to(&mut self, contact_id: &str) {
        let Some((seq, body)) = self.nearby_body(contact_id) else {
            return;
        };
        let marker = [MARK_NEARBY_PREFIX, body.as_bytes()].concat();
        if let Some((addr, frame)) = self.authed_control(contact_id, &marker, |from, message| {
            Frame::Captures { from, message }
        }) {
            self.nearby_announced.insert(contact_id.to_string(), seq);
            let done = OutboundDone::Announced(Announce::Nearby(seq), contact_id.to_string());
            let _ = self.deliver_with(&addr, contact_id, &frame, false, done);
        }
    }

    /// A contact's nearby announcement (already decrypted and authenticated). Returns whether
    /// anything changed, for the UI.
    pub(super) fn receive_nearby(&mut self, from: &str, body: &[u8]) -> bool {
        let Some(peer) = parse_body(body) else {
            return false;
        };
        let Some(chat) = self.chats.get_mut(from) else {
            return false;
        };
        chat.mark_alive();
        if peer.seq <= chat.peer_nearby_seq {
            return false;
        }
        chat.peer_nearby_seq = peer.seq;
        let changed = chat.peer_nearby_on != peer.on || chat.peer_nearby != peer.at;
        // A change costs a save and a route update here: counted, like the other control frames.
        if !changed || !within_allowance(chat, 0) {
            return false;
        }
        if peer.on && !chat.peer_nearby_on && chat.nearby_token.is_none() {
            chat.history.push(ChatMessage::system(
                "📶 They'd like to also connect on the same Wi-Fi. Turn it on in this chat's menu \
                 if you agree."
                    .to_string(),
            ));
        }
        chat.peer_nearby_on = peer.on;
        chat.peer_nearby = peer.at;
        self.dirty = true;
        self.push_nearby_routes();
        true
    }

    /// The contact needs to hear our nearby state again (they updated, or approved us).
    pub(super) fn forget_nearby_told(&mut self, contact_id: &str) {
        if let Some(chat) = self.chats.get_mut(contact_id) {
            chat.told_nearby_seq = 0;
        }
        self.nearby_announced.remove(contact_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> String {
        "A".repeat(crate::transport::nearby::TOKEN_CHARS)
    }

    #[test]
    fn announcements_parse_strictly() {
        assert_eq!(
            parse_body(b"7:0"),
            Some(PeerNearby {
                seq: 7,
                on: false,
                at: None
            })
        );
        assert_eq!(
            parse_body(b"8:1"),
            Some(PeerNearby {
                seq: 8,
                on: true,
                at: None
            })
        );
        let on = format!("9:1:192.168.1.20:40000:{}", token());
        assert_eq!(
            parse_body(on.as_bytes()).unwrap().at,
            Some(("192.168.1.20:40000".parse().unwrap(), token()))
        );
        // A public address, port 0, a bad token, junk: all refused.
        for bad in [
            format!("9:1:8.8.8.8:40000:{}", token()),
            format!("9:1:192.168.1.20:0:{}", token()),
            format!("9:1:192.168.1.20:9100:{}", token()),
            "9:1:192.168.1.20:40000:short".to_string(),
            "9:2".to_string(),
            "x:0".to_string(),
            format!("9:1:192.168.1.20:40000:{}:extra", token()),
        ] {
            assert_eq!(parse_body(bad.as_bytes()), None, "{bad}");
        }
    }
}
