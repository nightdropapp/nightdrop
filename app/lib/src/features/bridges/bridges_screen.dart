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
    if (result.rejected.isEmpty) {
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
