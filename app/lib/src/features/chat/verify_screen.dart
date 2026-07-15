import 'package:flutter/material.dart';
import 'package:qr_flutter/qr_flutter.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/nightdrop_core.dart';
import '../../core/models.dart';
import '../pairing/scan_screen.dart';

/// Safety-number verification screen (key-verification design). Shows the human-comparable
/// safety number (identical on both devices) and a QR of the raw fingerprint; the user confirms
/// no MITM sat on pairing by comparing the number out-of-band or scanning the other's QR.
class VerifyScreen extends StatefulWidget {
  const VerifyScreen({super.key, required this.contactId, required this.name});

  final String contactId;
  final String name;

  @override
  State<VerifyScreen> createState() => _VerifyScreenState();
}

class _VerifyScreenState extends State<VerifyScreen> {
  String? _number;
  String? _qr;
  bool _busy = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_number == null) _load();
  }

  Future<void> _load() async {
    final core = GhostScope.of(context);
    try {
      final number = await core.safetyNumber(widget.contactId);
      final qr = await core.safetyQr(widget.contactId);
      if (!mounted) return;
      setState(() {
        _number = number;
        _qr = qr;
      });
    } catch (_) {
      if (mounted) setState(() => _number = 'unavailable');
    }
  }

  Contact? _contact(NightdropCore core) {
    for (final c in core.contacts) {
      if (c.id == widget.contactId) return c;
    }
    return null;
  }

  Future<void> _scanToVerify() async {
    final core = GhostScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    final scanned = await Navigator.of(context).push<String>(
      MaterialPageRoute<String>(
        builder: (_) => ScanScreen(raw: true, title: l10n.scanTheirSafetyCode),
      ),
    );
    if (scanned == null || !mounted) return;
    setState(() => _busy = true);
    try {
      final ok = await core.verifySafetyQr(widget.contactId, scanned);
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(
        content: Text(ok
            ? l10n.verifiedMatch
            : l10n.verifiedNoMatch(widget.name)),
      ));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final core = GhostScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    return Scaffold(
      appBar: AppBar(title: Text(l10n.verifyTitle(widget.name))),
      body: ListenableBuilder(
        listenable: core,
        builder: (context, _) {
          final verified = _contact(core)?.verified ?? false;
          return ListView(
            padding: const EdgeInsets.all(20),
            children: [
              Text(
                l10n.verifyIntro(widget.name),
                style: Theme.of(context).textTheme.bodyMedium,
              ),
              const SizedBox(height: 20),
              _VerifiedChip(verified: verified),
              const SizedBox(height: 20),
              SelectableText(
                _number ?? '…',
                textAlign: TextAlign.center,
                style: const TextStyle(
                  fontFamily: 'monospace',
                  fontSize: 20,
                  letterSpacing: 1.5,
                  height: 1.6,
                ),
              ),
              const SizedBox(height: 24),
              if (_qr != null)
                Center(
                  child: Container(
                    padding: const EdgeInsets.all(12),
                    decoration: BoxDecoration(
                      color: Colors.white,
                      borderRadius: BorderRadius.circular(12),
                    ),
                    child: QrImageView(
                      data: _qr!,
                      version: QrVersions.auto,
                      size: 200,
                      semanticsLabel: l10n.safetyQrLabel,
                    ),
                  ),
                ),
              const SizedBox(height: 8),
              Center(
                child: Text(l10n.yourSafetyCodeHint,
                    style: const TextStyle(fontSize: 12)),
              ),
              const SizedBox(height: 24),
              FilledButton.icon(
                onPressed: _busy ? null : _scanToVerify,
                icon: const Icon(Icons.qr_code_scanner),
                label: Text(l10n.scanTheirCode),
              ),
              const SizedBox(height: 8),
              OutlinedButton.icon(
                onPressed: _busy
                    ? null
                    : () => core.setVerified(widget.contactId, !verified),
                icon: Icon(verified ? Icons.close : Icons.check),
                label: Text(verified ? l10n.markAsUnverified : l10n.markAsVerified),
              ),
            ],
          );
        },
      ),
    );
  }
}

class _VerifiedChip extends StatelessWidget {
  const _VerifiedChip({required this.verified});
  final bool verified;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Align(
      alignment: Alignment.center,
      child: Chip(
        avatar: Icon(
          verified ? Icons.verified_user : Icons.gpp_maybe,
          size: 18,
          color: verified ? scheme.onPrimaryContainer : scheme.onErrorContainer,
        ),
        label: Text(verified
            ? AppLocalizations.of(context)!.verified
            : AppLocalizations.of(context)!.notVerified),
        backgroundColor:
            verified ? scheme.primaryContainer : scheme.errorContainer,
      ),
    );
  }
}
