import 'dart:async';
import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/app_config.dart';
import '../../core/app_version.dart';
import '../../core/background_delivery.dart';
import '../../core/nightdrop_core.dart';
import '../../core/models.dart';
import '../backup/backup_actions.dart';
import '../chat/chat_screen.dart';
import '../donations/donations_screen.dart';
import '../lock/app_lock_settings.dart';
import '../pairing/pairing_screen.dart';

/// The conversation list. Empty until the user pairs with someone.
class HomeScreen extends StatelessWidget {
  const HomeScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final core = NightdropScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    return Scaffold(
      appBar: AppBar(
        title: Text(l10n.chats),
        actions: [
          IconButton(
            tooltip: l10n.supportNightDrop,
            icon: const Icon(Icons.volunteer_activism_outlined),
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute<void>(builder: (_) => const DonationsScreen()),
            ),
          ),
          PopupMenuButton<String>(
            tooltip: l10n.backUp,
            icon: const Icon(Icons.backup_outlined),
            onSelected: (value) {
              if (value == 'file') createAndSaveBackup(context, core);
              if (value == 'server') _createServerBackup(context, core);
              if (value == 'merge') mergeChatBackup(context, core);
            },
            itemBuilder: (context) => [
              PopupMenuItem(value: 'file', child: Text(l10n.saveBackupFile)),
              PopupMenuItem(
                  value: 'server', child: Text(l10n.backUpToServer24h)),
              PopupMenuItem(
                  value: 'merge', child: Text(l10n.mergeChatBackupMenu)),
            ],
          ),
          PopupMenuButton<String>(
            onSelected: (value) {
              if (value == 'identity') _showMyIdentity(context, core);
              if (value == 'background') _backgroundDeliverySettings(context);
              if (value == 'applock') showAppLockSettings(context, core);
              if (value == 'duress') showDuressSettings(context, core);
              if (value == 'cover') _coverTrafficSettings(context, core);
              if (value == 'relays') _editRelays(context, core);
              if (value == 'resettor') _confirmResetTor(context, core);
              if (value == 'update') _updateApp(context, core);
              if (value == 'about') _showAbout(context);
              if (value == 'logout') _confirmLogout(context, core);
            },
            itemBuilder: (context) => [
              PopupMenuItem(value: 'identity', child: Text(l10n.myIdentity)),
              if (BackgroundDelivery.supported)
                PopupMenuItem(
                    value: 'background', child: Text(l10n.backgroundDeliveryMenu)),
              PopupMenuItem(value: 'applock', child: Text(l10n.appLockMenu)),
              // Its own row, and deliberately stateless in the label: "Wipe code" reads the same
              // whether or not one is armed, so a glance at an unlocked phone gives nothing away.
              // The feature itself is public; only *your* having armed it is worth hiding (#3).
              PopupMenuItem(value: 'duress', child: Text(l10n.duressMenu)),
              PopupMenuItem(value: 'cover', child: Text(l10n.coverTrafficMenu)),
              PopupMenuItem(value: 'relays', child: Text(l10n.myRelaysMenu)),
              PopupMenuItem(value: 'resettor', child: Text(l10n.resetTorMenu)),
              PopupMenuItem(value: 'update', child: Text(l10n.updateApp)),
              PopupMenuItem(value: 'about', child: Text(l10n.aboutMenu)),
              PopupMenuItem(
                  value: 'logout', child: Text(l10n.logoutDeleteMenu)),
            ],
          ),
        ],
      ),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: () => Navigator.of(context).push(
          MaterialPageRoute<void>(builder: (_) => const PairingScreen()),
        ),
        icon: const Icon(Icons.qr_code_2),
        label: Text(l10n.newChat),
      ),
      body: Column(
        children: [
          _OnionBanner(core: core),
          _RelayHealthBanner(core: core),
          _BackupReminderBanner(core: core),
          _UpdateBanner(core: core),
          Expanded(
            child: ListenableBuilder(
        listenable: core,
        builder: (context, _) {
          final requests = core.incomingRequests;
          final contacts = core.contacts;
          if (requests.isEmpty && contacts.isEmpty) {
            return Center(
              child: Padding(
                padding: const EdgeInsets.all(32),
                child: Text(
                  l10n.noChatsYet,
                  textAlign: TextAlign.center,
                ),
              ),
            );
          }
          return ListView(
            children: [
              for (final r in requests) _RequestTile(request: r, core: core),
              for (final c in contacts)
                // Long-press (touch) or right-click (desktop) a chat to delete it.
                GestureDetector(
                  onLongPress: () => _confirmDeleteChat(context, core, c),
                  onSecondaryTapDown: (_) => _confirmDeleteChat(context, core, c),
                  child: ListTile(
                    leading: const ExcludeSemantics(
                      child: CircleAvatar(child: Text('👻')),
                    ),
                    title: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Flexible(child: Text(c.displayName)),
                        if (c.showIdentityTag) ...[
                          const SizedBox(width: 6),
                          IdentityTag(tag: c.identityTag),
                        ],
                        if (c.verified) ...[
                          const SizedBox(width: 6),
                          Icon(Icons.verified_user,
                              semanticLabel: l10n.verified,
                              size: 15,
                              color: Theme.of(context).colorScheme.primary),
                        ],
                      ],
                    ),
                    subtitle: c.remoteStorage
                        ? Text(l10n.storedOnServer24h)
                        : Text(l10n.storedOnThisDevice),
                    // Counted once per tile: unreadCount scans the chat's history.
                    trailing: switch (core.unreadCount(c.id)) {
                      0 => null,
                      final n => Badge(label: Text('$n')),
                    },
                    onTap: () => Navigator.of(context).push(
                      MaterialPageRoute<void>(
                        builder: (_) => ChatScreen(contactId: c.id),
                      ),
                    ),
                  ),
                ),
            ],
          );
        },
            ),
          ),
        ],
      ),
    );
  }
}

/// Cover traffic (#4). Opt-in, and the dialog carries the limit as prominently as the benefit:
/// this is chaff, not constant-rate transmission, so it raises the cost of traffic analysis
/// without ending it. A user who believes it makes them untrackable is worse off than one who
/// knows what it actually buys. See `docs/design/cover-traffic.md` §4.
/// Manually reset the Tor connection. Confirmed first because it drops the connection and
/// reconnects, which takes a minute or two — but it is the only remedy for a guard set that has
/// churned out of the network, and until this existed a user in that state could only reinstall.
///
/// Says plainly what it does and does not touch: the identity and chats are untouched, and the
/// `.onion` address is kept, so nobody loses contacts by trying it.
Future<void> _confirmResetTor(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final go = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.resetTorTitle),
      content: Text(l10n.resetTorBody),
      actions: [
        TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.cancel)),
        FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: Text(l10n.resetTorConfirm)),
      ],
    ),
  );
  if (go != true || !context.mounted) return;
  ScaffoldMessenger.of(context)
      .showSnackBar(SnackBar(content: Text(l10n.resetTorRunning)));
  await core.resetTorConnection();
}

Future<void> _coverTrafficSettings(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final on = await core.coverTrafficEnabled();
  if (!context.mounted) return;
  final theme = Theme.of(context);
  final turnOn = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.coverTrafficTitle),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(l10n.coverTrafficBody),
            const SizedBox(height: 16),
            Text(
              l10n.coverTrafficLimit,
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
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
          onPressed: () => Navigator.of(context).pop(!on),
          child: Text(on ? l10n.turnOff : l10n.turnOn),
        ),
      ],
    ),
  );
  if (turnOn == null || !context.mounted) return;
  final messenger = ScaffoldMessenger.of(context);
  await core.setCoverTraffic(turnOn);
  messenger.showSnackBar(SnackBar(
    content: Text(turnOn ? l10n.coverTrafficOn : l10n.coverTrafficOff),
  ));
}

/// A dismissible banner shown while this device's onion descriptor is still publishing to Tor
/// (the ~1–3 min after launch), during which peers can't reach us to pair. Polls [onionReady]
/// and disappears once we're reachable.
class _OnionBanner extends StatefulWidget {
  const _OnionBanner({required this.core});

  final NightdropCore core;

  @override
  State<_OnionBanner> createState() => _OnionBannerState();
}

class _OnionBannerState extends State<_OnionBanner> {
  bool _ready = true;
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _check();
    _timer = Timer.periodic(const Duration(seconds: 4), (_) => _check());
  }

  Future<void> _check() async {
    final ready = await widget.core.onionReady();
    if (!mounted) return;
    if (ready != _ready) setState(() => _ready = ready);
    if (ready) _timer?.cancel(); // reachable — no need to keep polling
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_ready) return const SizedBox.shrink();
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.secondaryContainer,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
        child: Row(
          children: [
            const SizedBox(
              width: 16,
              height: 16,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Text(
                AppLocalizations.of(context)!.publishingAddressTor,
                style: TextStyle(color: scheme.onSecondaryContainer, fontSize: 12.5),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Warns when one of the user's **own** advertised extra relays (#17) has stopped answering —
/// i.e. a self-hosted relay is down, so contacts' offline mail routed through it may be stuck.
/// Nudges the user to add a backup relay so delivery stays redundant. Polls on the same cadence
/// as the onion banner (relay health is refreshed by the background poller).
class _RelayHealthBanner extends StatefulWidget {
  const _RelayHealthBanner({required this.core});

  final NightdropCore core;

  @override
  State<_RelayHealthBanner> createState() => _RelayHealthBannerState();
}

class _RelayHealthBannerState extends State<_RelayHealthBanner> {
  List<RelayHealth> _health = const [];
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _check();
    _timer = Timer.periodic(const Duration(seconds: 6), (_) => _check());
  }

  Future<void> _check() async {
    final health = await widget.core.relayHealth();
    if (!mounted) return;
    setState(() => _health = health);
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    final offline = _health.where((h) => !h.reachable).toList();
    if (offline.isEmpty) return const SizedBox.shrink();
    final scheme = Theme.of(context).colorScheme;
    final hasBackup = _health.any((h) => h.reachable);
    final names = offline.map((h) => h.address).join(', ');
    final message = offline.length == 1
        ? l10n.relayOfflineOne(names)
        : l10n.relayOfflineMany(names);
    final advice = hasBackup
        ? l10n.relayAdviceHasBackup
        : l10n.relayAdviceNoBackup;
    return Material(
      color: scheme.errorContainer,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
        child: Row(
          children: [
            Icon(Icons.wifi_off, size: 18, color: scheme.onErrorContainer),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                '$message$advice',
                style:
                    TextStyle(color: scheme.onErrorContainer, fontSize: 12.5),
              ),
            ),
            const SizedBox(width: 8),
            TextButton(
              onPressed: () => _editRelays(context, widget.core),
              child: Text(l10n.relaysShort),
            ),
          ],
        ),
      ),
    );
  }
}

/// Gentle, dismissible reminder to back up once there are chats worth losing and no backup has
/// been made yet (lost backup / password = lost data, by design — §7). "Back up" opens the file
/// backup; "Later" snoozes it. Hidden entirely once a backup succeeds or while snoozed.
/// "A newer release exists" — shown only when our onion site says so (see `core/src/update.rs`).
///
/// Deliberately not dismissible-with-memory and deliberately quiet: it carries no urgency styling
/// and no download button. The app never fetches or installs a build; the user goes and gets it.
/// It disappears by itself once the running version matches, so there is nothing to dismiss.
/// "Update app" — the way back to an update the user hid, and the way to ask on demand rather
/// than waiting for the daily check.
///
/// Downloads and verifies; it never installs. Forces a check first, because the point of asking
/// is not to be told what yesterday's check thought.
Future<void> _updateApp(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final messenger = ScaffoldMessenger.of(context);
  messenger.showSnackBar(SnackBar(content: Text(l10n.updateChecking)));
  final answered = await core.checkForUpdateNow();
  final version = core.updateAvailable;
  if (!context.mounted) return;
  if (!answered) {
    // The site did not answer. Saying "up to date" here would be a confident lie on the one
    // screen where the user deliberately asked.
    messenger.showSnackBar(SnackBar(content: Text(l10n.updateCheckFailed)));
    return;
  }
  if (version == null) {
    messenger.showSnackBar(SnackBar(content: Text(l10n.updateUpToDate)));
    return;
  }
  final go = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      content: Text(l10n.updateAvailableBody(version)),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: Text(l10n.close),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: Text(l10n.updateDownload),
        ),
      ],
    ),
  );
  if (go != true) return;
  messenger.showSnackBar(SnackBar(content: Text(l10n.updateDownloading)));
  final path = await core.downloadUpdate();
  messenger.showSnackBar(SnackBar(
    content: Text(path == null ? l10n.updateFailed : '${l10n.updateDownloaded}\n$path'),
    duration: const Duration(seconds: 8),
  ));
}

class _UpdateBanner extends StatefulWidget {
  const _UpdateBanner({required this.core});

  final NightdropCore core;

  @override
  State<_UpdateBanner> createState() => _UpdateBannerState();
}

class _UpdateBannerState extends State<_UpdateBanner> {
  bool _busy = false;

  /// True while *any* download runs, including one the menu started. Asking the core rather than
  /// only tracking our own tap is what stops the banner offering to start a second one — tapping
  /// it to watch a download in progress used to do exactly that.
  bool get _downloading => _busy || widget.core.downloadInProgress;

  /// "45%" once there is a figure, empty until then. Empty rather than "0%" on purpose: a Tor
  /// circuit can take tens of seconds to produce the first byte, and "0%" for half a minute reads
  /// as stuck where a bare "Downloading…" reads as starting.
  String get _percentLabel {
    final p = widget.core.downloadProgress;
    return p == null ? '' : ' ${(p * 100).round()}%';
  }

  /// Tapping the banner downloads; it never installs. The file is verified against the hash the
  /// onion site published, then handed to the user — Android decides whether it may replace the
  /// app, and it refuses anything not signed by our release key.
  Future<void> _download() async {
    if (_downloading) return;
    setState(() => _busy = true);
    final l10n = AppLocalizations.of(context)!;
    final path = await widget.core.downloadUpdate();
    if (!mounted) return;
    setState(() => _busy = false);
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(
      content: Text(path == null ? l10n.updateFailed : '${l10n.updateDownloaded}\n$path'),
      duration: const Duration(seconds: 8),
    ));
  }

  @override
  Widget build(BuildContext context) {
    final version = widget.core.updateAvailable;
    if (version == null) return const SizedBox.shrink();
    final l10n = AppLocalizations.of(context)!;
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.secondaryContainer,
      child: InkWell(
        onTap: _downloading ? null : _download,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
          child: Row(
            children: [
              Icon(Icons.system_update_alt,
                  size: 18, color: scheme.onSecondaryContainer),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      _downloading
                          ? l10n.updateDownloadingPercent(_percentLabel)
                          : l10n.updateAvailableBody(version),
                      style: TextStyle(
                          color: scheme.onSecondaryContainer, fontSize: 12.5),
                    ),
                    // Only while downloading, and only then: a bar sitting under a plain notice
                    // would read as progress toward something the user has not started.
                    if (_downloading) ...[
                      const SizedBox(height: 6),
                      ClipRRect(
                        borderRadius: BorderRadius.circular(2),
                        // A null value renders the indeterminate animation, which is the honest
                        // display when the server did not say how big the file is.
                        child: LinearProgressIndicator(
                          value: widget.core.downloadProgress,
                          minHeight: 3,
                          backgroundColor:
                              scheme.onSecondaryContainer.withValues(alpha: 0.15),
                        ),
                      ),
                    ],
                  ],
                ),
              ),
              // Hiding is scoped to this version: a later release shows again, so a hidden
              // banner can never swallow the notice that actually matters.
              TextButton(
                onPressed: _downloading ? null : widget.core.hideUpdateBanner,
                child: Text(l10n.updateHide),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _BackupReminderBanner extends StatefulWidget {
  const _BackupReminderBanner({required this.core});

  final NightdropCore core;

  @override
  State<_BackupReminderBanner> createState() => _BackupReminderBannerState();
}

class _BackupReminderBannerState extends State<_BackupReminderBanner> {
  bool _show = false;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    final show = await widget.core.shouldSuggestBackup();
    if (mounted) setState(() => _show = show);
  }

  @override
  Widget build(BuildContext context) {
    if (!_show) return const SizedBox.shrink();
    final l10n = AppLocalizations.of(context)!;
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.secondaryContainer,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
        child: Row(
          children: [
            Icon(Icons.backup_outlined,
                size: 18, color: scheme.onSecondaryContainer),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                l10n.backupReminderBody,
                style:
                    TextStyle(color: scheme.onSecondaryContainer, fontSize: 12.5),
              ),
            ),
            TextButton(
              onPressed: () async {
                await widget.core.snoozeBackupReminder();
                if (mounted) setState(() => _show = false);
              },
              child: Text(l10n.later),
            ),
            FilledButton(
              onPressed: () async {
                await createAndSaveBackup(context, widget.core);
                // recordBackupDone (on success) makes this false; re-check either way.
                await _refresh();
              },
              child: Text(l10n.backUp),
            ),
          ],
        ),
      ),
    );
  }
}

/// Confirm and delete a chat from the list (long-press on touch, right-click on desktop). The
/// deletion is optimistic — the chat disappears at once while the peer is signalled in the
/// background — so we stay on the list, no navigation needed.
Future<void> _confirmDeleteChat(
    BuildContext context, NightdropCore core, Contact contact) async {
  final l10n = AppLocalizations.of(context)!;
  final confirmed = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.deleteThisChat),
      content: Text(l10n.deleteChatBody(contact.theirName)),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context, false),
          child: Text(l10n.cancel),
        ),
        FilledButton.tonal(
          style: FilledButton.styleFrom(
            backgroundColor: Theme.of(context).colorScheme.errorContainer,
            foregroundColor: Theme.of(context).colorScheme.onErrorContainer,
          ),
          onPressed: () => Navigator.pop(context, true),
          child: Text(l10n.deleteChat),
        ),
      ],
    ),
  );
  if (confirmed != true) return;
  await core.deleteChat(contact.id);
}

/// Opt-in Android background delivery (§11.8, #13): a switch that, when on, runs a foreground
/// service (persistent notification) so messages keep arriving while the app is backgrounded.
Future<void> _backgroundDeliverySettings(BuildContext context) async {
  final l10n = AppLocalizations.of(context)!;
  final messenger = ScaffoldMessenger.of(context);
  final enabled = await BackgroundDelivery.isEnabled();
  if (!context.mounted) return;
  final result = await showDialog<bool>(
    context: context,
    builder: (context) {
      var value = enabled;
      return StatefulBuilder(
        builder: (context, setState) => AlertDialog(
          title: Text(l10n.backgroundDelivery),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(l10n.backgroundDeliveryBody),
              const SizedBox(height: 8),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: Text(l10n.enabled),
                value: value,
                onChanged: (v) => setState(() => value = v),
              ),
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: Text(l10n.cancel),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, value),
              child: Text(l10n.save),
            ),
          ],
        ),
      );
    },
  );
  if (result == null) return;
  if (result) {
    final granted = await BackgroundDelivery.ensurePermission();
    if (!granted) {
      messenger.showSnackBar(SnackBar(
        content: Text(l10n.notificationPermissionRequired),
      ));
      return;
    }
  }
  await BackgroundDelivery.setEnabled(result);
  messenger.showSnackBar(SnackBar(
    content: Text(result ? l10n.backgroundDeliveryOn : l10n.backgroundDeliveryOff),
  ));
}

/// Edit our advertised **extra** relay set (#17). These are announced to contacts so their
/// offline mail is fanned out to them in addition to the shared primary relay — more paths to
/// reach us if one relay is down or censored. One relay address per line.
Future<void> _editRelays(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final messenger = ScaffoldMessenger.of(context);
  final current = await core.myRelays();
  if (!context.mounted) return;
  final controller = TextEditingController(text: current.join('\n'));
  final saved = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      // scrollable keeps the description + text field in a scroll view and the Cancel/Save
      // actions fixed below, so on mobile the keyboard can't push the field under the buttons
      // or overflow the dialog.
      scrollable: true,
      title: Text(l10n.myRelays),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(l10n.editRelaysBody),
          const SizedBox(height: 12),
          TextField(
            controller: controller,
            minLines: 2,
            maxLines: 5,
            autocorrect: false,
            enableSuggestions: false,
            decoration: const InputDecoration(
              border: OutlineInputBorder(),
              hintText: 'abcd…xyz.onion\n10.0.0.5:9080',
            ),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context, false),
          child: Text(l10n.cancel),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(context, true),
          child: Text(l10n.save),
        ),
      ],
    ),
  );
  if (saved != true) return;
  final relays = controller.text
      .split('\n')
      .map((l) => l.trim())
      .where((l) => l.isNotEmpty)
      .toList();
  try {
    await core.setMyRelays(relays);
  } catch (e) {
    messenger.showSnackBar(SnackBar(content: Text(l10n.couldNotSaveRelays(e.toString()))));
    return;
  }
  messenger.showSnackBar(SnackBar(
    content: Text(relays.isEmpty
        ? l10n.usingDefaultRelayOnly
        : l10n.advertisingExtraRelays(relays.length)),
  ));
}

/// Enable an opt-in server backup (§7c): store an encrypted copy on the relay and force the
/// "record this password" + exact-expiry acknowledgment the invariant requires.
Future<void> _createServerBackup(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final full = await pickBackupMode(context);
  if (full == null || !context.mounted) return;
  final ScaffoldMessengerState messenger = ScaffoldMessenger.of(context);
  final ServerBackup info;
  try {
    // Argon2 derivation + the Tor upload take a moment — show a loader.
    info = await runWithLoader(
      context,
      l10n.backingUpToServer,
      () => core.createServerBackup(24, full),
    );
  } catch (e) {
    messenger.showSnackBar(
      SnackBar(content: Text(l10n.couldNotBackUpToServer(e.toString()))),
    );
    return;
  }
  if (!context.mounted) return;
  final expiry = info.expiresAt.toLocal();
  final expiryText =
      '${expiry.year}-${expiry.month.toString().padLeft(2, '0')}-${expiry.day.toString().padLeft(2, '0')} '
      '${expiry.hour.toString().padLeft(2, '0')}:${expiry.minute.toString().padLeft(2, '0')}';
  // Same confirm-by-retype gate as the file backup (§7). The server copy is already stored, so this
  // can't un-store it — but it forces the user to actually record the password while it's still on
  // screen, before it's gone for good.
  await acknowledgeRecoveryPassword(
    context,
    password: info.password,
    intro: l10n.serverBackupIntro,
    footer: l10n.serverBackupFooter(expiryText),
  );
  await core.recordBackupDone(); // stop the backup-reminder nudge
}

/// Show this device's own anonymous identity (the id others key you by).
Future<void> _showMyIdentity(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final id = core.identity?.id ?? '(none)';
  await showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.myIdentity),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(l10n.myIdentityBody),
          const SizedBox(height: 12),
          SelectableText(id, style: const TextStyle(fontFamily: 'monospace', fontSize: 13)),
        ],
      ),
      actions: [
        FilledButton(onPressed: () => Navigator.pop(context), child: Text(l10n.close)),
      ],
    ),
  );
}

/// Minimal about dialog: icon, app name, version, copyright + license. Deliberately a custom
/// dialog rather than Flutter's `showAboutDialog`, which auto-adds a "View licenses" button (the
/// full bundled-package license list) and a "Powered by Flutter" footer we don't want here.
void _showAbout(BuildContext context) {
  final parts = kAppVersion.split('+');
  final version = parts.length == 2 ? '${parts[0]} (build ${parts[1]})' : kAppVersion;
  final l10n = AppLocalizations.of(context)!;
  showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      content: Row(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Image.asset('assets/icons/icon-512.png', width: 56, height: 56),
          const SizedBox(width: 16),
          Flexible(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(AppConfig.current.appName,
                    style: Theme.of(context).textTheme.titleLarge),
                const SizedBox(height: 2),
                Text('Version $version',
                    style: Theme.of(context).textTheme.bodyMedium),
                const SizedBox(height: 12),
                const Text('© 2026 Night Drop'),
                const Text('AGPL-3.0-or-later'),
              ],
            ),
          ),
        ],
      ),
      actions: [
        FilledButton(onPressed: () => Navigator.pop(context), child: Text(l10n.close)),
      ],
    ),
  );
}

/// Confirm and perform a logout / identity termination, spelling out the consequences.
Future<void> _confirmLogout(BuildContext context, NightdropCore core) async {
  // Grab the app-level messenger up front: the home screen is about to be replaced by onboarding,
  // but the root ScaffoldMessenger survives, so a follow-up notice still shows (and capturing it
  // before any await avoids using a stale BuildContext across the async gap).
  final l10n = AppLocalizations.of(context)!;
  final messenger = ScaffoldMessenger.of(context);
  final confirmed = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.logoutTitle),
      content: Text(l10n.logoutBody),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context, false),
          child: Text(l10n.cancel),
        ),
        FilledButton.tonal(
          style: FilledButton.styleFrom(
            backgroundColor: Theme.of(context).colorScheme.errorContainer,
            foregroundColor: Theme.of(context).colorScheme.onErrorContainer,
          ),
          onPressed: () => Navigator.pop(context, true),
          child: Text(l10n.logoutDelete),
        ),
      ],
    ),
  );
  if (confirmed != true) return;
  // _Root routes back to onboarding; logout returns how many contacts couldn't be told the chat
  // was deleted (§1.3) so we can be honest that a few peers may still message a dead identity.
  final notNotified = await core.logout();
  if (notNotified > 0) {
    messenger.showSnackBar(SnackBar(
      content: Text(l10n.logoutNotNotified(notNotified)),
    ));
  }
}

/// An inbound chat request: approve to start chatting, or decline to drop it (§5).
/// Approval is sent over Tor and can take a few seconds, so the tile shows progress and
/// disables its buttons while it is in flight (no accidental double-approvals).
class _RequestTile extends StatefulWidget {
  const _RequestTile({required this.request, required this.core});

  final Contact request;
  final NightdropCore core;

  @override
  State<_RequestTile> createState() => _RequestTileState();
}

class _RequestTileState extends State<_RequestTile> {
  bool _busy = false;

  Future<void> _decide(bool accept) async {
    final l10n = AppLocalizations.of(context)!;
    setState(() => _busy = true);
    try {
      await widget.core.authorize(widget.request.id, accept);
    } catch (e) {
      if (mounted) {
        setState(() => _busy = false);
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
              content: Text(accept
                  ? l10n.couldNotApprove(e.toString())
                  : l10n.couldNotDecline(e.toString()))),
        );
      }
    }
    // On success the tile disappears (it's no longer a pending request); no need to reset.
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    final scheme = Theme.of(context).colorScheme;
    return Container(
      color: scheme.secondaryContainer,
      child: ListTile(
        leading: const CircleAvatar(child: Icon(Icons.person_add_alt_1)),
        title: Text(l10n.chatRequest),
        subtitle: Text(
          _busy
              ? l10n.approvingOverTor
              : l10n.requestFrom(shortId(widget.request.id)),
        ),
        trailing: _busy
            ? const SizedBox(
                height: 22,
                width: 22,
                child: CircularProgressIndicator(strokeWidth: 2),
              )
            : Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  IconButton(
                    tooltip: l10n.decline,
                    icon: const Icon(Icons.close),
                    onPressed: () => _decide(false),
                  ),
                  IconButton(
                    tooltip: l10n.approve,
                    icon: const Icon(Icons.check),
                    onPressed: () => _decide(true),
                  ),
                ],
              ),
      ),
    );
  }
}
