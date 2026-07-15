import 'package:flutter/material.dart';

import 'src/app.dart';
import 'src/core/app_config.dart';
import 'src/core/background_delivery.dart';
import 'src/core/rust_nightdrop_core.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // Configure the opt-in Android background-delivery foreground service (§11.8, #13). No-op off
  // Android; harmless if the user never enables it.
  BackgroundDelivery.init();
  // Shared config (donation addresses, copy) from assets/app_config.json — the single source
  // of truth synced from config/app_config.json by `make config`.
  await AppConfig.load();
  // Load the Rust security core (flutter_rust_bridge). The native lib is built and
  // bundled by the platform build (see README "Wiring the Rust core").
  await RustLib.init();
  runApp(GhostApp(core: RustNightdropCore()));
}
