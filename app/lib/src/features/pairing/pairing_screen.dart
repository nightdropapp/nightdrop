import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:qr_flutter/qr_flutter.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/models.dart';
import '../chat/chat_screen.dart';
import 'scan_screen.dart';

/// Two ways to pair (ARCHITECTURE.md §5):
///   • Invite — show a QR (pre-authorized) and a short code (`slot-secret-words`).
///   • Join — enter a short code; the PAKE secret words authorize and block MITM.
class PairingScreen extends StatelessWidget {
  const PairingScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return DefaultTabController(
      length: 2,
      child: Scaffold(
        appBar: AppBar(
          title: Text(l10n.newChat),
          bottom: TabBar(
            tabs: [Tab(text: l10n.pairInvite), Tab(text: l10n.pairJoin)],
          ),
        ),
        body: const TabBarView(
          children: [_InviteTab(), _JoinTab()],
        ),
      ),
    );
  }
}

class _InviteTab extends StatefulWidget {
  const _InviteTab();

  @override
  State<_InviteTab> createState() => _InviteTabState();
}

class _InviteTabState extends State<_InviteTab> {
  PairingInvite? _invite;
  bool _requested = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_requested) return;
    _requested = true;
    GhostScope.of(context).createInvite().then((inv) {
      if (mounted) setState(() => _invite = inv);
    });
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    final invite = _invite;
    if (invite == null) {
      return const Center(child: CircularProgressIndicator());
    }
    // A pre-authorized QR carries the full pre-key bundle payload; otherwise the QR encodes
    // the short code itself, so the other device can scan instead of typing it. The secret
    // words travel only in this device-to-device visual channel, never via a server.
    final preAuth = invite.qrPayload.isNotEmpty;
    final qrData = preAuth ? invite.qrPayload : invite.shortCode;
    return SingleChildScrollView(
      padding: const EdgeInsets.all(24),
      child: Column(
        children: [
          Text(
            preAuth ? l10n.inviteScanPreAuth : l10n.inviteScanOrCode,
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 16),
          Container(
            padding: const EdgeInsets.all(12),
            color: Colors.white,
            // Sized generously: the pre-authorized payload is dense (full .onion + keys), so a
            // larger render gives the scanning phone more pixels-per-module to lock onto.
            child: QrImageView(
              data: qrData,
              size: 280,
              semanticsLabel: 'Night Drop pairing QR code',
            ),
          ),
          // Can't scan (e.g. the other device is a desktop with no camera)? The same payload can be
          // sent as a link and pasted under Join. Same trust model as the QR — a pre-authorized
          // bundle — so share it only over a channel you trust.
          if (preAuth) ...[
            const SizedBox(height: 12),
            OutlinedButton.icon(
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: qrData));
                if (context.mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(
                    SnackBar(content: Text(l10n.inviteLinkCopied)),
                  );
                }
              },
              icon: const Icon(Icons.link),
              label: Text(l10n.copyInviteLink),
            ),
            Text(
              l10n.cantScanHint,
              textAlign: TextAlign.center,
              style: const TextStyle(fontSize: 12),
            ),
          ],
          // The short code is present when a rendezvous invite was staged (needs a relay). A
          // pre-authorized QR with no staged code (Tor without a relay configured) simply omits
          // this section — the QR alone still pairs.
          if (invite.shortCode.isNotEmpty) ...[
            const SizedBox(height: 32),
            Text(l10n.orReadOutCode),
            const SizedBox(height: 12),
            SelectableText(
              invite.shortCode,
              style: Theme.of(context).textTheme.headlineSmall,
            ),
            const SizedBox(height: 12),
            Text(
              l10n.secretWordsExplanation,
              textAlign: TextAlign.center,
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ],
        ],
      ),
    );
  }
}

class _JoinTab extends StatefulWidget {
  const _JoinTab();

  @override
  State<_JoinTab> createState() => _JoinTabState();
}

class _JoinTabState extends State<_JoinTab> {
  final _controller = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _join([String? code]) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final contact = await GhostScope.of(context)
          .joinWithShortCode(code ?? _controller.text);
      if (!mounted) return;
      Navigator.of(context).pushReplacement(
        MaterialPageRoute<void>(
            builder: (_) => ChatScreen(contactId: contact.id)),
      );
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _scan() async {
    final payload = await Navigator.of(context).push<String>(
      MaterialPageRoute<String>(builder: (_) => const ScanScreen()),
    );
    // The scanned payload is either a pre-authorized bundle (§5a) or a short code; both are
    // handled by joinWithShortCode, so connect straight away.
    if (payload != null && mounted) await _join(payload);
  }

  /// Desktop pairing without a camera: pull an invite link (or short code) from the clipboard and
  /// connect. Mirrors what a scan would hand us — both go through joinWithShortCode.
  Future<void> _pasteLink() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text?.trim() ?? '';
    if (text.isEmpty) {
      if (mounted) {
        setState(() =>
            _error = AppLocalizations.of(context)!.clipboardEmpty);
      }
      return;
    }
    _controller.text = text;
    if (mounted) await _join(text);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return Stack(
      children: [
        Padding(
          padding: const EdgeInsets.all(24),
          // Scrollable so the field + buttons never overflow when the on-screen keyboard opens on
          // a short screen.
          child: SingleChildScrollView(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                TextField(
                  controller: _controller,
                  autofocus: true,
                  decoration: InputDecoration(
                    labelText: l10n.shortCodeOrInviteLink,
                    hintText: '4-ghost-lantern-river  or  nightdrop://pair?…',
                    border: const OutlineInputBorder(),
                    errorText: _error,
                  ),
                ),
                const SizedBox(height: 16),
                FilledButton(
                  onPressed: _busy ? null : _join,
                  child: Text(l10n.connect),
                ),
                const SizedBox(height: 16),
                // Three equal ways in, not a camera-first flow with fallbacks: type/paste the code
                // in the field above, scan a QR (mobile), or paste an invite link from the clipboard.
                // Scanning is offered only where there's a camera backend; pasting works everywhere.
                if (canScanQr)
                  OutlinedButton.icon(
                    onPressed: _busy ? null : _scan,
                    icon: const Icon(Icons.qr_code_scanner),
                    label: Text(l10n.scanInviteQr),
                  ),
                if (canScanQr) const SizedBox(height: 8),
                OutlinedButton.icon(
                  onPressed: _busy ? null : _pasteLink,
                  icon: const Icon(Icons.content_paste),
                  label: Text(l10n.pasteInviteLink),
                ),
              ],
            ),
          ),
        ),
        // While a scan/paste is being joined, block the tab with a clear "connecting" overlay so the
        // work is visible (the pairing handshake can take a moment over Tor/relay).
        if (_busy)
          Positioned.fill(
            child: ColoredBox(
              color: Colors.black54,
              child: Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const CircularProgressIndicator(color: Colors.white),
                    const SizedBox(height: 20),
                    Text(
                      AppLocalizations.of(context)!.connecting,
                      style: const TextStyle(color: Colors.white, fontSize: 16),
                    ),
                  ],
                ),
              ),
            ),
          ),
      ],
    );
  }
}
