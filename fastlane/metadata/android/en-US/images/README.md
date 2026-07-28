# Store images (F-Droid / fastlane layout)

F-Droid pulls the listing graphics from this folder.

- `icon.png` — 512×512 app icon (optional; F-Droid also extracts it from the APK).
  The app icon source lives at `app/assets/icons/icon-512.png`.
- `phoneScreenshots/` — in place: `01.png` pairing (Invite), `02.png` chat list,
  `03.png` background delivery, `04.png` relays. Captured on a physical device
  running 0.1.8 at 1080×2340.

Still worth adding when two paired devices are available: a **1:1 conversation** and the
**safety-number verification** screen. Neither can be captured from a single device, since both
require a paired contact.

## Before adding a screenshot — redact secrets

This listing is public, and several screens show live key material. **The pairing screen is the
dangerous one:**

- The **invite QR** encodes the device's onion address *and a pre-authorized pre-key bundle* —
  anyone who scans it can message that identity. It is not merely an address.
- The **short code's secret words** are the SPAKE2 secret; they carry the whole security of
  short-code pairing.
- **My identity** shows the onion address and safety number.

`01.png` has the QR and the secret words pixelated-then-blurred, which is irreversible. Prefer a
visible redaction over substituting plausible-looking fake data, so nobody mistakes a screenshot
for a working invite. Re-check any new screenshot for: onion addresses, invite QRs, short codes,
safety numbers, contact names, message text, and personal notification icons in the status bar.

Capture with throwaway identities where possible:

```sh
adb -s <ip:port> shell settings put system accelerometer_rotation 0
adb -s <ip:port> shell settings put system user_rotation 0   # portrait
adb -s <ip:port> exec-out screencap -p > shot.png
```
