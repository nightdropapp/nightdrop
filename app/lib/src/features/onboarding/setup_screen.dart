import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../../core/background_delivery.dart';
import '../../core/models.dart';
import '../relays/my_relays_screen.dart' show isRelayAddress;

/// What the user chose on [SetupScreen].
class SetupChoices {
  const SetupChoices({
    required this.coverTraffic,
    required this.backgroundDelivery,
    this.relays = const [RelayEntry.nightDrop()],
  });

  final bool coverTraffic;
  final bool backgroundDelivery;

  /// The new identity's relay list (`docs/design/optional-relay.md` §7); empty = direct only.
  final List<RelayEntry> relays;
}

enum _RelayChoice { nightDrop, own, direct }

/// The one setup step before an identity is created: the choices that change what a new identity
/// does on the network from its first minute (`docs/design/optional-relay.md` §7).
///
/// A page that pops with [SetupChoices] (or `null` for back), not dialogs awaited around
/// `createIdentity`: creating the identity makes `_Root` swap the onboarding screen for the home
/// screen, and a dialog still open then would be holding a context being torn out from under it.
/// So everything is decided here first, and the caller applies it and only then creates the
/// identity.
///
/// The relay choice defaults to the Night Drop relay, as recommended; every other option defaults
/// to off. Each says what it costs, and each can be changed later, which the page says too.
class SetupScreen extends StatefulWidget {
  const SetupScreen({
    super.key,
    this.coverTraffic = true,
    this.backgroundDelivery = false,
  });

  /// What each switch starts at. Cover traffic, a privacy setting, starts on (2026-10-18: security
  /// settings start on, and the user chooses to lower them); background delivery starts off unless
  /// already on (a preference that survived a reinstall).
  final bool coverTraffic;
  final bool backgroundDelivery;

  @override
  State<SetupScreen> createState() => _SetupScreenState();
}

class _SetupScreenState extends State<SetupScreen> {
  late bool _cover = widget.coverTraffic;
  late bool _background = widget.backgroundDelivery;
  _RelayChoice _relay = _RelayChoice.nightDrop;
  final _ownRelay = TextEditingController();
  String? _ownRelayError;

  @override
  void dispose() {
    _ownRelay.dispose();
    super.dispose();
  }

  void _finish() {
    final l10n = AppLocalizations.of(context)!;
    final List<RelayEntry> relays;
    switch (_relay) {
      case _RelayChoice.nightDrop:
        relays = const [RelayEntry.nightDrop()];
      case _RelayChoice.direct:
        relays = const [];
      case _RelayChoice.own:
        final address = _ownRelay.text.trim().toLowerCase();
        if (!isRelayAddress(address)) {
          setState(() => _ownRelayError = l10n.relayInvalidOnion);
          return;
        }
        relays = [RelayEntry.onion(address)];
    }
    Navigator.of(context).pop(SetupChoices(
      // There is no relay mailbox to cover on direct only (§6).
      coverTraffic: _cover && _relay != _RelayChoice.direct,
      backgroundDelivery: _background,
      relays: relays,
    ));
  }

  Widget _relayOption(_RelayChoice value, String title, String body,
      {Widget? badge, Widget? extra}) {
    final theme = Theme.of(context);
    return RadioListTile<_RelayChoice>(
      key: Key('setup-relay-${value.name}'),
      value: value,
      contentPadding: EdgeInsets.zero,
      title: Row(children: [
        Flexible(child: Text(title)),
        if (badge != null) ...[const SizedBox(width: 8), badge],
      ]),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(body,
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.onSurfaceVariant)),
          if (extra != null) extra,
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
    return Scaffold(
      appBar: AppBar(title: Text(l10n.setupTitle)),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 24),
          children: [
            Text(l10n.setupIntro, style: theme.textTheme.bodyMedium),
            const SizedBox(height: 16),
            Text(l10n.setupRelayTitle, style: theme.textTheme.titleMedium),
            const SizedBox(height: 4),
            RadioGroup<_RelayChoice>(
              groupValue: _relay,
              onChanged: (v) => setState(() {
                _relay = v ?? _relay;
                _ownRelayError = null;
              }),
              child: Column(children: [
                _relayOption(
                  _RelayChoice.nightDrop,
                  l10n.setupRelayNightDrop,
                  l10n.setupRelayNightDropBody,
                  badge: Chip(
                    label: Text(l10n.setupRecommended),
                    visualDensity: VisualDensity.compact,
                  ),
                ),
                _relayOption(
                  _RelayChoice.own,
                  l10n.setupRelayOwn,
                  l10n.setupRelayOwnBody,
                  extra: _relay == _RelayChoice.own
                      ? Padding(
                          padding: const EdgeInsets.only(top: 8),
                          child: TextField(
                            enableIMEPersonalizedLearning: false,
                            key: const Key('setup-relay-address'),
                            controller: _ownRelay,
                            autocorrect: false,
                            enableSuggestions: false,
                            decoration: InputDecoration(
                              border: const OutlineInputBorder(),
                              isDense: true,
                              hintText: l10n.relayAddHint,
                              errorText: _ownRelayError,
                              errorMaxLines: 3,
                            ),
                          ),
                        )
                      : null,
                ),
                _relayOption(
                  _RelayChoice.direct,
                  l10n.setupRelayDirect,
                  l10n.setupRelayDirectBody,
                ),
              ]),
            ),
            const Divider(height: 32),
            if (BackgroundDelivery.supported) ...[
              SwitchListTile(
                key: const Key('setup-background'),
                contentPadding: EdgeInsets.zero,
                title: Text(l10n.onboardingBackgroundTitle),
                value: _background,
                onChanged: (v) => setState(() => _background = v),
              ),
              Text(l10n.onboardingBackgroundBody, style: muted),
              const Divider(height: 32),
            ],
            // Only with a relay: direct only has no mailbox to cover (`optional-relay.md` §6).
            if (_relay != _RelayChoice.direct) ...[
              SwitchListTile(
                key: const Key('setup-cover'),
                contentPadding: EdgeInsets.zero,
                title: Text(l10n.setupCoverTitle),
                value: _cover,
                onChanged: (v) => setState(() => _cover = v),
              ),
              Text(l10n.coverTrafficBody, style: muted),
              const SizedBox(height: 8),
              Text(l10n.coverTrafficLimit, style: muted),
              const SizedBox(height: 24),
            ],
            Text(l10n.setupChangeLater, style: muted),
            const SizedBox(height: 16),
            FilledButton(
              key: const Key('setup-create'),
              onPressed: _finish,
              child: Text(l10n.createMyIdentity),
            ),
          ],
        ),
      ),
    );
  }
}
