import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:qr_flutter/qr_flutter.dart';

import '../../../l10n/app_localizations.dart';
import '../../core/app_config.dart';

export '../../core/app_config.dart' show DonationCoin;

/// Donation addresses, from the shared config (`config/app_config.json` → `make config`).
/// Edit addresses there, not here. Falls back to compiled-in defaults until the asset loads.
List<DonationCoin> get kDonationCoins => AppConfig.current.donations;

/// Lists privacy-coin donation addresses with copy + QR. No custody, no tracking.
class DonationsScreen extends StatelessWidget {
  const DonationsScreen({super.key});

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(AppLocalizations.of(context)!.supportNightDrop)),
      body: ListView(
        // Clear the system navigation bar: edge-to-edge is enforced from targetSdk 35, so a
        // flat inset leaves the last row of content unreadable underneath it.
        padding: EdgeInsets.fromLTRB(16, 16, 16, 16 + MediaQuery.viewPaddingOf(context).bottom),
        children: [
          Text(
            AppConfig.current.blurb,
            style: Theme.of(context).textTheme.bodyMedium,
          ),
          const SizedBox(height: 16),
          for (final coin in kDonationCoins) _CoinCard(coin: coin),
        ],
      ),
    );
  }
}

class _CoinCard extends StatelessWidget {
  const _CoinCard({required this.coin});

  final DonationCoin coin;

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return Card(
      margin: const EdgeInsets.only(bottom: 16),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('${coin.name} (${coin.ticker})',
                style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 4),
            Text(coin.note, style: Theme.of(context).textTheme.bodySmall),
            const SizedBox(height: 12),
            Center(
              child: Container(
                padding: const EdgeInsets.all(8),
                color: Colors.white,
                child: QrImageView(
                  data: coin.address,
                  size: 160,
                  semanticsLabel: l10n.donationQrLabel(coin.name),
                ),
              ),
            ),
            const SizedBox(height: 12),
            SelectableText(coin.address, style: const TextStyle(fontFamily: 'monospace')),
            const SizedBox(height: 8),
            Align(
              alignment: Alignment.centerRight,
              child: TextButton.icon(
                icon: const Icon(Icons.copy, size: 18),
                label: Text(l10n.copyAddress),
                onPressed: () async {
                  await Clipboard.setData(ClipboardData(text: coin.address));
                  if (context.mounted) {
                    ScaffoldMessenger.of(context).showSnackBar(
                      SnackBar(content: Text(l10n.addressCopied(coin.ticker))),
                    );
                  }
                },
              ),
            ),
          ],
        ),
      ),
    );
  }
}
