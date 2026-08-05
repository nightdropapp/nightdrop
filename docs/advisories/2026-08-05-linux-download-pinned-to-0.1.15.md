# Linux downloads served 0.1.15 for three days after 0.1.16 shipped

**Date:** 2026-08-05
**Severity:** Low direct risk, but it left users on a build with three known fixes missing.
**Affected:** People who downloaded the **Linux AppImage** from the clearnet website between
**2026-08-02** (when 0.1.16 was released) and **2026-08-05**.
**Not affected:** Android, and the onion site.

## What happened

The Linux download link on the website hardcoded a release tag:

```
https://github.com/nightdropapp/nightdrop/releases/download/v0.1.15/Night_Drop-x86_64.AppImage
```

Because the tag was pinned, the link kept serving 0.1.15 after 0.1.16 and 0.1.17 were released.
The Android link one line above it used `/releases/latest` and so tracked releases correctly,
which is why the problem went unnoticed: the page looked current and one platform genuinely was.

The onion site was never affected — it serves the AppImage from its own directory, which was
up to date.

## Why it matters

The AppImage has no auto-update, so anyone who downloaded in that window is still on 0.1.15
until they fetch it again by hand. 0.1.15 is missing three fixes that shipped in 0.1.16/0.1.17:

- **A new identity could come up on the previous identity's `.onion`.** After creating a fresh
  identity, arti could still launch it on the old on-disk onion key, so the "new" identity was
  reachable at the address the user had walked away from, and linkable to the identity they
  believed they had left behind. Silent and permanent once it happened.
- **A closed transport could fall back off Tor onto system DNS.** A shut-down transport reported
  "I cannot dial relays", which the node read as "use a direct TCP client" — and for `.onion`
  relay addresses that client handed the hostname to the system resolver. The resolver, and
  anyone watching it, would learn which hidden service the device wanted.
- **"Delivered" could be shown for a message the peer never received.** The relay drain recorded
  an acknowledgement before the frame was actually processed, so a dropped message still promoted
  its sibling messages to "Delivered".

Nothing about this exposed user data *to us*. Night Drop has no accounts, no server-side keys and
no logs; we cannot tell who downloaded what, or who is running which version. The risk is entirely
that affected users are running known-fixed defects.

## What to do

Download Night Drop again from the website and replace the AppImage. Identity, chats and contacts
live in local storage and are untouched by replacing the binary.

To check what you are running, the version is shown in the app under Settings.

## Fix

- The download link now points at `/releases/latest/download/…`, which resolves to the current
  release automatically. It was deliberately *not* bumped to `v0.1.17`: pinning a new tag would
  re-arm the identical trap at the next release.
- Fixed in `config/app_config.json`, the source of truth, rather than in the generated
  `website/config.js` — a `make config` run would otherwise have reverted it.
- A notice on the download section of the site tells affected users to update.
- No other pinned release tags remain under `website/`.

## Lesson

A download link that names a version is a link that will be wrong. The only reason this was
caught at all is that someone read the page and noticed the version looked old; nothing in the
release process would have flagged it, because the release itself was fine. Links that must track
releases should resolve to `latest` and never be edited per release.
