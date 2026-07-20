# Night Drop — common tasks. Requires the Flutter SDK and the Rust toolchain.
.PHONY: help app-get app-run app-test app-analyze core-build core-test relay-run fmt clippy bootstrap gen-bridge config

help:
	@echo "App (Flutter, in app/):"
	@echo "  make app-get      flutter pub get"
	@echo "  make app-run      run the app (against the mock core)"
	@echo "  make app-test     flutter test"
	@echo "  make app-analyze  flutter analyze"
	@echo "Core + relay (Rust):"
	@echo "  make core-build   cargo build -p nightdrop (produces libnightdrop.so)"
	@echo "  make core-test    cargo test  -p nightdrop"
	@echo "  make relay-run    cargo run   -p nightdrop_relay"
	@echo "  make gen-bridge   regenerate flutter_rust_bridge bindings"
	@echo "  make fmt          cargo fmt"
	@echo "  make clippy       cargo clippy --all-targets"
	@echo "  make bootstrap    one-time: populate Flutter platform folders + pub get"

app-get:      ; cd app && flutter pub get
app-run:      config ; cd app && flutter run
# Build the core first so the bridge test can load libnightdrop.so.
app-test:     config core-build ; cd app && flutter test

# Single source of truth: edit config/app_config.json (donation addresses, download links,
# copy), then this propagates it to the app (asset) and website (config.js). Wired into the
# app build targets so they stay in sync automatically.
config:
	@mkdir -p app/assets website
	@cp config/app_config.json app/assets/app_config.json
	@printf '// GENERATED from config/app_config.json by `make config` — do not edit by hand.\nwindow.NIGHTDROP_CONFIG = %s;\n' "$$(cat config/app_config.json)" > website/config.js
	@echo "config: synced -> app/assets/app_config.json, website/config.js"
app-analyze:  ; cd app && flutter analyze
gen-bridge:   ; flutter_rust_bridge_codegen generate

core-build:   ; cargo build -p nightdrop
core-test:    ; cargo test  -p nightdrop
relay-run:    ; cargo run   -p nightdrop_relay
fmt:          ; cargo fmt
clippy:       ; cargo clippy --all-targets

# Generate the native platform shells (android/ios/windows/linux/macos) into app/
# without clobbering the provided lib/ and pubspec.yaml, then fetch packages.
bootstrap:
	cd app && flutter create --project-name night_drop \
		--platforms=android,ios,windows,linux,macos . && flutter pub get
