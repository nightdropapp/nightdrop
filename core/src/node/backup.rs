//! [`Node`] persistence & backup: export/restore, Lite/Full + scoped backups, merge,
//! onion-keystore bundling, and server backup. Split out of `node.rs`.
use super::*;

impl Node {
    /// Capture the full device state for at-rest persistence (`storage::`). Identity and
    /// sessions are vodozemac-encrypted under `key`; the rest is sealed by the caller.
    // App-level save/load is wired once the real transport replaces the demo peer; the
    // mechanism is exercised by tests now.
    #[allow(dead_code)]
    pub fn export(&self, key: &StoreKey) -> PersistedState {
        let account_pickle = self.identity.account().pickle().encrypt(key);
        let chats = self
            .chats
            .values()
            .map(|chat| PersistedChat {
                contact_id: chat.contact.id.clone(),
                peer_address: chat.peer_address.clone(),
                their_name: chat.contact.their_name.clone(),
                my_name: chat.contact.my_name.clone(),
                remote_storage: chat.contact.remote_storage,
                disappearing_secs: chat.contact.disappearing_secs,
                session_pickle: chat.session.pickle().encrypt(key),
                history: chat
                    .history
                    .iter()
                    .map(|m| PersistedMessage {
                        from_me: m.from_me,
                        text: m.text.clone(),
                        system: m.system,
                        msg_id: m.msg_id.clone(),
                        edited: m.edited,
                        at: m.at,
                        delivery: m.delivery.clone(),
                        kind: m.kind.clone(),
                        mime: m.mime.clone(),
                        media_id: m.media_id.clone(),
                        media_size: m.media_size,
                        transfer_id: m.transfer_id.clone(),
                        thumb_id: m.thumb_id.clone(),
                    })
                    .collect(),
                closed: chat.closed,
                backed_up: chat.contact.backed_up,
                peer_backed_up: chat.contact.peer_backed_up,
                verified: chat.contact.verified,
                peer_verified: chat.contact.peer_verified,
                peer_relays: chat.contact.peer_relays.clone(),
                // Persist recall receipts for still-queued messages so an edit/unsend can pull an
                // undelivered blob off the relay even after a restart (§1.1). Flatten the
                // by-msg_id map into a list carrying its target.
                queued_receipts: chat
                    .relay_receipts
                    .iter()
                    .flat_map(|(target, receipts)| {
                        receipts
                            .iter()
                            .map(move |r| crate::storage::PersistedReceipt {
                                target_msg_id: target.clone(),
                                relay_addr: r.relay_addr.clone(),
                                msg_id: r.msg_id.clone(),
                                delete_token: r.delete_token.clone(),
                            })
                    })
                    .collect(),
                last_seen_unix: chat.last_seen,
                local_name: chat.local_name.clone(),
            })
            .collect();
        PersistedState {
            account_pickle,
            address: self.address(),
            chats,
            media: Vec::new(),      // populated only for backups (see `backup`)
            onion_keys: Vec::new(), // populated only for backups (see `backup`)
            my_relays: self.my_relays.clone(),
            discovered_relays: self.discovered_relays.clone(),
            directory_version: self.directory_version,
            pending_control: self.export_pending_control(),
        }
    }

    /// Serialize the undelivered chat-delete `Closed` queue (§11.6) for persistence: the sealed
    /// frame bytes go out base64-encoded (the whole state is sealed around them).
    fn export_pending_control(&self) -> Vec<crate::storage::PersistedPendingControl> {
        use base64::Engine as _;
        self.pending_control
            .iter()
            .map(|p| crate::storage::PersistedPendingControl {
                recipient_ik: p.recipient_ik.clone(),
                peer_address: p.peer_address.clone(),
                relays: p.relays.clone(),
                bytes: base64::engine::general_purpose::STANDARD.encode(&p.bytes),
            })
            .collect()
    }

    /// Rebuild the undelivered chat-delete `Closed` queue (§11.6) on restore; entries whose bytes
    /// don't base64-decode are dropped rather than aborting the whole restore.
    fn import_pending_control(
        persisted: &[crate::storage::PersistedPendingControl],
    ) -> Vec<super::PendingControl> {
        use base64::Engine as _;
        persisted
            .iter()
            .filter_map(|p| {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(p.bytes.as_bytes())
                    .ok()?;
                Some(super::PendingControl {
                    recipient_ik: p.recipient_ik.clone(),
                    peer_address: p.peer_address.clone(),
                    relays: p.relays.clone(),
                    bytes,
                })
            })
            .collect()
    }

    /// Read arti's onion keystore (`<state_dir>/arti-state/keystore`) into base64 [`PersistedFile`]s
    /// so a backup reproduces the same `.onion`. Empty if Tor state isn't known or absent.
    fn collect_onion_keys(&self) -> Vec<crate::storage::PersistedFile> {
        use base64::Engine as _;
        let Some(base) = &self.tor_state_dir else {
            return Vec::new();
        };
        let root = std::path::Path::new(base)
            .join("arti-state")
            .join("keystore");
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(bytes) = std::fs::read(&p) {
                    if let Ok(rel) = p.strip_prefix(&root) {
                        out.push(crate::storage::PersistedFile {
                            path: rel.to_string_lossy().into_owned(),
                            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                        });
                    }
                }
            }
        }
        out
    }

    /// Write the onion keystore from a restored backup into `<state_dir>/arti-state/keystore`
    /// (key files mode 0600) so the next Tor bootstrap publishes the same `.onion`. Must run
    /// *before* the Tor transport bootstraps.
    #[allow(dead_code)] // used by the Tor-backed api path (`--features tor`)
    pub fn write_onion_keys(
        files: &[crate::storage::PersistedFile],
        state_dir: &str,
    ) -> Result<()> {
        use base64::Engine as _;
        let root = std::path::Path::new(state_dir)
            .join("arti-state")
            .join("keystore");
        for f in files {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(f.data.as_bytes())
                .map_err(|_| anyhow::anyhow!("bad onion-key encoding"))?;
            let dest = root.join(&f.path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, &bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
            }
        }
        Ok(())
    }

    /// Collect every attachment referenced by chat history (images, videos, and video
    /// thumbnails), decrypted from the media store, so a backup can be self-contained. No-op
    /// (empty) if media storage isn't configured.
    fn collect_media(&self) -> Vec<crate::storage::PersistedMedia> {
        use base64::Engine as _;
        let mut seen = std::collections::HashSet::new();
        let mut ids: Vec<String> = Vec::new();
        for chat in self.chats.values() {
            for m in &chat.history {
                for id in [m.media_id.as_str(), m.thumb_id.as_str()] {
                    if !id.is_empty() && seen.insert(id) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
        let mut out = Vec::new();
        for id in ids {
            if let Ok(bytes) = self.media_bytes(&id) {
                out.push(crate::storage::PersistedMedia {
                    id,
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                });
            }
        }
        out
    }

    /// Rebuild a node from persisted state onto a (fresh) transport, restoring the
    /// identity, every session, and history so the app resumes after a restart.
    #[allow(dead_code)]
    pub fn restore(
        state: &PersistedState,
        transport: Box<dyn Transport>,
        key: &StoreKey,
    ) -> Result<Self> {
        let account = Account::from_pickle(
            AccountPickle::from_encrypted(&state.account_pickle, key)
                .map_err(|e| anyhow::anyhow!("restore account: {e}"))?,
        );
        let mut node = Self::with_identity(LocalIdentity::from_account(account), transport);
        // Backups carry attachments inline; stash them to be written once a media store is set.
        node.pending_media = state.media.clone();
        // Remember the address we last persisted, so startup can detect an onion change (#11).
        node.restored_address = state.address.clone();
        node.my_relays = state.my_relays.clone();
        node.discovered_relays = state.discovered_relays.clone();
        node.directory_version = state.directory_version;
        node.pending_control = Self::import_pending_control(&state.pending_control);
        for chat in &state.chats {
            let session = Session::from_pickle(
                SessionPickle::from_encrypted(&chat.session_pickle, key)
                    .map_err(|e| anyhow::anyhow!("restore session: {e}"))?,
            );
            // Rebuild the recall receipts for still-queued messages (§1.1) so an edit/unsend after
            // this restart can still pull an undelivered blob off the relay.
            let mut relay_receipts: HashMap<String, Vec<QueuedReceipt>> = HashMap::new();
            for pr in &chat.queued_receipts {
                relay_receipts
                    .entry(pr.target_msg_id.clone())
                    .or_default()
                    .push(QueuedReceipt {
                        relay_addr: pr.relay_addr.clone(),
                        msg_id: pr.msg_id.clone(),
                        delete_token: pr.delete_token.clone(),
                    });
            }
            node.chats.insert(
                chat.contact_id.clone(),
                Chat {
                    contact: Contact {
                        id: chat.contact_id.clone(),
                        their_name: chat.their_name.clone(),
                        my_name: chat.my_name.clone(),
                        remote_storage: chat.remote_storage,
                        disappearing_secs: chat.disappearing_secs,
                        backed_up: chat.backed_up,
                        peer_backed_up: chat.peer_backed_up,
                        verified: chat.verified,
                        peer_verified: chat.peer_verified,
                        peer_relays: chat.peer_relays.clone(),
                        remote_storage_healthy: true,
                        last_seen_secs: 0, // these three are filled in `contacts()` from the chat
                        local_name: String::new(),
                        identity_tag: String::new(),
                    },
                    peer_address: chat.peer_address.clone(),
                    session,
                    history: chat
                        .history
                        .iter()
                        .map(|m| ChatMessage {
                            from_me: m.from_me,
                            text: m.text.clone(),
                            system: m.system,
                            kind: m.kind.clone(),
                            mime: m.mime.clone(),
                            media_id: m.media_id.clone(),
                            media_size: m.media_size,
                            transfer_id: m.transfer_id.clone(),
                            thumb_id: m.thumb_id.clone(),
                            delivery: m.delivery.clone(),
                            msg_id: m.msg_id.clone(),
                            edited: m.edited,
                            at: m.at,
                        })
                        .collect(),
                    last_seen: chat.last_seen_unix,
                    local_name: chat.local_name.clone(),
                    authorized: true, // persisted chats were authorized before saving
                    code: None,
                    closed: chat.closed,
                    relay_receipts,
                    remote_storage_healthy: true,
                },
            );
        }
        Ok(node)
    }

    /// Merge a **single-chat scoped backup** ([`backup_chat`](Self::backup_chat)) into this
    /// live identity (#8): bring in each chat the blob carries **without** disturbing our
    /// identity or an active session. A chat we don't have is inserted; one we already have
    /// keeps its (possibly more-advanced) session and only gains the backup's missing history
    /// messages (deduped by `msg_id`). Returns how many messages were added. Any attachments in
    /// the blob are sealed into the media store.
    pub fn merge_from_backup(&mut self, blob: &[u8], password: &str) -> Result<usize> {
        let (state, mut key) = Self::open_backup(blob, password)?;
        let mut added = 0usize;
        for pchat in &state.chats {
            match self.chats.get_mut(&pchat.contact_id) {
                None => {
                    // New chat: rebuild its session and insert it wholesale.
                    let session = match SessionPickle::from_encrypted(&pchat.session_pickle, &key) {
                        Ok(p) => Session::from_pickle(p),
                        Err(_) => continue, // wrong key / corrupt — skip this chat
                    };
                    let history: Vec<ChatMessage> =
                        pchat.history.iter().map(persisted_to_message).collect();
                    added += history.len();
                    self.chats.insert(
                        pchat.contact_id.clone(),
                        Chat {
                            contact: Contact {
                                id: pchat.contact_id.clone(),
                                their_name: pchat.their_name.clone(),
                                my_name: pchat.my_name.clone(),
                                remote_storage: pchat.remote_storage,
                                disappearing_secs: pchat.disappearing_secs,
                                backed_up: pchat.backed_up,
                                peer_backed_up: pchat.peer_backed_up,
                                verified: pchat.verified,
                                peer_verified: pchat.peer_verified,
                                peer_relays: pchat.peer_relays.clone(),
                                remote_storage_healthy: true,
                                last_seen_secs: 0, // these three are filled in `contacts()` from the chat
                                local_name: String::new(),
                                identity_tag: String::new(),
                            },
                            peer_address: pchat.peer_address.clone(),
                            session,
                            history,
                            authorized: true,
                            code: None,
                            closed: pchat.closed,
                            relay_receipts: HashMap::new(),
                            last_seen: pchat.last_seen_unix,
                            local_name: pchat.local_name.clone(),
                            remote_storage_healthy: true,
                        },
                    );
                }
                Some(chat) => {
                    // Existing chat: keep the live session; fold in only messages we're missing.
                    let have: std::collections::HashSet<String> = chat
                        .history
                        .iter()
                        .filter(|m| !m.msg_id.is_empty())
                        .map(|m| m.msg_id.clone())
                        .collect();
                    for pm in &pchat.history {
                        // Only merge identifiable messages we don't already have (can't dedup
                        // ones with no id, so skip those to avoid duplicates).
                        if pm.msg_id.is_empty() || have.contains(&pm.msg_id) {
                            continue;
                        }
                        chat.history.push(persisted_to_message(pm));
                        added += 1;
                    }
                    chat.history.sort_by_key(|m| m.at);
                }
            }
        }
        // Seal any carried attachments into the store so the merged history resolves them.
        if let Some((dir, mkey)) = &self.media_store {
            use base64::Engine as _;
            for item in &state.media {
                if let Ok(bytes) =
                    base64::engine::general_purpose::STANDARD.decode(item.data.as_bytes())
                {
                    if let Ok(sealed) = crate::storage::seal(mkey, &bytes) {
                        let _ = std::fs::write(format!("{dir}/{}.bin", item.id), sealed);
                    }
                }
            }
        }
        key.zeroize();
        Ok(added)
    }

    // -------- Backup & restore (§7) --------

    /// Produce a **portable, password-encrypted backup** (§7a). The key is derived from
    /// `password` with Argon2 (random salt prepended); identity, sessions, and history are
    /// inside. The same bytes work for **device-to-device** transfer if `password` is a
    /// PAKE-derived transfer secret instead of a user password (§7b).
    #[allow(dead_code)]
    pub fn backup(&self, password: &str) -> Result<Vec<u8>> {
        self.backup_with_mode(password, true)
    }

    /// As [`backup`](Self::backup), choosing the **content matrix** (§11.5, #7):
    ///
    /// * **Full** (`full = true`): identity + onion keystore + contacts + session pickles **plus
    ///   message history and media** — a complete clone of the device.
    /// * **Lite** (`full = false`): identity + onion keystore + contacts + session pickles only —
    ///   enough to *keep chatting* (the ratchet survives) and reappear on the same `.onion`, but
    ///   **no message history and no attachments** leave the device. The privacy-preferring
    ///   default the UI offers.
    ///
    /// Both modes bundle the onion keystore so identity survives a device change; Lite simply
    /// drops each chat's history and skips media bundling.
    pub fn backup_with_mode(&self, password: &str, full: bool) -> Result<Vec<u8>> {
        let mut salt = [0u8; BACKUP_SALT_LEN];
        rand::Rng::fill(&mut rand::thread_rng(), &mut salt);
        let mut key = crate::storage::derive_key(password, &salt)?;
        let mut state = self.export(&key);
        if full {
            state.media = self.collect_media();
        } else {
            // Lite: strip message history; keep names/sessions so the chat still works.
            for chat in &mut state.chats {
                chat.history.clear();
            }
        }
        state.onion_keys = self.collect_onion_keys();
        let sealed = crate::storage::seal(&key, &serde_json::to_vec(&state)?)?;
        key.zeroize();
        let mut out = salt.to_vec();
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// A **single-chat scoped backup** (§11.7 phase 4, #8): the same encrypted blob as
    /// [`backup_with_mode`] but carrying **only** the one chat (its contact + session pickle,
    /// plus history + media when `full`). Meant to be **merged** into an existing identity via
    /// [`merge_from_backup`](Self::merge_from_backup) — e.g. to bring one conversation's history
    /// onto a device restored from a Lite backup. Errors if the contact is unknown.
    pub fn backup_chat(&self, contact_id: &str, password: &str, full: bool) -> Result<Vec<u8>> {
        if !self.chats.contains_key(contact_id) {
            anyhow::bail!("unknown contact");
        }
        let mut salt = [0u8; BACKUP_SALT_LEN];
        rand::Rng::fill(&mut rand::thread_rng(), &mut salt);
        let mut key = crate::storage::derive_key(password, &salt)?;
        let mut state = self.export(&key);
        state.chats.retain(|c| c.contact_id == contact_id);
        if full {
            state.media = self.collect_media_for(contact_id);
        } else {
            for chat in &mut state.chats {
                chat.history.clear();
            }
        }
        // A scoped backup restores identity/onion only via a *full-device* restore; the merge
        // path ignores them, so leave the onion keystore out to keep the blob small.
        let sealed = crate::storage::seal(&key, &serde_json::to_vec(&state)?)?;
        key.zeroize();
        let mut out = salt.to_vec();
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// Attachments referenced by one chat's history (for a scoped backup).
    fn collect_media_for(&self, contact_id: &str) -> Vec<crate::storage::PersistedMedia> {
        use base64::Engine as _;
        let Some(chat) = self.chats.get(contact_id) else {
            return Vec::new();
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for m in &chat.history {
            for id in [m.media_id.as_str(), m.thumb_id.as_str()] {
                if !id.is_empty() && seen.insert(id.to_string()) {
                    if let Ok(bytes) = self.media_bytes(id) {
                        out.push(crate::storage::PersistedMedia {
                            id: id.to_string(),
                            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                        });
                    }
                }
            }
        }
        out
    }

    /// Decrypt a password-encrypted backup into its [`PersistedState`] + the derived key, without
    /// binding it to a transport — so the caller can restore the onion keystore (and thus the
    /// `.onion`) *before* bootstrapping Tor, then call [`restore`](Self::restore) with the key.
    pub fn open_backup(blob: &[u8], password: &str) -> Result<(PersistedState, StoreKey)> {
        if blob.len() < BACKUP_SALT_LEN {
            anyhow::bail!("backup too short");
        }
        let (salt, sealed) = blob.split_at(BACKUP_SALT_LEN);
        let key = crate::storage::derive_key(password, salt)?;
        let json = crate::storage::open(&key, sealed)?;
        let state: PersistedState = serde_json::from_slice(&json)?;
        Ok((state, key))
    }

    /// Restore a node from a password-encrypted backup onto a fresh transport (§7).
    #[allow(dead_code)]
    pub fn restore_from_backup(
        blob: &[u8],
        password: &str,
        transport: Box<dyn Transport>,
    ) -> Result<Self> {
        let (state, mut key) = Self::open_backup(blob, password)?;
        let node = Self::restore(&state, transport, &key);
        key.zeroize();
        node
    }

    /// Upload an encrypted backup to the relay for fresh-device recovery (§7c). Retention
    /// defaults to 24h and is capped at 36h. The relay only ever holds the opaque blob;
    /// the password (and thus the contents) never reach it.
    #[allow(dead_code)]
    pub fn server_backup(
        &self,
        relay: &RelayClient,
        password: &str,
        ttl: Option<Duration>,
        full: bool,
    ) -> Result<()> {
        let ttl = ttl
            .unwrap_or(SERVER_BACKUP_DEFAULT_TTL)
            .min(SERVER_BACKUP_MAX_TTL);
        let blob = self.backup_with_mode(password, full)?;
        relay
            .post(&backup_handle(password)?, &blob, ttl)
            .map(|_| ())
    }
}

impl Node {
    /// Recover from a server backup on a fresh device using the recovery password (§7c).
    #[allow(dead_code)]
    pub fn restore_from_server(
        relay: &RelayClient,
        password: &str,
        transport: Box<dyn Transport>,
    ) -> Result<Self> {
        let blob = relay
            .take(&backup_handle(password)?)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                anyhow::anyhow!("no server backup found (expired or wrong password?)")
            })?;
        Self::restore_from_backup(&blob, password, transport)
    }
}
