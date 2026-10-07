# Night Drop — Flutter app

The UI shell over the Rust security core (`../core`). It talks only to the abstract
`NightdropCore` seam (`lib/src/core/nightdrop_core.dart`); no keys, crypto or transport live here.

- Project overview: [`../README.md`](../README.md)
- Design and invariants: [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
- Dev builds: [`../BUILD_AND_DEPLOY.md`](../BUILD_AND_DEPLOY.md); toolchain, tests, releases:
  [`../MAINTENANCE.md`](../MAINTENANCE.md)
- Translations: drop `lib/l10n/app_<locale>.arb` and run `flutter gen-l10n` — no code changes
  (English is the only locale so far). Time formatting is not localized: chat times use a fixed
  12-hour clock (`lib/src/features/chat/chat_screen.dart`).
