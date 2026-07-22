import 'dart:convert';

import 'package:flutter/services.dart' show rootBundle;

/// A privacy-coin donation address. Addresses are non-custodial — published only.
class DonationCoin {
  const DonationCoin({
    required this.name,
    required this.ticker,
    required this.address,
    required this.note,
  });

  final String name;
  final String ticker;
  final String address;
  final String note;

  factory DonationCoin.fromJson(Map<String, dynamic> j) => DonationCoin(
        name: j['name'] as String? ?? '',
        ticker: j['ticker'] as String? ?? '',
        address: j['address'] as String? ?? '',
        note: j['note'] as String? ?? '',
      );
}

/// Built-in fallback used when the bundled config can't be read (e.g. widget tests). The
/// canonical values live in `config/app_config.json` and are synced here by `make config`.
const List<DonationCoin> kDefaultDonations = [
  DonationCoin(
    name: 'Monero',
    ticker: 'XMR',
    address:
        '49yRv29r6yHYBGZH4z1uGTXg68VFYX4Zf1cWopevd32YLUwj86mXddNe8bCTaZKcRQYDRdHJrcL6uAiCRKH1AMrDTQNNZZm',
    note: 'Default. Strong sender/receiver/amount privacy.',
  ),
];

/// App-wide configuration loaded from the shared `assets/app_config.json` (generated from
/// `config/app_config.json` — the single place to edit donation addresses, copy, and links
/// for BOTH the app and the website). Falls back to compiled-in defaults if the asset is
/// missing so the app (and tests) always work.
class AppConfig {
  const AppConfig({
    required this.appName,
    required this.headline,
    required this.tagline,
    required this.blurb,
    required this.donations,
  });

  final String appName;
  final String headline;
  final String tagline;
  final String blurb;
  final List<DonationCoin> donations;

  static const AppConfig _fallback = AppConfig(
    appName: 'Night Drop',
    headline: 'Sealed. Dropped. Delivered.',
    tagline:
        'Like a night deposit box: every message is a sealed envelope, dropped for one '
        'person only, opened by no one in between. Anonymous, end-to-end encrypted 1:1 '
        'chat over Tor. No accounts. No phone number. No server-side keys. No logs.',
    blurb:
        "Night Drop is free. If it's useful to you, a donation keeps it alive. We accept "
        'privacy coins so giving stays anonymous — no accounts, no tracking, no trail.',
    donations: kDefaultDonations,
  );

  static AppConfig _current = _fallback;

  /// The active configuration (defaults until [load] succeeds).
  static AppConfig get current => _current;

  /// Load the bundled config asset. Safe to call once at startup; keeps defaults on any error.
  static Future<void> load() async {
    try {
      final raw = await rootBundle.loadString('assets/app_config.json');
      final json = jsonDecode(raw) as Map<String, dynamic>;
      final app = (json['app'] as Map<String, dynamic>?) ?? const {};
      final coins = (json['donations'] as List<dynamic>? ?? const [])
          .map((e) => DonationCoin.fromJson(e as Map<String, dynamic>))
          .toList();
      _current = AppConfig(
        appName: app['name'] as String? ?? _fallback.appName,
        headline: app['headline'] as String? ?? _fallback.headline,
        tagline: app['tagline'] as String? ?? _fallback.tagline,
        blurb: app['blurb'] as String? ?? _fallback.blurb,
        donations: coins.isEmpty ? kDefaultDonations : coins,
      );
    } catch (_) {
      _current = _fallback; // keep compiled-in defaults
    }
  }
}
