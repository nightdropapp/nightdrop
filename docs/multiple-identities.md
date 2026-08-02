# Running more than one identity

Night Drop is **one identity per install**, deliberately. If you want separate identities for
separate contexts — so that a contact in one cannot be linked to a contact in another — the
operating system already does this better than the app could, by running a second copy of Night
Drop in an isolated profile.

This was tested on a Galaxy S25 (Android 16) on 2026-08-01. See §4 for the one real limitation.

## 1. Why not build it into the app

Two reasons, both discovered by looking rather than assuming.

**The OS does it better.** Samsung's Secure Folder is a Knox-hardened Android work profile: separate
storage, separate app data, and keys tied to the device's secure element. An in-app profile switcher
would protect its data with our own app lock — Argon2 over a secret you chose — which, as
`docs/design/app-lock.md` §2 says plainly, cannot save a short PIN from an offline attack. Knox's
hardware-throttled unlock can.

**We could not have hidden it anyway.** arti writes the onion service secret key as a plaintext file
(`arti-state/keystore/hss/<svc>/ks_hs_id.ed25519_expanded_private`), outside anything we encrypt. A
second in-app identity would need a second onion, hence a second plaintext key, visible to anyone
who images the phone. So an in-app version could not have offered deniability against a cloned
device — the property that would have justified the complexity.

**Bonus, verified:** `adb` cannot reach the Secure Folder profile at all. `pm list packages
--user 150` and `pm install --user 150` both fail with `SecurityException: Shell does not have
permission to access user 150`. So an adversary with your unlocked phone and an authorised
debugging host still cannot enumerate what is inside it. That is strictly better than the normal
profile.

## 2. How

**Samsung (Secure Folder):** open Secure Folder → **+ Add apps** → pick Night Drop → **Add**. A
second copy appears inside, with its own storage. Open it there and it offers to create an identity,
because it has none.

**Other Android:** the same mechanism exists as a plain work profile — apps like Shelter or Island
create one. Android's multi-user feature also works, where the vendor enables it.

## 3. What was verified

* The Secure Folder copy asks to **create a new identity**; it does not see the outer one.
* The outer instance is **completely untouched** — same identity, same chats.
* **Sending and receiving both work** inside Secure Folder, so Tor bootstraps and the onion
  publishes normally in that profile.
* **Draining the relay mailbox works**, so offline mail arrives when you open it.

## 4. The limitation that matters: no background delivery

**Background delivery does not survive Secure Folder.** The profile suspends its apps when locked,
which stops the foreground service — so the inner identity receives nothing while Secure Folder is
closed.

That is not merely a delay. Mail waits on the relay for **24 hours** and is then reaped
(`RELAY_TTL`). Leave a Secure Folder identity closed for longer than that and messages sent to it
are **lost**; the sender sees "Not delivered (expired)".

So treat a Secure Folder identity as one you **open deliberately, at least daily** — a
compartment you visit, not an always-reachable address. Tell contacts who rely on it that you are
not continuously reachable there.

## 5. Other costs

* **Storage doubles.** Each copy keeps its own Tor directory cache — roughly 100 MB — plus its own
  message store.
* **Two onions from one device.** If both copies are open at once, both onion services publish from
  the same device on the same network. A global observer who can correlate that timing could link
  them. Opening them at different times avoids it.
* **Not tested:** whether notifications from the inner copy appear on the outer lock screen, and how
  the app-switcher preview behaves inside the profile.
