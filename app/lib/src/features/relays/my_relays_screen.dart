import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/models.dart';

/// A v3 onion address: 56 base32 characters, `.onion`, an optional port. Only onions: a relay
/// reached any other way would bypass Tor (ARCHITECTURE.md §1a, "Tor by default").
final _onionPattern = RegExp(r'^[a-z2-7]{56}\.onion(:\d{1,5})?$');

/// Whether [address] is something the relay list accepts.
bool isRelayAddress(String address) =>
    _onionPattern.hasMatch(address.trim().toLowerCase());

/// "My relays" (`docs/design/optional-relay.md` §2): where offline mail waits for us. The Night
/// Drop relay is an ordinary entry that can be removed and added back; an empty list is direct
/// only, and removing the last relay first says what that turns off (§6). Each change is saved at
/// once and announced to contacts by the core.
class MyRelaysScreen extends StatefulWidget {
  const MyRelaysScreen({super.key});

  @override
  State<MyRelaysScreen> createState() => _MyRelaysScreenState();
}

class _MyRelaysScreenState extends State<MyRelaysScreen> {
  List<RelayEntry>? _relays;
  RelayFeatures _features = const RelayFeatures();
  Map<String, bool> _reachable = const {};
  final _field = TextEditingController();
  String? _fieldError;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _load());
  }

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final core = NightdropScope.of(context);
    final relays = await core.relayList();
    final features = await core.relayFeatures();
    final health = await core.relayHealth();
    if (!mounted) return;
    setState(() {
      _relays = relays;
      _features = features;
      _reachable = {for (final h in health) h.address: h.reachable};
    });
  }

  Future<void> _save(List<RelayEntry> next) async {
    final core = NightdropScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    final messenger = ScaffoldMessenger.of(context);
    if (next.isEmpty && (_relays?.isNotEmpty ?? false)) {
      if (!await _confirmDirectOnly()) return;
    }
    try {
      await core.setRelayList(next);
    } catch (e) {
      messenger.showSnackBar(
          SnackBar(content: Text(l10n.couldNotSaveRelays(e.toString()))));
      return;
    }
    messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(l10n.relaysSaved)));
    await _load();
  }

  Future<bool> _confirmDirectOnly() async {
    final l10n = AppLocalizations.of(context)!;
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: Text(l10n.directOnlyConfirmTitle),
        content: Text(l10n.directOnlyConfirmBody),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.cancel),
          ),
          FilledButton(
            key: const Key('relays-confirm-direct'),
            onPressed: () => Navigator.pop(context, true),
            child: Text(l10n.directOnlyConfirm),
          ),
        ],
      ),
    );
    return ok == true;
  }

  void _add() {
    final l10n = AppLocalizations.of(context)!;
    final address = _field.text.trim().toLowerCase();
    if (!isRelayAddress(address)) {
      setState(() => _fieldError = l10n.relayInvalidOnion);
      return;
    }
    setState(() => _fieldError = null);
    _field.clear();
    _save([...?_relays, RelayEntry.onion(address)]);
  }

  Future<void> _showAccessKey(String onion) async {
    final core = NightdropScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    final messenger = ScaffoldMessenger.of(context);
    String key;
    try {
      key = await core.createRelayAccessKey(onion);
    } catch (e) {
      messenger.showSnackBar(SnackBar(content: Text(e.toString())));
      return;
    }
    if (!mounted) return;
    await showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: Text(l10n.relayAccessKey),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(l10n.relayAccessKeyBody),
            const SizedBox(height: 12),
            SelectableText(key,
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12)),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () async {
              await Clipboard.setData(ClipboardData(text: key));
              messenger.showSnackBar(
                  SnackBar(content: Text(l10n.relayAccessKeyCopied)));
            },
            child: Text(l10n.copy),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context),
            child: Text(l10n.close),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    final theme = Theme.of(context);
    final muted = theme.textTheme.bodySmall
        ?.copyWith(color: theme.colorScheme.onSurfaceVariant);
    final relays = _relays;
    return Scaffold(
      appBar: AppBar(title: Text(l10n.myRelays)),
      body: relays == null
          ? const Center(child: CircularProgressIndicator())
          : ListView(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 24),
              children: [
                Text(l10n.relaysIntro),
                const SizedBox(height: 12),
                if (relays.isEmpty)
                  Card(
                    child: ListTile(
                      leading: const Icon(Icons.wifi_tethering_off),
                      title: Text(l10n.directOnlyName),
                      subtitle: Text(l10n.directOnlyListEmpty),
                    ),
                  ),
                for (final r in relays) _tile(context, r, relays),
                const SizedBox(height: 12),
                if (!relays.contains(const RelayEntry.nightDrop()))
                  Align(
                    alignment: Alignment.centerLeft,
                    child: OutlinedButton.icon(
                      key: const Key('relays-add-nightdrop'),
                      onPressed: relays.length >= _features.maxRelays
                          ? null
                          : () => _save(
                              [const RelayEntry.nightDrop(), ...relays]),
                      icon: const Icon(Icons.add),
                      label: Text(l10n.relayAddNightDrop),
                    ),
                  ),
                const SizedBox(height: 12),
                if (relays.length >= _features.maxRelays)
                  Text(l10n.relayLimitReached('${_features.maxRelays}'),
                      style: muted)
                else
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(
                        child: TextField(
                          enableIMEPersonalizedLearning: false,
                          key: const Key('relays-field'),
                          controller: _field,
                          autocorrect: false,
                          enableSuggestions: false,
                          decoration: InputDecoration(
                            border: const OutlineInputBorder(),
                            hintText: l10n.relayAddHint,
                            errorText: _fieldError,
                            errorMaxLines: 3,
                          ),
                          onSubmitted: (_) => _add(),
                        ),
                      ),
                      const SizedBox(width: 8),
                      Padding(
                        padding: const EdgeInsets.only(top: 4),
                        child: FilledButton(
                          key: const Key('relays-add'),
                          onPressed: _add,
                          child: Text(l10n.relayAdd),
                        ),
                      ),
                    ],
                  ),
                const SizedBox(height: 16),
                Text(l10n.relayRemovedStillChecked, style: muted),
              ],
            ),
    );
  }

  Widget _tile(BuildContext context, RelayEntry r, List<RelayEntry> relays) {
    final l10n = AppLocalizations.of(context)!;
    final scheme = Theme.of(context).colorScheme;
    final down = !r.nightDrop && _reachable[r.onion] == false;
    return Card(
      child: ListTile(
        leading: Icon(r.nightDrop ? Icons.nightlight_outlined : Icons.dns_outlined),
        title: Text(
          r.nightDrop ? l10n.relayNightDropName : r.onion,
          overflow: TextOverflow.ellipsis,
        ),
        subtitle: Text(
          down
              ? l10n.relayNotAnswering
              : r.nightDrop
                  ? l10n.relayNightDropBody
                  : l10n.relayOwnBody,
          style: down ? TextStyle(color: scheme.error) : null,
        ),
        trailing: PopupMenuButton<String>(
          key: Key('relay-menu-${r.nightDrop ? 'nightdrop' : r.onion}'),
          onSelected: (value) {
            if (value == 'remove') {
              _save([for (final e in relays) if (e != r) e]);
            }
            if (value == 'key') _showAccessKey(r.onion);
          },
          itemBuilder: (context) => [
            if (!r.nightDrop)
              PopupMenuItem(value: 'key', child: Text(l10n.relayAccessKey)),
            PopupMenuItem(value: 'remove', child: Text(l10n.relayRemove)),
          ],
        ),
      ),
    );
  }
}
