import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:flutter_zxing/flutter_zxing.dart';
import 'package:permission_handler/permission_handler.dart';

import '../../../l10n/app_localizations.dart';

/// Whether this device can scan a QR with its camera. Only the mobile builds ship a `camera`
/// plugin backend; the desktop builds have none, so [ScanScreen] would otherwise show a blank
/// camera that never fires a scan. Callers use this to offer paste-the-link pairing instead.
bool get canScanQr => Platform.isAndroid || Platform.isIOS;

/// Returns true if [raw] is a pre-authorized Night Drop pairing payload (§5a):
/// `nightdrop://pair?addr=...&ik=...&otk=...`.
bool isNightdropInvite(String raw) {
  final uri = Uri.tryParse(raw);
  return uri != null &&
      uri.scheme == 'nightdrop' &&
      uri.host == 'pair' &&
      uri.queryParameters.containsKey('ik');
}

/// Returns true if [raw] looks like a short code (`slot-secret-words`): lowercase
/// alphanumeric segments joined by hyphens, at least a slot plus two words.
final _shortCodePattern = RegExp(r'^[a-z0-9]+(-[a-z0-9]+){2,}$');
bool looksLikeShortCode(String raw) => _shortCodePattern.hasMatch(raw.trim());

/// A QR payload we can act on: either a pre-authorized invite or a short code.
bool isScannablePairing(String raw) =>
    isNightdropInvite(raw) || looksLikeShortCode(raw);

/// Camera QR scanner. Pops with the scanned payload string. By default it only accepts a valid
/// Night Drop invite (pairing); pass [raw] to accept **any** QR text (used by verify-by-QR, whose
/// payload is a safety fingerprint, not a pairing URI).
class ScanScreen extends StatefulWidget {
  const ScanScreen({super.key, this.raw = false, this.title});

  /// When true, pop with the first non-empty QR text without the pairing-payload filter.
  final bool raw;

  /// App-bar title. Null falls back to the localized "Scan invite QR" (see [build]).
  final String? title;

  @override
  State<ScanScreen> createState() => _ScanScreenState();
}

/// If the camera hasn't delivered a single preview frame within this long after the reader mounts,
/// we treat it as a stalled start (the CameraX device was contended and died right after opening —
/// see the 2026-07-13 logcat capture where the camera went OPEN→ACTIVE→CLOSED in ~1ms yet
/// `onControllerCreated` still reported success). We surface a retry rather than a black preview.
const _cameraStallTimeout = Duration(seconds: 7);

class _ScanScreenState extends State<ScanScreen> {
  bool _handled = false;

  /// Camera runtime-permission status. `null` while the first request is in flight. We gate the
  /// reader on this so a fresh install gets an explicit prompt (and a clear "denied" path) instead
  /// of the camera plugin failing into a black preview when access was refused.
  PermissionStatus? _perm;

  /// A valid payload was decoded; show the "connecting" overlay while we hand it back so the scan
  /// registers visibly instead of the screen just vanishing.
  bool _detected = false;

  /// The camera failed to start (init exception) or stalled (no frames). Shows the retry UI.
  Object? _cameraError;

  /// Bumped on retry so the [ReaderWidget] gets a fresh key and fully re-mounts (dispose + re-init
  /// the camera), which is what recovers a transient device-contention failure in place.
  int _readerEpoch = 0;

  /// Frame heartbeat: the reader fires onScan/onScanFailure once per processed frame, so a fresh
  /// timestamp here means the camera stream is genuinely alive.
  DateTime _lastFrameAt = DateTime.now();
  Timer? _watchdog;

  @override
  void initState() {
    super.initState();
    if (canScanQr) _requestPermission();
  }

  @override
  void dispose() {
    _watchdog?.cancel();
    super.dispose();
  }

  /// Ask for camera access (or re-check after the user returns from Settings). Surfaces the result
  /// through [_perm] so [build] can show the reader, a re-ask, or an open-settings path.
  Future<void> _requestPermission() async {
    final status = await Permission.camera.request();
    if (mounted) setState(() => _perm = status);
  }

  void _startWatchdog() {
    _watchdog?.cancel();
    _lastFrameAt = DateTime.now();
    if (!canScanQr) return;
    _watchdog = Timer.periodic(const Duration(seconds: 2), (_) {
      if (!mounted || _handled || _cameraError != null) return;
      if (DateTime.now().difference(_lastFrameAt) > _cameraStallTimeout) {
        setState(() => _cameraError = 'The camera didn’t start delivering a preview.');
      }
    });
  }

  void _onFrame() {
    _lastFrameAt = DateTime.now();
  }

  void _onScan(Code code) {
    _onFrame();
    if (_handled) return;
    final text = code.text?.trim();
    if (text == null || text.isEmpty) return;
    if (widget.raw || isScannablePairing(text)) {
      _handled = true;
      _watchdog?.cancel();
      setState(() => _detected = true);
      // Let the "connecting" overlay paint one frame before we pop, so the successful read is
      // visibly acknowledged rather than the camera just disappearing.
      Future<void>.delayed(const Duration(milliseconds: 220), () {
        if (mounted) Navigator.of(context).pop(text);
      });
    }
  }

  void _retry() {
    setState(() {
      _cameraError = null;
      _readerEpoch++;
    });
    _startWatchdog();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    final title = widget.title ?? l10n.scanInviteQr;
    // No camera backend (desktop): don't show a blank camera that silently never scans — tell the
    // user why and point them at the paste-the-link path.
    if (!canScanQr) {
      return Scaffold(
        appBar: AppBar(title: Text(title)),
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: Text(
              l10n.cameraUnavailable,
              textAlign: TextAlign.center,
            ),
          ),
        ),
      );
    }
    // Still asking for camera access on first entry.
    if (_perm == null) {
      return Scaffold(
        appBar: AppBar(title: Text(title)),
        body: const Center(child: CircularProgressIndicator()),
      );
    }
    // Access refused. A plain deny can be re-asked in-app; a permanent deny / restriction must be
    // changed in system Settings, so we route to the right action rather than a dead black preview.
    if (!_perm!.isGranted) {
      final permanent = _perm!.isPermanentlyDenied || _perm!.isRestricted;
      return Scaffold(
        appBar: AppBar(title: Text(title)),
        body: _CameraPermissionView(
          permanentlyDenied: permanent,
          onPrimary: permanent
              ? () async {
                  await openAppSettings();
                  // Re-check when they come back; if they enabled it, the reader appears.
                  await _requestPermission();
                }
              : _requestPermission,
        ),
      );
    }
    return Scaffold(
      appBar: AppBar(title: Text(title)),
      body: Stack(
        children: [
          if (_cameraError != null)
            _CameraErrorView(onRetry: _retry)
          else ...[
            // ZXing (Apache-2.0, on-device, no Google Play Services) replaces ML Kit here so
            // the pre-authorized pairing scan pulls in no Google runtime — see §5a / privacy note.
            // A per-epoch key forces a full re-mount on retry so a stalled camera restarts cleanly.
            ReaderWidget(
              key: ValueKey<int>(_readerEpoch),
              onScan: _onScan,
              onScanFailure: (_) => _onFrame(), // heartbeat: frames are flowing.
              onControllerCreated: (controller, error) {
                if (!mounted) return;
                if (error != null) {
                  setState(() => _cameraError = error);
                } else {
                  _startWatchdog();
                }
              },
              codeFormat: Format.qrCode, // QR-only: faster per-frame decode.
              tryHarder: true,
              tryInverted: true,
              tryDownscale: true, // helps when a dense invite QR fills much of the frame.
              // The invite QR is dense (full .onion + identity key + one-time prekey), so give the
              // decoder the best shot: a higher-resolution stream, a wider scan region than the
              // default centre-50% (so the user doesn't have to frame it perfectly), and — most
              // importantly — a short inter-attempt delay. The default 1s delay means only ~1
              // decode try per second; 150ms gives ~6×, which is what actually locks a dense code.
              resolution: ResolutionPreset.veryHigh,
              cropPercent: 0.8,
              scanDelay: const Duration(milliseconds: 150),
              showGallery: false, // camera-only; no photo-library access for pairing.
            ),
            if (_detected)
              _ProcessingOverlay(label: l10n.qrDetectedConnecting)
            else
              Align(
                alignment: Alignment.bottomCenter,
                child: Padding(
                  padding: const EdgeInsets.all(24),
                  child: Text(
                    l10n.pointCameraHint,
                    textAlign: TextAlign.center,
                    style: const TextStyle(
                        color: Colors.white, backgroundColor: Colors.black54),
                  ),
                ),
              ),
          ],
        ],
      ),
    );
  }
}

/// Shown when the camera never started (transient device contention). Offers an in-place retry that
/// re-mounts the reader, plus the paste-the-link fallback that always works.
class _CameraErrorView extends StatelessWidget {
  const _CameraErrorView({required this.onRetry});

  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.videocam_off, size: 48),
            const SizedBox(height: 16),
            Text(
              l10n.cameraDidntStart,
              textAlign: TextAlign.center,
            ),
            const SizedBox(height: 24),
            FilledButton.icon(
              onPressed: onRetry,
              icon: const Icon(Icons.refresh),
              label: Text(l10n.retryCamera),
            ),
            const SizedBox(height: 8),
            Text(
              l10n.cameraErrorFallback,
              textAlign: TextAlign.center,
              style: const TextStyle(fontSize: 12),
            ),
          ],
        ),
      ),
    );
  }
}

/// Shown when camera access is refused. For a plain deny we re-ask in-app; for a permanent deny /
/// restriction we send the user to system Settings (they can't be re-prompted otherwise). Either
/// way the paste-the-link fallback is offered so pairing is never blocked on the camera.
class _CameraPermissionView extends StatelessWidget {
  const _CameraPermissionView({
    required this.permanentlyDenied,
    required this.onPrimary,
  });

  final bool permanentlyDenied;
  final VoidCallback onPrimary;

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.no_photography, size: 48),
            const SizedBox(height: 16),
            Text(
              permanentlyDenied
                  ? l10n.cameraPermissionPermanent
                  : l10n.cameraPermissionNeeded,
              textAlign: TextAlign.center,
            ),
            const SizedBox(height: 24),
            FilledButton.icon(
              onPressed: onPrimary,
              icon: Icon(permanentlyDenied ? Icons.settings : Icons.photo_camera),
              label: Text(permanentlyDenied ? l10n.openSettings : l10n.allowCamera),
            ),
            const SizedBox(height: 8),
            Text(
              l10n.cameraPermissionFallback,
              textAlign: TextAlign.center,
              style: const TextStyle(fontSize: 12),
            ),
          ],
        ),
      ),
    );
  }
}

/// Full-screen "working" overlay shown the instant a valid QR is decoded, so the successful read is
/// acknowledged before the scanner hands the payload back to the join flow.
class _ProcessingOverlay extends StatelessWidget {
  const _ProcessingOverlay({required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    return ColoredBox(
      color: Colors.black54,
      child: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const CircularProgressIndicator(color: Colors.white),
            const SizedBox(height: 20),
            Text(
              label,
              textAlign: TextAlign.center,
              style: const TextStyle(color: Colors.white, fontSize: 16),
            ),
          ],
        ),
      ),
    );
  }
}
