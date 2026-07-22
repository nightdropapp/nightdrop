# Store images (F-Droid / fastlane layout)

F-Droid pulls the listing graphics from this folder. Add before submission:

- `icon.png` — 512×512 app icon (optional; F-Droid also extracts it from the APK).
  The app icon source lives at `app/assets/icons/icon-512.png`.
- `phoneScreenshots/` — one or more PNG/JPG screenshots (e.g. `01.png`, `02.png`).
  Recommended shots: the chat list, a 1:1 conversation, the pairing (Invite) screen,
  and the safety-number verification screen. Capture on a device/emulator so no real
  identities or messages are shown — use throwaway test identities.

These are the only pieces of the F-Droid listing that can't be generated from source;
everything else (title, descriptions, changelog) is already in this directory.
