import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/material.dart';

import '../l10n/app_localizations.dart';
import 'core/background_delivery.dart';
import 'core/nightdrop_core.dart';
import 'core/notifications.dart';
import 'features/home/home_screen.dart';
import 'features/lock/lock_screen.dart';
import 'features/onboarding/onboarding_screen.dart';
import 'theme/theme.dart';

/// Exposes the [NightdropCore] to the widget tree. Read it with `NightdropScope.of(context)`;
/// widgets rebuild when the core notifies.
class NightdropScope extends InheritedNotifier<NightdropCore> {
  const NightdropScope({super.key, required NightdropCore core, required super.child})
      : super(notifier: core);

  static NightdropCore of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<NightdropScope>();
    assert(scope?.notifier != null, 'NightdropScope not found in the widget tree');
    return scope!.notifier!;
  }
}

class NightdropApp extends StatelessWidget {
  const NightdropApp({super.key, required this.core});

  final NightdropCore core;

  @override
  Widget build(BuildContext context) {
    return NightdropScope(
      core: core,
      child: MaterialApp(
        onGenerateTitle: (context) => AppLocalizations.of(context)!.appTitle,
        debugShowCheckedModeBanner: false,
        theme: nightdropTheme(),
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: const _Root(),
      ),
    );
  }
}

/// Routes between a launch splash, onboarding, and the home/chat list. On first build it
/// asks the core to restore any persisted identity; while that runs it shows a splash.
class _Root extends StatefulWidget {
  const _Root();

  @override
  State<_Root> createState() => _RootState();
}

class _RootState extends State<_Root> with WidgetsBindingObserver {
  bool _started = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    NotificationService.init();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final foreground = state == AppLifecycleState.resumed;
    final core = NightdropScope.of(context);
    core.setLifecycle(foreground);
    // Opt-in Android foreground service (#13): keep the process alive in the background so the
    // poller's Tor `peek` keeps delivering notifications; stop it when back in the foreground.
    BackgroundDelivery.onLifecycle(
      foreground: foreground,
      hasIdentity: core.identity != null,
    );
    // Re-lock on leaving the foreground, so the secret is required again on return rather than
    // once per install. A no-op when background delivery is on — that mode needs the key resident
    // to receive anything (see docs/design/app-lock.md §5) — and when no lock is set at all.
    //
    // Deliberately `paused`/`hidden` only, NOT `inactive`. On Android `inactive` fires transiently
    // while the app is still on screen — a dialog taking focus, the keyboard opening, the
    // notification shade — and re-locking there produced a real bug: disabling the app lock threw
    // the user onto the lock screen, because the re-lock ran mid-flow while the lock file still
    // existed, and left `_lockedOut` set after it was gone. The app-switcher thumbnail is not
    // affected: that is `FLAG_SECURE` in `MainActivity.onPause`, independent of this.
    final gone = state == AppLifecycleState.paused || state == AppLifecycleState.hidden;
    if (gone) unawaited(core.lockStore());
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_started) return;
    _started = true;
    // Auto-restore a saved identity (no-op if none); _Root rebuilds when it finishes.
    // Deferred past the first frame: start() can notify synchronously (the non-Tor path
    // has no await before its finally-notify), and notifying mid-build throws
    // "setState() or markNeedsBuild() called during build".
    final core = NightdropScope.of(context);
    WidgetsBinding.instance.addPostFrameCallback((_) => core.start());
  }

  @override
  Widget build(BuildContext context) {
    final core = NightdropScope.of(context);
    return ListenableBuilder(
      listenable: core,
      builder: (context, _) {
        if (core.isBooting) return const _Splash();
        // Saved data exists but wouldn't open: don't pretend it's a fresh install (onboarding
        // would overwrite it). Show an explicit recovery choice instead.
        if (core.loadError) return const _LoadErrorScreen();
        // BEFORE the identity check: a locked store can't expose an identity, and reading that as
        // a fresh install would send the user to onboarding, which overwrites recoverable data.
        if (core.needsUnlock) return const LockScreen();
        return core.identity == null
            ? const OnboardingScreen()
            : const HomeScreen();
      },
    );
  }
}

/// Splash while the core checks for (and restores) a saved identity at launch. Restoring a Tor
/// identity re-bootstraps a circuit, which can take up to a minute, so we cycle staged status lines
/// (rather than a static label) to show progress. A bounded bootstrap (core `BOOTSTRAP_TIMEOUT`)
/// eventually surfaces an error + retry if the network is dead/blocked, so this never spins forever.
class _Splash extends StatefulWidget {
  const _Splash();

  @override
  State<_Splash> createState() => _SplashState();
}

class _SplashState extends State<_Splash> {
  // Fixed number of staged lines; the localized text is resolved in [build].
  static const _stageCount = 4;
  int _i = 0;
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _timer = Timer.periodic(const Duration(seconds: 6), (_) {
      if (_i < _stageCount - 1 && mounted) setState(() => _i++);
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    final stages = [
      l10n.splashRestoringSession,
      l10n.splashReconnectingTor,
      l10n.splashRebuildingCircuit,
      l10n.splashAlmostThere,
    ];
    return Scaffold(
      body: Center(
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            const Text('👻', style: TextStyle(fontSize: 64)),
            const SizedBox(height: 16),
            const SizedBox(
              height: 22,
              width: 22,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
            const SizedBox(height: 16),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 32),
              child: Text(stages[_i], textAlign: TextAlign.center),
            ),
          ],
        ),
      ),
    );
  }
}

/// Whether this is a phone/tablet, for the platform-appropriate recovery wording. `Platform` is
/// unavailable on web, which the app doesn't target; `kIsWeb` guards aren't needed here.
bool get _isMobile => Platform.isAndroid || Platform.isIOS;

/// Shown when saved data exists on this device but couldn't be opened at launch. We deliberately do
/// NOT drop straight to onboarding here: creating a new identity would overwrite the existing state
/// file. Instead we offer to retry (the failure may be transient) or to continue to setup — after
/// which the unreadable bytes have already been preserved as a sidecar copy.
class _LoadErrorScreen extends StatelessWidget {
  const _LoadErrorScreen();

  @override
  Widget build(BuildContext context) {
    final core = NightdropScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    return Scaffold(
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(32),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(Icons.lock_outline, size: 48),
              const SizedBox(height: 16),
              Text(
                l10n.loadErrorTitle,
                style: Theme.of(context).textTheme.titleMedium,
                textAlign: TextAlign.center,
              ),
              const SizedBox(height: 12),
              Text(
                // Platform-specific on purpose: the desktop copy names KDE Wallet / GNOME
                // Keyring, which is meaningless on a phone and sends the user hunting for
                // something that doesn't exist there. The mobile equivalent isn't a thing they
                // can unlock by hand, so the wording points at the causes they can actually act on.
                _isMobile ? l10n.loadErrorBodyMobile : l10n.loadErrorBody,
                textAlign: TextAlign.center,
              ),
              const SizedBox(height: 24),
              FilledButton.icon(
                onPressed: () => core.retryStart(),
                icon: const Icon(Icons.refresh),
                label: Text(l10n.tryAgain),
              ),
              const SizedBox(height: 8),
              TextButton(
                onPressed: () => core.dismissLoadError(),
                child: Text(l10n.setUpNewIdentity),
              ),
              const SizedBox(height: 8),
              Text(
                l10n.loadErrorFootnote,
                textAlign: TextAlign.center,
                style: const TextStyle(fontSize: 12),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
