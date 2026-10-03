import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/models.dart';
import '../../core/nightdrop_core.dart';

/// Configure Tor **bridges** from inside the app (`docs/design/android-bridges.md`).
///
/// This screen exists for Android specifically. The core has always read `bridges.txt` from the Tor
/// state directory, but on Android that directory is app-private — a user behind a national
/// firewall had no way to put a file there, which is the platform that needs it most.
///
/// Two things it must be honest about, both stated in the UI rather than only here:
///
///  * Bridges apply when Tor is next started, not immediately.
///  * Vanilla bridges get past a **block of the public relay list**. They do *not* get past deep
///    packet inspection — where a censor fingerprints the Tor protocol itself, obfs4/Snowflake is
///    needed, and that needs a transport binary this build does not ship (§3 of the design note).
///    Telling someone in a heavily censored country that "3 bridges saved" means they are safe
///    would be the worst kind of wrong.
class BridgesScreen extends StatefulWidget {
  const BridgesScreen({super.key});

  @override
  State<BridgesScreen> createState() => _BridgesScreenState();
}

class _BridgesScreenState extends State<BridgesScreen> {
  final _controller = TextEditingController();
  List<RejectedBridge> _rejected = const [];
  bool _loading = true;
  bool _saving = false;
  bool _fetching = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _load());
  }

  Future<void> _load() async {
    final text = await NightdropScope.of(context).readBridges();
    if (!mounted) return;
    setState(() {
      _controller.text = text;
      _loading = false;
    });
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _save(NightdropCore core) async {
    final l10n = AppLocalizations.of(context)!;
    setState(() => _saving = true);
    final result = await core.writeBridges(_controller.text);
    if (!mounted) return;
    setState(() {
      _rejected = result.rejected;
      _saving = false;
    });
    final messenger = ScaffoldMessenger.of(context);
    messenger.showSnackBar(SnackBar(content: Text(l10n.bridgesSaved(result.accepted))));
    // Bridges are read when the Tor client is built, so nothing changes until it is rebuilt. Offer
    // that plainly instead of leaving the user to guess whether it took effect.
    //
    // Reached from onboarding there is no connection to reconnect yet, and the bridges will be
    // picked up by the core that identity creation builds a moment later — so offering it there
    // would only invite a pointless two-minute wait.
    if (result.rejected.isEmpty && core.identity != null) {
      final restart = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(l10n.bridgesRestartTitle),
          content: Text(l10n.bridgesRestartBody),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(context).pop(false),
              child: Text(l10n.later),
            ),
            FilledButton(
              onPressed: () => Navigator.of(context).pop(true),
              child: Text(l10n.bridgesRestartNow),
            ),
          ],
        ),
      );
      if (restart == true && mounted) {
        await core.retryStart();
        if (mounted) Navigator.of(context).pop();
      }
    }
  }

  /// "Get bridges": ask first, because this one request does NOT go through Tor (it exists for when
  /// Tor is blocked), then add what the Tor Project sends to the editor - unsaved, for the user to
  /// look at and save like any other lines (docs/design/android-bridges.md §7a.1).
  Future<void> _fetch(NightdropCore core) async {
    final l10n = AppLocalizations.of(context)!;
    final country = await showDialog<String>(
      context: context,
      builder: (context) => const _FetchConsentDialog(),
    );
    if (country == null || !mounted) return; // cancelled
    setState(() => _fetching = true);
    try {
      final f = await core.fetchBridges(country: country.isEmpty ? null : country);
      if (!mounted) return;
      final existing = _controller.text.split('\n').map((l) => l.trim()).toSet();
      final fresh = [for (final l in f.lines) if (!existing.contains(l)) l];
      final today = DateTime.now().toIso8601String().substring(0, 10);
      final where = f.country == null ? '' : ', ${f.country}';
      final block = [
        '# WebTunnel bridges from the Tor Project, $today$where${f.fromDefaults ? ' (defaults)' : ''}',
        ...fresh,
      ].join('\n');
      final current = _controller.text.trimRight();
      _controller.text = current.isEmpty ? block : '$current\n$block';
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text(l10n.bridgesFetchAdded(fresh.length))));
    } catch (e) {
      if (!mounted) return;
      // Stop the spinner before explaining the failure, not after the explanation is dismissed.
      setState(() => _fetching = false);
      final reason = e is StateError ? e.message : '$e';
      await showDialog<void>(
        context: context,
        builder: (context) => AlertDialog(
          content: Text(l10n.bridgesFetchFailed(reason)),
          actions: [
            TextButton(onPressed: () => Navigator.of(context).pop(), child: Text(l10n.close)),
          ],
        ),
      );
    } finally {
      if (mounted) setState(() => _fetching = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final core = NightdropScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(title: Text(l10n.bridgesTitle)),
      body: _loading
          ? const Center(child: CircularProgressIndicator())
          : ListView(
              padding: const EdgeInsets.all(20),
              children: [
                Text(l10n.bridgesBody, style: theme.textTheme.bodyMedium),
                const SizedBox(height: 16),
                // The limit sits above the input, not buried under it: it has to be read before
                // someone concludes this is enough for where they are.
                Container(
                  padding: const EdgeInsets.all(12),
                  decoration: BoxDecoration(
                    color: theme.colorScheme.surfaceContainerHighest,
                    borderRadius: BorderRadius.circular(10),
                  ),
                  child: Text(
                    l10n.bridgesLimit,
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
                  ),
                ),
                const SizedBox(height: 16),
                OutlinedButton.icon(
                  onPressed: _fetching || _saving ? null : () => _fetch(core),
                  icon: _fetching
                      ? const SizedBox(
                          height: 16,
                          width: 16,
                          child: CircularProgressIndicator(strokeWidth: 2))
                      : const Icon(Icons.download_outlined),
                  label: Text(_fetching ? l10n.bridgesFetching : l10n.bridgesFetchButton),
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: _controller,
                  maxLines: 8,
                  minLines: 4,
                  autocorrect: false,
                  enableSuggestions: false,
                  style: const TextStyle(fontFamily: 'monospace', fontSize: 12.5),
                  decoration: InputDecoration(
                    border: const OutlineInputBorder(),
                    hintText: l10n.bridgesHint,
                    helperText: l10n.bridgesWhereToGet,
                    helperMaxLines: 3,
                  ),
                ),
                if (_rejected.isNotEmpty) ...[
                  const SizedBox(height: 16),
                  Text(l10n.bridgesRejected(_rejected.length),
                      style: theme.textTheme.titleSmall
                          ?.copyWith(color: theme.colorScheme.error)),
                  const SizedBox(height: 8),
                  for (final r in _rejected)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 8),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(r.line,
                              style: const TextStyle(
                                  fontFamily: 'monospace', fontSize: 12)),
                          Text(r.reason,
                              style: theme.textTheme.bodySmall?.copyWith(
                                  color: theme.colorScheme.onSurfaceVariant)),
                        ],
                      ),
                    ),
                ],
                const SizedBox(height: 20),
                FilledButton(
                  onPressed: _saving ? null : () => _save(core),
                  child: _saving
                      ? const SizedBox(
                          height: 18,
                          width: 18,
                          child: CircularProgressIndicator(strokeWidth: 2))
                      : Text(l10n.save),
                ),
              ],
            ),
    );
  }
}

/// The consent step before fetching bridges. Says plainly that this request does not go through
/// Tor, who sees the IP address, and that nothing goes to Night Drop. Pops the (possibly empty)
/// country code on "Get bridges", or null on cancel.
class _FetchConsentDialog extends StatefulWidget {
  const _FetchConsentDialog();

  @override
  State<_FetchConsentDialog> createState() => _FetchConsentDialogState();
}

class _FetchConsentDialogState extends State<_FetchConsentDialog> {
  final _country = TextEditingController();

  @override
  void dispose() {
    _country.dispose();
    super.dispose();
  }

  bool get _countryOk {
    final c = _country.text.trim();
    return c.isEmpty || RegExp(r'^[a-zA-Z]{2}$').hasMatch(c);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return AlertDialog(
      title: Text(l10n.bridgesFetchTitle),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(l10n.bridgesFetchBody),
            const SizedBox(height: 16),
            TextField(
              controller: _country,
              maxLength: 2,
              autocorrect: false,
              enableSuggestions: false,
              onChanged: (_) => setState(() {}),
              decoration: InputDecoration(
                labelText: l10n.bridgesFetchCountryLabel,
                helperText: l10n.bridgesFetchCountryHint,
                helperMaxLines: 2,
                border: const OutlineInputBorder(),
              ),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
        FilledButton(
          onPressed: _countryOk
              ? () => Navigator.of(context).pop(_country.text.trim().toLowerCase())
              : null,
          child: Text(l10n.bridgesFetchGo),
        ),
      ],
    );
  }
}
