import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../../core/background_delivery.dart';

/// What the user chose on [SetupScreen].
class SetupChoices {
  const SetupChoices({
    required this.coverTraffic,
    required this.backgroundDelivery,
  });

  final bool coverTraffic;
  final bool backgroundDelivery;
}

/// The one setup step before an identity is created: the choices that change what a new identity
/// does on the network from its first minute (`docs/design/optional-relay.md` §7).
///
/// A page that pops with [SetupChoices] (or `null` for back), not dialogs awaited around
/// `createIdentity`: creating the identity makes `_Root` swap the onboarding screen for the home
/// screen, and a dialog still open then would be holding a context being torn out from under it.
/// So everything is decided here first, and the caller applies it and only then creates the
/// identity.
///
/// Every option defaults to off and says what it costs. Each can be changed later from the home
/// menu, which the page says too.
class SetupScreen extends StatefulWidget {
  const SetupScreen({
    super.key,
    this.coverTraffic = false,
    this.backgroundDelivery = false,
  });

  /// What each switch starts at: off, unless already on (a preference that survived a reinstall).
  final bool coverTraffic;
  final bool backgroundDelivery;

  @override
  State<SetupScreen> createState() => _SetupScreenState();
}

class _SetupScreenState extends State<SetupScreen> {
  late bool _cover = widget.coverTraffic;
  late bool _background = widget.backgroundDelivery;

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
            Text(l10n.setupChangeLater, style: muted),
            const SizedBox(height: 16),
            FilledButton(
              key: const Key('setup-create'),
              onPressed: () => Navigator.of(context).pop(SetupChoices(
                coverTraffic: _cover,
                backgroundDelivery: _background,
              )),
              child: Text(l10n.createMyIdentity),
            ),
          ],
        ),
      ),
    );
  }
}
