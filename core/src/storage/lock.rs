//! Passphrase lock for the at-rest store key (§app-lock).
//!
//! Without a lock the 32-byte store key sits in the platform keystore, so anything running as the
//! app on an unlocked device can read the whole history. A lock removes that copy: the key exists
//! only wrapped under a key derived from something the user knows, and only in memory once
//! entered.
//!
//! Threat model, stated plainly because it drives the parameters below:
//!
//! * A **passphrase** with real entropy makes the wrapped blob useless to someone who images the
//!   device. This is the case the feature is for.
//! * A short **PIN** does not. ~20 bits cannot be rescued by any key-derivation cost — an offline
//!   attacker simply tries all of them. A PIN is only meaningfully protected by hardware that
//!   throttles attempts (Android Keystore with user-authentication binding), which is a different
//!   mechanism to this one. Callers must say so rather than implying a PIN is equivalent.
//!
//! So the cost here is set well above `Argon2::default()` (which `SECURITY.md` deliberately
//! restricts to *randomly generated* backup passwords), to buy what can be bought.

use crate::storage::{open, seal, StoreKey};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Argon2id cost for a **user-chosen** secret: 64 MiB, 3 passes. Roughly half a second on a
/// mid-range phone, which is tolerable once per unlock and about 25× the memory of the crate
/// default used for random backup passwords. Persisted per lock so these can be raised later
/// without stranding an existing lock file.
const M_COST_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;

/// Filename beside the state blob. Contains no secret: salts, the cost, and wrapped keys that are
/// worthless without the passphrase.
pub const LOCK_FILE: &str = "store-key.lock";

/// Current format version. v1 (single slot, flat fields) is still readable — see [`read`].
const FORMAT_V: u8 = 2;

/// One wrapped key with its own salt. Separate salts per slot mean an offline attacker must run a
/// derivation **per slot** to test a candidate secret, instead of one derivation testing both.
#[derive(Serialize, Deserialize, Clone)]
struct Slot {
    salt: String,
    /// A key sealed under the derived key. Sealing (not raw XOR) is what makes a wrong passphrase
    /// fail loudly instead of yielding a plausible-looking wrong key.
    wrapped: String,
}

/// v2: a normal slot and a duress slot (#3). The duress slot is **always present** — when duress
/// is unarmed it holds a decoy, so the file never reveals whether a duress secret exists. See
/// `docs/design/duress-wipe.md` §3.
#[derive(Serialize, Deserialize)]
struct LockV2 {
    v: u8,
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
    normal: Slot,
    duress: Slot,
    /// Whether the duress slot is real or a decoy, sealed **under the store key**.
    ///
    /// The slots themselves are indistinguishable on purpose (§3), which means the app cannot tell
    /// either — and a user who can't see whether their wipe code is armed may believe they have one
    /// when they don't, and find out while being coerced. That is worse than the leak avoided.
    ///
    /// Sealing it under the store key splits the difference exactly: someone who *images* the
    /// device still learns nothing, because reading this requires the key the lock protects; the
    /// app, already unlocked, reads it freely. Always written, always the same size.
    ///
    /// `default` is **load-bearing**: this field was added after v2 shipped, and without it every
    /// lock file written by the earlier build fails to deserialize — which surfaces as "your PIN is
    /// wrong" for a PIN that is perfectly correct, with no way in. A new field on an existing
    /// format version must never be required. Missing reads as "not armed"; see
    /// `duress_armed` for why that is the right default.
    #[serde(default)]
    armed: String,
}

/// v1: one slot, fields inline. Written by builds before the duress wipe existed; still read so an
/// existing lock is never stranded.
#[derive(Deserialize)]
struct LockV1 {
    salt: String,
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
    wrapped: String,
}

/// What a secret turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Opened {
    /// The normal secret: here is the store key.
    Normal(StoreKey),
    /// The duress secret (#3). Carries **no key** — presenting it never yields readable data. The
    /// caller must wipe; see `docs/design/duress-wipe.md` §5.
    Duress,
}

/// A slot nobody can open: 32 random bytes sealed under a key that was never derived from any
/// secret. Indistinguishable from a real slot, which is the entire point — the on-disk shape must
/// not say whether duress is armed.
fn decoy_slot() -> Result<Slot> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut salt = [0u8; 16];
    let mut kek: StoreKey = [0u8; 32];
    let mut filler: StoreKey = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut salt[..]);
    rand::Rng::fill(&mut rand::thread_rng(), &mut kek[..]);
    rand::Rng::fill(&mut rand::thread_rng(), &mut filler[..]);
    Ok(Slot {
        salt: b64.encode(salt),
        wrapped: b64.encode(seal(&kek, &filler)?),
    })
}

/// Seal the armed flag under the store key. One byte, so the ciphertext is a fixed size either way
/// and its length says nothing.
fn seal_armed(key: &StoreKey, armed: bool) -> Result<String> {
    use base64::Engine as _;
    let byte = [u8::from(armed)];
    Ok(base64::engine::general_purpose::STANDARD.encode(seal(key, &byte)?))
}

/// Wrap `key` under `secret` in a fresh slot.
fn wrap_slot(key: &StoreKey, secret: &str) -> Result<Slot> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut salt = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut salt[..]);
    let kek = derive(secret, &salt, M_COST_KIB, T_COST, P_COST)?;
    Ok(Slot {
        salt: b64.encode(salt),
        wrapped: b64.encode(seal(&kek, key)?),
    })
}

/// Try `secret` against one slot. `None` means "not this one" — never distinguished from a
/// malformed slot, so a caller can't learn which failure it was.
fn try_slot(slot: &Slot, secret: &str, m: u32, t: u32, p: u32) -> Option<StoreKey> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let salt = b64.decode(&slot.salt).ok()?;
    let kek = derive(secret, &salt, m, t, p).ok()?;
    let wrapped = b64.decode(&slot.wrapped).ok()?;
    open(&kek, &wrapped).ok()?.try_into().ok()
}

/// Read the lock file in either format, normalising v1 into the v2 shape (its missing duress slot
/// becomes a decoy, so every later step is uniform).
fn read(dir: &str) -> Result<LockV2> {
    let raw = std::fs::read(path(dir)).context("this store is not locked")?;
    let value: serde_json::Value =
        serde_json::from_slice(&raw).context("lock file is unreadable")?;
    match value.get("v").and_then(|v| v.as_u64()) {
        Some(1) => {
            let old: LockV1 = serde_json::from_value(value).context("lock file is unreadable")?;
            Ok(LockV2 {
                v: 1,
                m_cost_kib: old.m_cost_kib,
                t_cost: old.t_cost,
                p_cost: old.p_cost,
                normal: Slot {
                    salt: old.salt,
                    wrapped: old.wrapped,
                },
                duress: decoy_slot()?,
                // A v1 lock predates duress entirely, so it cannot be armed. The empty string
                // reads as "not armed" without needing the store key.
                armed: String::new(),
            })
        }
        Some(2) => serde_json::from_value(value).context("lock file is unreadable"),
        _ => anyhow::bail!("lock file was written by a newer version of the app"),
    }
}

/// Write `lock` atomically, so a crash mid-write cannot leave a truncated file — which would make
/// the store permanently unopenable.
fn write(dir: &str, lock: &LockV2) -> Result<()> {
    std::fs::create_dir_all(dir).ok();
    let tmp = path(dir).with_extension("lock.tmp");
    std::fs::write(&tmp, serde_json::to_vec(lock)?)?;
    std::fs::rename(&tmp, path(dir))?;
    Ok(())
}

fn derive(passphrase: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<StoreKey> {
    let params = argon2::Params::new(m, t, p, Some(32))
        .map_err(|e| anyhow::anyhow!("bad argon2 parameters: {e}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("key derivation failed: {e}"))?;
    Ok(key)
}

fn path(dir: &str) -> std::path::PathBuf {
    std::path::Path::new(dir).join(LOCK_FILE)
}

/// Whether this store is passphrase-locked.
pub fn is_locked(dir: &str) -> bool {
    path(dir).exists()
}

/// Wrap `key` under `passphrase`. The caller must delete any keystore copy afterwards, or the lock
/// buys nothing — the whole point is that the key stops being retrievable without the passphrase.
pub fn set_passphrase(dir: &str, key: &StoreKey, passphrase: &str) -> Result<()> {
    if passphrase.is_empty() {
        anyhow::bail!("passphrase must not be empty");
    }
    // The duress slot starts as a decoy: unarmed, but present, so the file shape is identical
    // whether or not a duress secret is ever set (#3, `duress-wipe.md` §3).
    write(
        dir,
        &LockV2 {
            v: FORMAT_V,
            m_cost_kib: M_COST_KIB,
            t_cost: T_COST,
            p_cost: P_COST,
            normal: wrap_slot(key, passphrase)?,
            duress: decoy_slot()?,
            armed: seal_armed(key, false)?,
        },
    )
}

/// Whether a real duress secret is armed. Needs the store key, which is what keeps this readable to
/// the unlocked app and useless to someone holding only an image of the device.
///
/// False for anything unreadable — a v1 lock, a wrong key, a damaged field — because the honest
/// default is "you have no wipe code", never a claim of protection that isn't there.
pub fn duress_armed(dir: &str, key: &StoreKey) -> bool {
    use base64::Engine as _;
    let Ok(lock) = read(dir) else {
        return false;
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    let Ok(blob) = b64.decode(&lock.armed) else {
        return false;
    };
    matches!(open(key, &blob), Ok(plain) if plain == [1u8])
}

/// Arm (or replace) the **duress** secret (#3). Requires the normal secret, so an adversary who has
/// already coerced one unlock cannot quietly disarm it.
///
/// The duress slot deliberately wraps **random bytes, not the store key**: opening it is a signal
/// to wipe, and there is no path where the duress secret yields readable data.
pub fn set_duress(dir: &str, passphrase: &str, duress: &str) -> Result<()> {
    if duress.is_empty() {
        anyhow::bail!("duress secret must not be empty");
    }
    let mut lock = read(dir)?;
    let m = lock.m_cost_kib;
    let (t, p) = (lock.t_cost, lock.p_cost);
    // Proves knowledge of the normal secret, and hands back the store key that the armed flag is
    // sealed under.
    let key = try_slot(&lock.normal, passphrase, m, t, p)
        .ok_or_else(|| anyhow::anyhow!("wrong passphrase"))?;
    // Refused by *behaviour*, not string comparison: a duress secret that also opens the normal
    // slot would be unreachable, since the normal slot is checked first.
    if try_slot(&lock.normal, duress, m, t, p).is_some() {
        anyhow::bail!("the duress secret must be different from the normal one");
    }
    let mut filler: StoreKey = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut filler[..]);
    lock.duress = wrap_slot(&filler, duress)?;
    lock.armed = seal_armed(&key, true)?;
    lock.v = FORMAT_V; // a v1 file is upgraded in place the moment duress is armed
    let previous = std::fs::read(path(dir)).ok();
    write(dir, &lock)?;
    // Self-check, because this is the one secret that can never be rehearsed: confirm the file we
    // just wrote really reports duress for this secret. Not a user-facing test — it can't be re-run
    // later, so it can't be used by someone holding the phone to discover whether duress is armed.
    // On failure, put the previous lock back rather than leaving a lock nobody understands.
    if !matches!(unlock(dir, duress), Ok(Opened::Duress)) {
        if let Some(bytes) = previous {
            std::fs::write(path(dir), bytes)?;
        }
        anyhow::bail!("could not arm the duress secret");
    }
    Ok(())
}

/// Disarm duress by replacing the slot with a fresh decoy. Requires the normal secret. Disarming
/// when nothing was armed **succeeds and is indistinguishable** — the caller must not be able to
/// learn whether duress was set, and neither must anyone watching them.
pub fn clear_duress(dir: &str, passphrase: &str) -> Result<()> {
    let mut lock = read(dir)?;
    let (m, t, p) = (lock.m_cost_kib, lock.t_cost, lock.p_cost);
    let key = try_slot(&lock.normal, passphrase, m, t, p)
        .ok_or_else(|| anyhow::anyhow!("wrong passphrase"))?;
    lock.duress = decoy_slot()?;
    lock.armed = seal_armed(&key, false)?;
    lock.v = FORMAT_V;
    write(dir, &lock)
}

/// Whether `secret` is the **normal** secret for this lock — the check a settings screen needs
/// before it asks for anything else, so a wrong secret is refused immediately rather than after
/// the user has filled in a whole flow.
///
/// A duress secret returns `false` here, not a wipe: the wipe code's contract is the lock screen,
/// and firing it from inside settings on an already-unlocked app would be a surprise, not a
/// protection.
pub fn is_normal_secret(dir: &str, secret: &str) -> bool {
    unlock_normal(dir, secret).is_ok()
}

/// Try `secret` against the lock and report what it was.
///
/// **Both slots are always derived**, even once the normal one matches. Short-circuiting would make
/// a duress unlock take twice as long as a normal one — invisible to a human, trivial to measure,
/// and the observer this feature exists for is standing next to the user. See
/// `docs/design/duress-wipe.md` §4.
///
/// A wrong secret fails the AEAD tag on both slots; callers must not leak more than a generic
/// failure.
pub fn unlock(dir: &str, secret: &str) -> Result<Opened> {
    let lock = read(dir)?;
    let (m, t, p) = (lock.m_cost_kib, lock.t_cost, lock.p_cost);
    let normal = try_slot(&lock.normal, secret, m, t, p);
    let duress = try_slot(&lock.duress, secret, m, t, p);
    match (normal, duress) {
        (Some(key), _) => Ok(Opened::Normal(key)),
        (None, Some(_)) => Ok(Opened::Duress),
        (None, None) => anyhow::bail!("wrong passphrase"),
    }
}

/// Recover the store key, refusing a duress secret. For callers that only ever want the key and
/// would have no way to act on a wipe (backup import, tests).
pub fn unlock_normal(dir: &str, passphrase: &str) -> Result<StoreKey> {
    match unlock(dir, passphrase)? {
        Opened::Normal(key) => Ok(key),
        Opened::Duress => anyhow::bail!("wrong passphrase"),
    }
}

/// Remove the lock, proving knowledge of the passphrase first. Returns the store key so the caller
/// can put it back in the keystore — otherwise removing the lock would lose the store entirely.
/// A duress secret cannot remove the lock: it wipes instead, and that is the caller's job.
pub fn clear(dir: &str, passphrase: &str) -> Result<StoreKey> {
    let key = unlock_normal(dir, passphrase)?;
    std::fs::remove_file(path(dir))?;
    Ok(key)
}

/// Delete the lock file outright, without any secret. Only for the duress wipe (#3), where the
/// store it protects is being destroyed in the same breath — leaving the lock behind would show a
/// lock screen for a store that no longer exists.
pub fn destroy(dir: &str) -> Result<()> {
    match std::fs::remove_file(path(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> String {
        let d = std::env::temp_dir().join(format!("nd-lock-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&d).unwrap();
        d.to_string_lossy().into_owned()
    }

    #[test]
    fn wraps_and_recovers_the_exact_key() {
        let dir = tmpdir();
        let key: StoreKey = [7u8; 32];
        assert!(!is_locked(&dir));
        set_passphrase(&dir, &key, "correct horse battery staple").unwrap();
        assert!(is_locked(&dir));
        assert_eq!(
            unlock(&dir, "correct horse battery staple").unwrap(),
            Opened::Normal(key),
            "the unwrapped key must be byte-identical or the whole store is lost"
        );
    }

    #[test]
    fn the_duress_secret_reports_duress_and_never_yields_a_key() {
        let dir = tmpdir();
        let key: StoreKey = [7u8; 32];
        set_passphrase(&dir, &key, "the normal one is long").unwrap();
        set_duress(&dir, "the normal one is long", "the duress one").unwrap();

        // Normal still opens, unchanged by arming duress.
        assert_eq!(
            unlock(&dir, "the normal one is long").unwrap(),
            Opened::Normal(key)
        );
        // Duress reports itself and carries no key — there is no path from the duress secret to
        // readable data, which is the whole point of wrapping filler in that slot.
        assert_eq!(unlock(&dir, "the duress one").unwrap(), Opened::Duress);
        // And it cannot be used to take the lock off, which would preserve the store.
        assert!(clear(&dir, "the duress one").is_err());
        assert!(is_locked(&dir), "a failed clear must not remove the lock");
        // A third, unrelated secret is still just wrong.
        assert!(unlock(&dir, "neither of them").is_err());
    }

    #[test]
    fn arming_duress_needs_the_normal_secret_and_refuses_a_duplicate() {
        let dir = tmpdir();
        set_passphrase(&dir, &[1u8; 32], "normal secret here").unwrap();
        // Someone who coerced one unlock must not be able to disarm or re-arm duress with it.
        assert!(set_duress(&dir, "wrong secret here", "duress secret").is_err());
        assert!(clear_duress(&dir, "wrong secret here").is_err());
        // A duress secret equal to the normal one would be unreachable (normal matches first).
        assert!(set_duress(&dir, "normal secret here", "normal secret here").is_err());
        assert!(set_duress(&dir, "normal secret here", "").is_err());
    }

    #[test]
    fn disarming_duress_is_indistinguishable_from_never_arming_it() {
        let dir = tmpdir();
        let key: StoreKey = [5u8; 32];
        set_passphrase(&dir, &key, "normal secret here").unwrap();
        set_duress(&dir, "normal secret here", "duress secret").unwrap();
        clear_duress(&dir, "normal secret here").unwrap();

        // The armed secret is now merely wrong — the wipe signal is gone.
        assert!(unlock(&dir, "duress secret").is_err());
        assert_eq!(
            unlock(&dir, "normal secret here").unwrap(),
            Opened::Normal(key)
        );
        // Disarming when nothing is armed must also succeed, so the call itself never reveals
        // whether duress was set.
        clear_duress(&dir, "normal secret here").unwrap();
    }

    #[test]
    fn the_file_never_reveals_whether_duress_is_armed() {
        // The on-disk shape must be identical either way: an adversary imaging the device learning
        // that a duress secret exists is exactly what makes coercion effective.
        let unarmed = tmpdir();
        let armed = tmpdir();
        set_passphrase(&unarmed, &[2u8; 32], "normal secret here").unwrap();
        set_passphrase(&armed, &[2u8; 32], "normal secret here").unwrap();
        set_duress(&armed, "normal secret here", "duress secret").unwrap();

        let read_json = |d: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(path(d)).unwrap()).unwrap()
        };
        let (a, b) = (read_json(&unarmed), read_json(&armed));
        let keys = |v: &serde_json::Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&a), keys(&b), "same fields, armed or not");
        for slot in ["normal", "duress"] {
            assert_eq!(
                keys(&a[slot]),
                keys(&b[slot]),
                "{slot}: same slot fields, armed or not"
            );
            assert_eq!(
                a[slot]["wrapped"].as_str().unwrap().len(),
                b[slot]["wrapped"].as_str().unwrap().len(),
                "{slot}: a decoy must be the same size as a real slot"
            );
        }
    }

    #[test]
    fn a_v1_lock_still_opens_and_can_be_upgraded() {
        // Locks written before duress existed must never be stranded.
        let dir = tmpdir();
        let key: StoreKey = [8u8; 32];
        let wrapped = {
            use base64::Engine as _;
            let kek = derive("old passphrase", &[0u8; 16], M_COST_KIB, T_COST, P_COST).unwrap();
            base64::engine::general_purpose::STANDARD.encode(seal(&kek, &key).unwrap())
        };
        let v1 = serde_json::json!({
            "v": 1,
            "salt": "AAAAAAAAAAAAAAAAAAAAAA==",
            "m_cost_kib": M_COST_KIB,
            "t_cost": T_COST,
            "p_cost": P_COST,
            "wrapped": wrapped,
        });
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path(&dir), serde_json::to_vec(&v1).unwrap()).unwrap();

        assert_eq!(unlock(&dir, "old passphrase").unwrap(), Opened::Normal(key));
        // Arming duress upgrades it in place, and both secrets work afterwards.
        set_duress(&dir, "old passphrase", "the duress one").unwrap();
        assert_eq!(unlock(&dir, "old passphrase").unwrap(), Opened::Normal(key));
        assert_eq!(unlock(&dir, "the duress one").unwrap(), Opened::Duress);
    }

    #[test]
    fn the_armed_flag_tracks_state_and_needs_the_store_key() {
        let dir = tmpdir();
        let key: StoreKey = [11u8; 32];
        set_passphrase(&dir, &key, "normal secret here").unwrap();
        assert!(!duress_armed(&dir, &key), "a fresh lock has no wipe code");

        set_duress(&dir, "normal secret here", "the wipe code").unwrap();
        assert!(duress_armed(&dir, &key));
        // Someone holding only an image of the device can't read it: without the store key — which
        // is what the lock protects — the flag says nothing.
        assert!(!duress_armed(&dir, &[0u8; 32]));

        clear_duress(&dir, "normal secret here").unwrap();
        assert!(!duress_armed(&dir, &key));
    }

    #[test]
    fn a_v2_lock_written_before_the_armed_flag_still_opens() {
        // Regression (field report, 2026-08-01): `armed` was added to v2 as a required field, so
        // every lock file written by the previous build stopped deserializing — and the user was
        // told their correct PIN was wrong, with no way back in. Adding a field to a format version
        // that already shipped must never be able to do that.
        let dir = tmpdir();
        let key: StoreKey = [13u8; 32];
        set_passphrase(&dir, &key, "normal secret here").unwrap();

        // Strip the field, exactly as the older build would have written it.
        let p = path(&dir);
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        raw.as_object_mut().unwrap().remove("armed");
        assert!(raw.get("armed").is_none());
        std::fs::write(&p, serde_json::to_vec(&raw).unwrap()).unwrap();

        assert_eq!(
            unlock(&dir, "normal secret here").unwrap(),
            Opened::Normal(key),
            "an existing lock must keep opening across an app update"
        );
        assert!(!duress_armed(&dir, &key), "no flag reads as not armed");
        // And it can be re-armed, which writes the field back.
        set_duress(&dir, "normal secret here", "the wipe code").unwrap();
        assert!(duress_armed(&dir, &key));
    }

    #[test]
    fn the_normal_secret_can_be_checked_without_side_effects() {
        // Settings must be able to reject a wrong secret up front, instead of letting the user
        // fill in a whole flow and failing at the end.
        let dir = tmpdir();
        set_passphrase(&dir, &[12u8; 32], "normal secret here").unwrap();
        set_duress(&dir, "normal secret here", "the wipe code").unwrap();

        assert!(is_normal_secret(&dir, "normal secret here"));
        assert!(!is_normal_secret(&dir, "nope nope nope"));
        // The wipe code is not the normal secret, and checking it here must not wipe anything.
        assert!(!is_normal_secret(&dir, "the wipe code"));
        assert!(is_locked(&dir));
    }

    #[test]
    fn a_failed_arming_leaves_the_previous_lock_intact() {
        // The self-check in `set_duress` must not be able to leave a half-armed lock behind: the
        // normal secret has to keep working no matter what.
        let dir = tmpdir();
        let key: StoreKey = [6u8; 32];
        set_passphrase(&dir, &key, "normal secret here").unwrap();
        set_duress(&dir, "normal secret here", "first duress").unwrap();
        // A rejected re-arm (duplicate of the normal secret) changes nothing.
        assert!(set_duress(&dir, "normal secret here", "normal secret here").is_err());
        assert_eq!(
            unlock(&dir, "normal secret here").unwrap(),
            Opened::Normal(key)
        );
        assert_eq!(unlock(&dir, "first duress").unwrap(), Opened::Duress);
    }

    #[test]
    fn destroy_removes_the_lock_and_is_idempotent() {
        let dir = tmpdir();
        set_passphrase(&dir, &[4u8; 32], "normal secret here").unwrap();
        destroy(&dir).unwrap();
        assert!(!is_locked(&dir), "the wipe must take the lock file with it");
        destroy(&dir).unwrap(); // a re-run must not fail
    }

    #[test]
    fn a_wrong_passphrase_fails_rather_than_returning_a_wrong_key() {
        let dir = tmpdir();
        set_passphrase(&dir, &[9u8; 32], "the real one").unwrap();
        let e = unlock(&dir, "not the real one").unwrap_err().to_string();
        assert!(e.contains("wrong passphrase"), "got: {e}");
    }

    #[test]
    fn a_tampered_lock_file_is_rejected() {
        let dir = tmpdir();
        set_passphrase(&dir, &[3u8; 32], "hunter2 but longer").unwrap();
        // Flip a byte inside the wrapped key: the AEAD tag must catch it.
        let p = path(&dir);
        let mut lock: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        let w = lock["normal"]["wrapped"].as_str().unwrap().to_string();
        let mut bytes = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(&w)
                .unwrap()
        };
        bytes[0] ^= 0xff;
        lock["normal"]["wrapped"] = serde_json::Value::String({
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        });
        std::fs::write(&p, serde_json::to_vec(&lock).unwrap()).unwrap();
        assert!(unlock(&dir, "hunter2 but longer").is_err());
    }

    #[test]
    fn clearing_returns_the_key_so_the_store_is_not_orphaned() {
        let dir = tmpdir();
        let key: StoreKey = [42u8; 32];
        set_passphrase(&dir, &key, "a passphrase").unwrap();
        assert!(
            clear(&dir, "wrong").is_err(),
            "clear must prove knowledge first"
        );
        assert!(is_locked(&dir), "a failed clear must not remove the lock");
        assert_eq!(clear(&dir, "a passphrase").unwrap(), key);
        assert!(!is_locked(&dir));
    }

    #[test]
    fn an_empty_passphrase_is_refused() {
        let dir = tmpdir();
        assert!(set_passphrase(&dir, &[1u8; 32], "").is_err());
    }
}
