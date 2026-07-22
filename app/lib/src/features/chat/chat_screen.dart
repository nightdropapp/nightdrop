import 'dart:io';

import 'package:fc_native_video_thumbnail/fc_native_video_thumbnail.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:image/image.dart' as img;
import 'package:open_filex/open_filex.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/nightdrop_core.dart';
import '../../core/media_cache.dart';
import '../../core/models.dart';
import '../backup/backup_actions.dart';
import 'verify_screen.dart';

/// Downscale + recompress an image to JPEG so it's small enough to move over Tor quickly.
/// Runs in a background isolate (via [compute]). Returns the original bytes on failure.
Uint8List compressImage(Uint8List input) {
  try {
    final decoded = img.decodeImage(input);
    if (decoded == null) return input;
    const maxDim = 1600;
    final scaled = (decoded.width > maxDim || decoded.height > maxDim)
        ? img.copyResize(
            decoded,
            width: decoded.width >= decoded.height ? maxDim : null,
            height: decoded.height > decoded.width ? maxDim : null,
          )
        : decoded;
    final jpg = img.encodeJpg(scaled, quality: 82);
    // Only use the recompressed version if it's actually smaller.
    return jpg.length < input.length ? Uint8List.fromList(jpg) : input;
  } catch (_) {
    return input;
  }
}

/// Largest attachment the core accepts (must match `MAX_MEDIA_BYTES` in the Rust core).
const int kMaxMediaBytes = 100 * 1024 * 1024;

const Set<String> _videoExts = {
  'mp4',
  'mov',
  'webm',
  'mkv',
  'avi',
  'm4v',
  '3gp'
};

String _mimeFor(String ext, bool isVideo) {
  const map = {
    'jpg': 'image/jpeg',
    'jpeg': 'image/jpeg',
    'png': 'image/png',
    'gif': 'image/gif',
    'webp': 'image/webp',
    'heic': 'image/heic',
    'mp4': 'video/mp4',
    'mov': 'video/quicktime',
    'webm': 'video/webm',
    'mkv': 'video/x-matroska',
    'avi': 'video/x-msvideo',
    'm4v': 'video/x-m4v',
    '3gp': 'video/3gpp',
  };
  return map[ext] ?? (isVideo ? 'video/octet-stream' : 'image/png');
}

/// Human-readable byte size, e.g. "3.2 MB".
String formatBytes(int bytes) {
  if (bytes < 1024) return '$bytes B';
  const units = ['KB', 'MB', 'GB'];
  var size = bytes / 1024;
  var unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit++;
  }
  return '${size.toStringAsFixed(size >= 10 ? 0 : 1)} ${units[unit]}';
}

/// A 1:1 conversation. Shows the remote-storage warning banner when server storage is
/// on, lets you rename yourself in this chat, and toggle 24h server storage.
class ChatScreen extends StatefulWidget {
  const ChatScreen({super.key, required this.contactId});

  final String contactId;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  /// Tracks the message count so we can auto-scroll to the bottom on any new message
  /// (sent or received), and on first open.
  int _lastCount = 0;
  final _input = TextEditingController();
  final _scroll = ScrollController();

  @override
  void dispose() {
    _input.dispose();
    _scroll.dispose();
    super.dispose();
  }

  Contact? _contact(NightdropCore core) {
    for (final c in core.contacts) {
      if (c.id == widget.contactId) return c;
    }
    return null; // gone (deleted locally or never existed) — build pops out
  }

  Future<void> _send() async {
    final l10n = AppLocalizations.of(context)!;
    final text = _input.text;
    if (text.trim().isEmpty) return;
    try {
      await NightdropScope.of(context).sendMessage(widget.contactId, text);
      // Only clear the draft once the core has accepted it — a failed send keeps your text.
      _input.clear();
      _scrollToEnd();
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(_sendErrorText(l10n, e))),
      );
    }
  }

  /// A friendly, non-technical explanation for a failed send (the only send error the core
  /// raises is "offline and no relay accepted the message"). The draft is preserved on failure.
  String _sendErrorText(AppLocalizations l10n, Object e) {
    final msg = cleanCoreError(e);
    if (msg.contains('no relay accepted')) {
      return l10n.couldntSendOffline;
    }
    return l10n.couldntSend(msg);
  }

  /// Pick an image or video from the device and send it.
  Future<void> _attachMedia() async {
    final l10n = AppLocalizations.of(context)!;
    final result = await FilePicker.pickFiles(
      type:
          FileType.media, // images + videos; we read large files from the path
    );
    final picked = result?.files.single;
    final path = picked?.path;
    if (path == null) return;

    final size = picked!.size;
    if (size > kMaxMediaBytes) {
      _toast(l10n.fileTooLarge(formatBytes(size), formatBytes(kMaxMediaBytes)));
      return;
    }
    final ext = (picked.extension ?? '').toLowerCase();
    final isVideo = _videoExts.contains(ext);
    var bytes = await File(path).readAsBytes();
    var mime = _mimeFor(ext, isVideo);
    var thumb = <int>[];
    if (isVideo) {
      thumb = await _videoThumbnail(path);
    } else if (ext != 'gif') {
      // Images (except animated GIFs): downscale/recompress to JPEG off the UI thread so the
      // transfer over Tor is fast. GIFs are left alone to preserve animation.
      bytes = await compute(compressImage, bytes);
      mime = 'image/jpeg';
    }
    await _sendMedia(bytes, mime, isVideo ? 'video' : 'image', thumb);
  }

  /// Extract a small preview frame from a video (all desktop + mobile platforms; on
  /// Linux this uses ffmpeg via the plugin). Any failure just means no thumbnail —
  /// the receiver sees the generic video tile instead.
  Future<List<int>> _videoThumbnail(String path) async {
    try {
      final data = await FcNativeVideoThumbnail().saveThumbnailToBytes(
        srcFile: path,
        width: 320,
        height: 320,
        format: 'jpeg',
        quality: 60,
      );
      return data ?? const [];
    } catch (_) {
      return const [];
    }
  }

  /// Paste text from the clipboard into the composer at the cursor. (Image paste was dropped
  /// with the `pasteboard` plugin — the last KGP-warning dependency; send a picture with the
  /// attach button instead, which also compresses it for Tor.)
  Future<void> _paste() async {
    final l10n = AppLocalizations.of(context)!;
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    if (text != null && text.isNotEmpty) {
      final sel = _input.selection;
      final base = _input.text;
      if (sel.isValid) {
        _input.text = base.replaceRange(sel.start, sel.end, text);
      } else {
        _input.text = base + text;
      }
    } else {
      _toast(l10n.nothingToPaste);
    }
  }

  Future<void> _sendMedia(
      List<int> bytes, String mime, String kind, List<int> thumb) async {
    final l10n = AppLocalizations.of(context)!;
    try {
      await NightdropScope.of(context)
          .sendMedia(widget.contactId, bytes, mime, kind, thumb);
      _scrollToEnd();
    } catch (e) {
      _toast(l10n.couldNotSendAttachment(e.toString()));
    }
  }

  void _toast(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text(message)));
  }

  Future<void> _confirmDelete(Contact contact) async {
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
    if (confirmed != true || !mounted) return;
    await NightdropScope.of(context).deleteChat(widget.contactId);
    if (mounted) Navigator.of(context).pop(); // back to the chat list
  }

  void _scrollToEnd() {
    void toBottom({bool animate = false}) {
      if (!_scroll.hasClients) return;
      final target = _scroll.position.maxScrollExtent;
      if (animate) {
        _scroll.animateTo(target,
            duration: const Duration(milliseconds: 200), curve: Curves.easeOut);
      } else {
        _scroll.jumpTo(target);
      }
    }

    // Scroll after layout, then settle again shortly after — tall items (images) finish
    // laying out asynchronously, so a single pass lands at the top of the new message.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      toBottom(animate: true);
      Future.delayed(const Duration(milliseconds: 350), () => toBottom());
      Future.delayed(const Duration(milliseconds: 800), () => toBottom());
    });
  }

  /// Edit one of our own messages (long-press on an eligible bubble). The core enforces
  /// the same rule as [Message.canEdit]; errors (e.g. window just closed) surface as a
  /// snackbar.
  Future<void> _editMessage(Message message) async {
    final l10n = AppLocalizations.of(context)!;
    final controller = TextEditingController(text: message.text);
    final newText = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: Text(l10n.editMessageTitle),
        content: TextField(
          controller: controller,
          autofocus: true,
          minLines: 1,
          maxLines: 5,
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: Text(l10n.cancel),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text),
            child: Text(l10n.save),
          ),
        ],
      ),
    );
    if (newText == null || !mounted) return;
    final trimmed = newText.trim();
    if (trimmed.isEmpty || trimmed == message.text) return;
    try {
      await NightdropScope.of(context)
          .editMessage(widget.contactId, message.msgId, trimmed);
    } catch (e) {
      _toast(l10n.couldNotEdit(e.toString()));
    }
  }

  /// Long-press menu for one of our own eligible messages: edit or unsend.
  Future<void> _showMessageMenu(Message message) async {
    final l10n = AppLocalizations.of(context)!;
    final action = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.edit_outlined),
              title: Text(l10n.edit),
              onTap: () => Navigator.pop(context, 'edit'),
            ),
            ListTile(
              leading: const Icon(Icons.delete_outline),
              title: Text(l10n.deleteForEveryone),
              onTap: () => Navigator.pop(context, 'unsend'),
            ),
          ],
        ),
      ),
    );
    if (!mounted || action == null) return;
    if (action == 'edit') {
      await _editMessage(message);
    } else if (action == 'unsend') {
      await _unsendMessage(message);
    }
  }

  /// Unsend ("delete for both"). Confirms first, then asks the core to tombstone the message
  /// on both devices (recalling it from the relay if it hasn't been delivered yet).
  Future<void> _unsendMessage(Message message) async {
    final l10n = AppLocalizations.of(context)!;
    final confirm = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.deleteForEveryoneTitle),
        content: Text(l10n.unsendBody),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.cancel),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: Text(l10n.delete),
          ),
        ],
      ),
    );
    if (confirm != true || !mounted) return;
    try {
      await NightdropScope.of(context)
          .unsendMessage(widget.contactId, message.msgId);
    } catch (e) {
      _toast(l10n.couldNotDelete(e.toString()));
    }
  }

  Future<void> _renameSelf(Contact contact) async {
    final l10n = AppLocalizations.of(context)!;
    final controller = TextEditingController(text: contact.myName);
    final name = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: Text(l10n.yourNameInChat),
        content: TextField(
          controller: controller,
          autofocus: true,
          decoration: const InputDecoration(hintText: kDefaultName),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: Text(l10n.cancel),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text),
            child: Text(l10n.save),
          ),
        ],
      ),
    );
    if (name != null && mounted) {
      NightdropScope.of(context).setMyNameInChat(widget.contactId, name);
    }
  }

  /// Pick this chat's disappearing-messages timer. The choice is synced to the peer (both
  /// devices then delete messages older than the timer).
  Future<void> _pickDisappearing(Contact contact) async {
    final l10n = AppLocalizations.of(context)!;
    final options = <String, int>{
      l10n.disappearingOff: 0,
      l10n.disappearing1Hour: 3600,
      l10n.disappearing1Day: 86400,
      l10n.disappearing1Week: 604800,
    };
    final secs = await showModalBottomSheet<int>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              title: Text(l10n.disappearingMessages),
              subtitle: Text(l10n.disappearingSubtitle),
            ),
            for (final e in options.entries)
              ListTile(
                title: Text(e.key),
                trailing: contact.disappearingSecs == e.value
                    ? const Icon(Icons.check)
                    : null,
                onTap: () => Navigator.pop(context, e.value),
              ),
          ],
        ),
      ),
    );
    if (secs == null || !mounted || secs == contact.disappearingSecs) return;
    try {
      await NightdropScope.of(context).setDisappearing(widget.contactId, secs);
    } catch (e) {
      _toast(l10n.couldNotSetTimer(e.toString()));
    }
  }

  @override
  Widget build(BuildContext context) {
    final core = NightdropScope.of(context);
    final l10n = AppLocalizations.of(context)!;
    return ListenableBuilder(
      listenable: core,
      builder: (context, _) {
        final contact = _contact(core);
        if (contact == null) {
          // The chat was deleted while open — leave the screen on the next frame.
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted) Navigator.of(context).maybePop();
          });
          return const Scaffold(body: SizedBox.shrink());
        }
        final messages = core.messagesFor(widget.contactId);
        // Joiner-side short-code pairing: the core leaves an `await_approval` system notice until
        // the code provider accepts (or the first message arrives). Surface it as a persistent
        // banner rather than a scrollable line, and hide the raw notice from the list.
        final awaitingApproval =
            messages.any((m) => m.system && m.kind == 'await_approval');
        final visibleMessages = awaitingApproval
            ? messages
                .where((m) => !(m.system && m.kind == 'await_approval'))
                .toList()
            : messages;
        // Viewing the chat clears its unread badge (post-frame to avoid notifying mid-build).
        if (core.unreadCount(widget.contactId) > 0) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted) core.markRead(widget.contactId);
          });
        }
        // Auto-scroll to the newest message whenever the count changes (new send/receive
        // or first open).
        if (messages.length != _lastCount) {
          _lastCount = messages.length;
          _scrollToEnd();
        }
        return Scaffold(
          appBar: AppBar(
            title: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Flexible(child: Text(contact.theirName)),
                    if (contact.verified) ...[
                      const SizedBox(width: 6),
                      Icon(Icons.verified_user,
                          semanticLabel: l10n.verified,
                          size: 16,
                          color: Theme.of(context).colorScheme.primary),
                    ],
                  ],
                ),
                Text(
                  shortId(contact.id),
                  style: const TextStyle(
                    fontSize: 11,
                    fontFamily: 'monospace',
                    fontWeight: FontWeight.normal,
                  ),
                ),
              ],
            ),
            actions: [
              IconButton(
                tooltip: l10n.verifySafetyNumber,
                icon: Icon(contact.verified
                    ? Icons.verified_user
                    : Icons.shield_outlined),
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => VerifyScreen(
                      contactId: contact.id,
                      name: contact.theirName,
                    ),
                  ),
                ),
              ),
              IconButton(
                tooltip: l10n.renameYourselfTooltip,
                icon: const Icon(Icons.badge_outlined),
                onPressed: () => _renameSelf(contact),
              ),
              IconButton(
                tooltip: contact.remoteStorage
                    ? l10n.storedServerTooltipOn
                    : l10n.storedServerTooltipOff,
                icon: Icon(
                  contact.remoteStorage ? Icons.cloud : Icons.cloud_off,
                ),
                onPressed: () => core.setRemoteStorage(
                  widget.contactId,
                  !contact.remoteStorage,
                ),
              ),
              IconButton(
                tooltip: contact.disappearingSecs > 0
                    ? l10n.disappearingTooltipOn(
                        _disappearingLabel(contact.disappearingSecs))
                    : l10n.disappearingTooltipOff,
                icon: Icon(contact.disappearingSecs > 0
                    ? Icons.timer
                    : Icons.timer_off_outlined),
                onPressed: () => _pickDisappearing(contact),
              ),
              IconButton(
                tooltip: l10n.deleteThisChatTooltip,
                icon: const Icon(Icons.delete_outline),
                onPressed: () => _confirmDelete(contact),
              ),
              PopupMenuButton<String>(
                tooltip: l10n.more,
                onSelected: (v) {
                  if (v == 'backup') {
                    createAndSaveBackup(context, core,
                        contactId: widget.contactId);
                  }
                },
                itemBuilder: (context) => [
                  PopupMenuItem(
                      value: 'backup', child: Text(l10n.backUpThisChat)),
                ],
              ),
            ],
          ),
          body: Column(
            children: [
              if (awaitingApproval) const _AwaitingApprovalBanner(),
              // Nudge toward safety-number verification once the chat is live and still unverified.
              // Tapping opens the same VerifyScreen as the app-bar shield. Suppressed while awaiting
              // approval (nothing to verify yet).
              if (!awaitingApproval && !contact.verified)
                _UnverifiedBanner(
                  onVerify: () => Navigator.of(context).push(
                    MaterialPageRoute<void>(
                      builder: (_) => VerifyScreen(
                        contactId: contact.id,
                        name: contact.theirName,
                      ),
                    ),
                  ),
                ),
              if (contact.remoteStorage)
                _RemoteStorageBanner(healthy: contact.remoteStorageHealthy),
              if (contact.peerBackedUp) const _PeerBackupBanner(),
              Expanded(
                child: visibleMessages.isEmpty
                    ? Center(child: Text(l10n.sayHi))
                    : ListView.builder(
                        controller: _scroll,
                        padding: const EdgeInsets.all(12),
                        itemCount: visibleMessages.length,
                        itemBuilder: (context, i) {
                          final m = visibleMessages[i];
                          final row = m.system
                              ? _SystemNotice(text: m.text)
                              : _Bubble(
                                  message: m,
                                  senderName: m.fromMe
                                      ? contact.myName
                                      : contact.theirName,
                                  // Right-click (desktop) or long-press (mobile) own recent/
                                  // queued text to edit or unsend it.
                                  onLongPress: m.canEdit
                                      ? () => _showMessageMenu(m)
                                      : null,
                                );
                          // A day separator above the first message of each calendar day
                          // (skipping messages with no real timestamp — pre-timestamp history).
                          final prev = i > 0 ? visibleMessages[i - 1] : null;
                          final showDay = _hasTime(m.at) &&
                              (prev == null || !_sameDay(prev.at, m.at));
                          if (!showDay) return row;
                          return Column(
                            crossAxisAlignment: CrossAxisAlignment.stretch,
                            children: [_DaySeparator(day: m.at), row],
                          );
                        },
                      ),
              ),
              _Composer(
                controller: _input,
                onSend: _send,
                onAttach: _attachMedia,
                onPaste: _paste,
              ),
            ],
          ),
        );
      },
    );
  }
}

/// Persistent warning shown to BOTH parties while server storage is active (§6). When
/// [healthy] is false the last send couldn't reach a relay to store its copy, so the banner
/// switches to an error tone and says recent messages weren't stored server-side.
class _RemoteStorageBanner extends StatelessWidget {
  const _RemoteStorageBanner({this.healthy = true});

  final bool healthy;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final bg = healthy ? scheme.tertiaryContainer : scheme.errorContainer;
    final fg = healthy ? scheme.onTertiaryContainer : scheme.onErrorContainer;
    return Container(
      width: double.infinity,
      color: bg,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
      child: Row(
        children: [
          Icon(healthy ? Icons.cloud : Icons.cloud_off, size: 18, color: fg),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              healthy
                  ? AppLocalizations.of(context)!.remoteStorageBannerHealthy
                  : AppLocalizations.of(context)!.remoteStorageBannerUnhealthy,
              style: TextStyle(color: fg, fontSize: 12.5),
            ),
          ),
        ],
      ),
    );
  }
}

/// Persistent transparency warning that the peer keeps a Full backup of this chat (#7), so
/// messages sent here may persist in the other person's backup. Mirrors the server-storage
/// banner — a signal the invariant treats like remote storage.
/// Subtle, tappable nudge shown on an unverified chat. Verification (comparing the safety number
/// out-of-band) is what actually rules out a MITM on pairing, but it's easy to skip — so we keep a
/// low-key reminder in front of the user until they verify. Tap → [VerifyScreen].
class _UnverifiedBanner extends StatelessWidget {
  const _UnverifiedBanner({required this.onVerify});

  final VoidCallback onVerify;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.surfaceContainerHighest,
      child: InkWell(
        onTap: onVerify,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 9),
          child: Row(
            children: [
              Icon(Icons.shield_outlined,
                  size: 18, color: scheme.onSurfaceVariant),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  AppLocalizations.of(context)!.unverifiedBannerBody,
                  style:
                      TextStyle(color: scheme.onSurfaceVariant, fontSize: 12.5),
                ),
              ),
              const SizedBox(width: 8),
              Text(
                AppLocalizations.of(context)!.verify,
                style: TextStyle(
                  color: scheme.primary,
                  fontSize: 12.5,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _PeerBackupBanner extends StatelessWidget {
  const _PeerBackupBanner();

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      color: scheme.secondaryContainer,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
      child: Row(
        children: [
          Icon(Icons.inventory_2_outlined,
              size: 18, color: scheme.onSecondaryContainer),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              AppLocalizations.of(context)!.peerBackupBanner,
              style:
                  TextStyle(color: scheme.onSecondaryContainer, fontSize: 12.5),
            ),
          ),
        ],
      ),
    );
  }
}

/// Persistent status shown to the **joiner** after short-code pairing, until the code provider
/// accepts the chat (§5). Driven by the core's `await_approval` notice, which it clears on the
/// approval signal or the first received message — so this banner disappears exactly when the
/// chat goes live. Messages typed meanwhile still send (queued) but aren't delivered until then.
class _AwaitingApprovalBanner extends StatelessWidget {
  const _AwaitingApprovalBanner();

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      color: scheme.tertiaryContainer,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
      child: Row(
        children: [
          SizedBox(
            width: 16,
            height: 16,
            child: CircularProgressIndicator(
              strokeWidth: 2,
              color: scheme.onTertiaryContainer,
            ),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: Text(
              AppLocalizations.of(context)!.awaitingApprovalBanner,
              style:
                  TextStyle(color: scheme.onTertiaryContainer, fontSize: 12.5),
            ),
          ),
        ],
      ),
    );
  }
}

/// A centered, unobtrusive system notice (chat deleted / approved / code reused).
class _SystemNotice extends StatelessWidget {
  const _SystemNotice({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      margin: const EdgeInsets.symmetric(vertical: 8, horizontal: 24),
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Text(
        text,
        textAlign: TextAlign.center,
        style: TextStyle(
          color: scheme.onSurfaceVariant,
          fontSize: 12.5,
          fontStyle: FontStyle.italic,
        ),
      ),
    );
  }
}

class _Bubble extends StatelessWidget {
  const _Bubble(
      {required this.message, required this.senderName, this.onLongPress});

  final Message message;
  final String senderName;

  /// Non-null when this message has long-press actions (own text, within the window or
  /// still queued) — opens the edit / delete menu.
  final VoidCallback? onLongPress;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final l10n = AppLocalizations.of(context)!;
    final mine = message.fromMe;
    return Align(
      alignment: mine ? Alignment.centerRight : Alignment.centerLeft,
      // Faded while the message is still being sent (not delivered yet).
      child: GestureDetector(
        onLongPress: onLongPress,
        child: Opacity(
          opacity: message.sending ? 0.55 : 1,
          child: Container(
            margin: const EdgeInsets.symmetric(vertical: 4),
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
            constraints: const BoxConstraints(maxWidth: 320),
            decoration: BoxDecoration(
              color: mine ? scheme.primary : scheme.surfaceContainerHighest,
              borderRadius: BorderRadius.circular(16),
            ),
            child: Column(
              crossAxisAlignment:
                  mine ? CrossAxisAlignment.end : CrossAxisAlignment.start,
              children: [
                Text(
                  senderName,
                  style: TextStyle(
                    fontSize: 11,
                    color: (mine ? scheme.onPrimary : scheme.onSurfaceVariant)
                        .withValues(alpha: 0.7),
                  ),
                ),
                const SizedBox(height: 2),
                if (message.isDeleted)
                  Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(Icons.block,
                          size: 13,
                          color: (mine ? scheme.onPrimary : scheme.onSurface)
                              .withValues(alpha: 0.6)),
                      const SizedBox(width: 5),
                      Text(
                        l10n.messageDeleted,
                        style: TextStyle(
                          fontStyle: FontStyle.italic,
                          color: (mine ? scheme.onPrimary : scheme.onSurface)
                              .withValues(alpha: 0.6),
                        ),
                      ),
                    ],
                  )
                else if (message.isText)
                  Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Flexible(
                        child: Text(
                          message.text,
                          style: TextStyle(
                              color:
                                  mine ? scheme.onPrimary : scheme.onSurface),
                        ),
                      ),
                      if (message.sending) ...[
                        const SizedBox(width: 6),
                        Icon(Icons.schedule,
                            size: 12,
                            color: (mine ? scheme.onPrimary : scheme.onSurface)
                                .withValues(alpha: 0.7)),
                      ],
                    ],
                  )
                else
                  _MediaContent(message: message, mine: mine),
                // "edited" tag once a sender edit replaced the text.
                if (message.edited) ...[
                  const SizedBox(height: 2),
                  Text(
                    l10n.editedTag,
                    style: TextStyle(
                      fontSize: 10,
                      fontStyle: FontStyle.italic,
                      color: (mine ? scheme.onPrimary : scheme.onSurfaceVariant)
                          .withValues(alpha: 0.7),
                    ),
                  ),
                ],
                // Sender delivery status: held on the relay, delivered, or expired unread.
                if (mine &&
                    !message.sending &&
                    (message.delivery == 'queued' ||
                        message.delivery == 'delivered' ||
                        message.delivery == 'expired')) ...[
                  const SizedBox(height: 3),
                  Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(
                        switch (message.delivery) {
                          'queued' => Icons.cloud_upload_outlined,
                          'expired' => Icons.error_outline,
                          _ => Icons.done_all,
                        },
                        size: 12,
                        color: scheme.onPrimary.withValues(alpha: 0.75),
                      ),
                      const SizedBox(width: 4),
                      Text(
                        switch (message.delivery) {
                          'queued' => l10n.deliveryHeld,
                          'expired' => l10n.deliveryExpired,
                          _ => l10n.deliveryDelivered,
                        },
                        style: TextStyle(
                            fontSize: 10.5,
                            color: scheme.onPrimary.withValues(alpha: 0.75)),
                      ),
                    ],
                  ),
                ],
                // Message time (local clock). Omitted for pre-timestamp history (at == 0).
                if (_hasTime(message.at)) ...[
                  const SizedBox(height: 2),
                  Text(
                    _formatTime(message.at),
                    style: TextStyle(
                      fontSize: 10,
                      color: (mine ? scheme.onPrimary : scheme.onSurfaceVariant)
                          .withValues(alpha: 0.6),
                    ),
                  ),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// A centered date chip separating messages from different calendar days.
class _DaySeparator extends StatelessWidget {
  const _DaySeparator({required this.day});

  final DateTime day;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Center(
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 10),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
        decoration: BoxDecoration(
          color: scheme.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(12),
        ),
        child: Text(
          _dayLabel(day),
          style: TextStyle(
            fontSize: 11.5,
            color: scheme.onSurfaceVariant,
            fontWeight: FontWeight.w500,
          ),
        ),
      ),
    );
  }
}

/// Messages persisted before timestamps existed carry `at == 0` (epoch); treat those as
/// having no known time so we don't render a "1970" label.
bool _hasTime(DateTime at) => at.toUtc().year > 2000;

bool _sameDay(DateTime a, DateTime b) {
  final la = a.toLocal();
  final lb = b.toLocal();
  return la.year == lb.year && la.month == lb.month && la.day == lb.day;
}

/// A compact label for a disappearing-messages timer, e.g. "1h", "1d", "1w".
String _disappearingLabel(int secs) {
  if (secs <= 0) return 'off';
  if (secs % 604800 == 0) return '${secs ~/ 604800}w';
  if (secs % 86400 == 0) return '${secs ~/ 86400}d';
  if (secs % 3600 == 0) return '${secs ~/ 3600}h';
  if (secs % 60 == 0) return '${secs ~/ 60}m';
  return '${secs}s';
}

/// A 12-hour local clock time, e.g. "3:45 PM" (no `intl` dependency).
String _formatTime(DateTime at) {
  final t = at.toLocal();
  final hour12 = t.hour % 12 == 0 ? 12 : t.hour % 12;
  final minute = t.minute.toString().padLeft(2, '0');
  return '$hour12:$minute ${t.hour < 12 ? 'AM' : 'PM'}';
}

/// "Today" / "Yesterday" / "Jul 8" (adding the year only when it differs from now).
String _dayLabel(DateTime at) {
  const months = [
    'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
    'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec' //
  ];
  final now = DateTime.now();
  final t = at.toLocal();
  final day = DateTime(t.year, t.month, t.day);
  final today = DateTime(now.year, now.month, now.day);
  final diff = today.difference(day).inDays;
  if (diff == 0) return 'Today';
  if (diff == 1) return 'Yesterday';
  final year = t.year == now.year ? '' : ', ${t.year}';
  return '${months[t.month - 1]} ${t.day}$year';
}

/// Renders an attachment inside a bubble: images show as a fixed cropped thumbnail (tap to
/// expand fullscreen); videos/other files show a tappable tile that opens in the system
/// player. While uploading, an optimistic preview is shown with a spinner overlay.
class _MediaContent extends StatelessWidget {
  const _MediaContent({required this.message, required this.mine});

  final Message message;
  final bool mine;

  static const double _thumbW = 220;
  static const double _thumbH = 160;

  // Decrypted bytes/temp-file paths are memoized in the process-wide [MediaCache] so a
  // rebuild (e.g. sending another image) doesn't re-fetch every image and flash its
  // spinner. The cache is wiped on logout so no plaintext outlives the identity.

  /// Bytes for display: the optimistic local copy if present, else decrypted from the core
  /// (memoized by media id). Captures the core, not the BuildContext, so the cached future
  /// stays valid across rebuilds.
  Future<Uint8List> _bytes(BuildContext context) {
    final local = message.localBytes;
    if (local != null) return Future.value(Uint8List.fromList(local));
    final core = NightdropScope.of(context);
    return MediaCache.bytes.putIfAbsent(
      message.mediaId,
      () async => Uint8List.fromList(await core.mediaBytes(message.mediaId)),
    );
  }

  /// Decrypt the attachment to a temp file once; subsequent calls reuse the same future,
  /// so a video pre-warmed when its bubble scrolled into view opens near-instantly on tap.
  Future<String> _fileFuture(BuildContext context) {
    final core = NightdropScope.of(context);
    final ext = message.mime.split('/').last;
    return MediaCache.files.putIfAbsent(
      message.mediaId,
      () => core.mediaToFile(message.mediaId, ext),
    );
  }

  Future<void> _openExternally(BuildContext context) async {
    final l10n = AppLocalizations.of(context)!;
    try {
      final path =
          await _fileFuture(context); // usually already decrypted (pre-warmed)
      final result = await OpenFilex.open(path);
      if (result.type != ResultType.done && context.mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(l10n.couldNotOpenFile(result.message))),
        );
      }
    } catch (e) {
      if (context.mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(content: Text(l10n.couldNotOpen(e.toString()))));
      }
    }
  }

  void _expand(BuildContext context) {
    final bytesFuture = _bytes(context);
    Navigator.of(context).push(MaterialPageRoute<void>(
      builder: (_) => _ImageViewer(
          bytes: bytesFuture, title: formatBytes(message.mediaSize)),
    ));
  }

  /// A video's preview-frame bytes (optimistic local copy, else the sealed thumbnail), or
  /// null if there's no thumbnail to show.
  Future<Uint8List>? _thumbBytes(BuildContext context) {
    final local = message.localBytes;
    if (local != null) return Future.value(Uint8List.fromList(local));
    if (message.thumbId.isEmpty) return null;
    final core = NightdropScope.of(context);
    return MediaCache.bytes.putIfAbsent(
      'thumb:${message.thumbId}',
      () async => Uint8List.fromList(await core.mediaBytes(message.thumbId)),
    );
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final l10n = AppLocalizations.of(context)!;
    final onColor = mine ? scheme.onPrimary : scheme.onSurface;
    final sizeLabel = formatBytes(message.mediaSize);
    final faint = onColor.withValues(alpha: 0.7);

    if (message.isImage) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          ClipRRect(
            borderRadius: BorderRadius.circular(10),
            child: SizedBox(
              width: _thumbW,
              height: _thumbH,
              child: Stack(
                fit: StackFit.expand,
                children: [
                  FutureBuilder<Uint8List>(
                    future: _bytes(context),
                    builder: (context, snap) {
                      if (snap.connectionState != ConnectionState.done) {
                        return const ColoredBox(
                          color: Colors.black26,
                          child: Center(child: CircularProgressIndicator()),
                        );
                      }
                      if (snap.hasError || snap.data == null) {
                        return ColoredBox(
                          color: Colors.black26,
                          child: Center(child: Text(l10n.imageUnavailable)),
                        );
                      }
                      return GestureDetector(
                        onTap: message.sending ? null : () => _expand(context),
                        // Cropped to the thumbnail box so a tall image can't blow up the
                        // bubble. cacheWidth decodes at ~2x the box (crisp on hi-dpi)
                        // instead of rasterizing the full-resolution photo per bubble.
                        child: Image.memory(snap.data!,
                            fit: BoxFit.cover,
                            cacheWidth: (_thumbW * 2).round()),
                      );
                    },
                  ),
                  if (message.sending) const _UploadingOverlay(),
                ],
              ),
            ),
          ),
          const SizedBox(height: 4),
          Text(
            message.sending
                ? l10n.mediaImageSending(sizeLabel)
                : l10n.mediaImage(sizeLabel),
            style: TextStyle(fontSize: 11, color: faint),
          ),
        ],
      );
    }

    // Video. Show its thumbnail (if any) with a play/spinner overlay; while the payload is
    // still in flight (sending or incoming) it can't be opened yet.
    final inFlight = message.sending || message.receiving;
    final arrived = message.mediaId.isNotEmpty;
    final status = message.sending
        ? l10n.mediaStatusSending
        : (message.receiving
            ? l10n.mediaStatusIncoming
            : l10n.mediaStatusTapToPlay);
    final thumbFuture = message.isVideo ? _thumbBytes(context) : null;
    // Pre-warm: start decrypting to a temp file now (the bubble is in view) so the system
    // player launches almost immediately when tapped.
    if (message.isVideo && arrived && !inFlight) _fileFuture(context);

    if (message.isVideo && thumbFuture != null) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          ClipRRect(
            borderRadius: BorderRadius.circular(10),
            child: SizedBox(
              width: _thumbW,
              height: _thumbH,
              child: Stack(
                fit: StackFit.expand,
                children: [
                  FutureBuilder<Uint8List>(
                    future: thumbFuture,
                    builder: (context, snap) =>
                        (snap.connectionState == ConnectionState.done &&
                                snap.data != null)
                            ? Image.memory(snap.data!,
                                fit: BoxFit.cover,
                                cacheWidth: (_thumbW * 2).round())
                            : const ColoredBox(color: Colors.black38),
                  ),
                  const DecoratedBox(
                      decoration: BoxDecoration(color: Colors.black26)),
                  Center(
                    child: inFlight
                        ? const SizedBox(
                            width: 34,
                            height: 34,
                            child: CircularProgressIndicator(
                                strokeWidth: 3, color: Colors.white),
                          )
                        : const Icon(Icons.play_circle_fill,
                            color: Colors.white, size: 52),
                  ),
                  Positioned(
                    left: 6,
                    bottom: 6,
                    child: GestureDetector(
                      onTap: (arrived && !inFlight)
                          ? () => _openExternally(context)
                          : null,
                      child: Container(
                        padding: const EdgeInsets.symmetric(
                            horizontal: 6, vertical: 2),
                        decoration: BoxDecoration(
                          color: Colors.black54,
                          borderRadius: BorderRadius.circular(6),
                        ),
                        child: Text('🎬 $sizeLabel • $status',
                            style: const TextStyle(
                                color: Colors.white, fontSize: 11)),
                      ),
                    ),
                  ),
                  if (arrived && !inFlight)
                    Positioned.fill(
                      child: GestureDetector(
                        behavior: HitTestBehavior.translucent,
                        onTap: () => _openExternally(context),
                      ),
                    ),
                ],
              ),
            ),
          ),
        ],
      );
    }

    // Video without a thumbnail, or a generic file: a tappable tile.
    return InkWell(
      onTap: (arrived && !inFlight) ? () => _openExternally(context) : null,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          inFlight
              ? SizedBox(
                  width: 36,
                  height: 36,
                  child: Center(
                    child: SizedBox(
                      width: 22,
                      height: 22,
                      child: CircularProgressIndicator(
                          strokeWidth: 2, color: onColor),
                    ),
                  ),
                )
              : Icon(
                  message.isVideo
                      ? Icons.play_circle_fill
                      : Icons.insert_drive_file,
                  color: onColor,
                  size: 36),
          const SizedBox(width: 10),
          Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(message.isVideo ? l10n.video : l10n.file,
                  style:
                      TextStyle(color: onColor, fontWeight: FontWeight.w600)),
              Text('$sizeLabel • $status',
                  style: TextStyle(fontSize: 11, color: faint)),
            ],
          ),
        ],
      ),
    );
  }
}

/// Translucent spinner overlay shown over a media preview while it uploads.
class _UploadingOverlay extends StatelessWidget {
  const _UploadingOverlay();

  @override
  Widget build(BuildContext context) {
    return const DecoratedBox(
      decoration: BoxDecoration(color: Colors.black45),
      child: Center(
        child: SizedBox(
          width: 30,
          height: 30,
          child: CircularProgressIndicator(strokeWidth: 3, color: Colors.white),
        ),
      ),
    );
  }
}

/// Fullscreen, pinch-to-zoom image viewer opened by tapping a thumbnail.
class _ImageViewer extends StatelessWidget {
  const _ImageViewer({required this.bytes, required this.title});

  final Future<Uint8List> bytes;
  final String title;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.black,
      appBar: AppBar(
        backgroundColor: Colors.black,
        foregroundColor: Colors.white,
        title: Text(title),
      ),
      body: Center(
        child: FutureBuilder<Uint8List>(
          future: bytes,
          builder: (context, snap) {
            if (snap.connectionState != ConnectionState.done) {
              return const CircularProgressIndicator();
            }
            if (snap.hasError || snap.data == null) {
              return Text(AppLocalizations.of(context)!.imageUnavailable,
                  style: const TextStyle(color: Colors.white));
            }
            return InteractiveViewer(
              maxScale: 5,
              child: Image.memory(snap.data!, fit: BoxFit.contain),
            );
          },
        ),
      ),
    );
  }
}

class _Composer extends StatelessWidget {
  const _Composer({
    required this.controller,
    required this.onSend,
    required this.onAttach,
    required this.onPaste,
  });

  final TextEditingController controller;
  final Future<void> Function() onSend;
  final Future<void> Function() onAttach;
  final Future<void> Function() onPaste;

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return SafeArea(
      top: false,
      child: Padding(
        padding: const EdgeInsets.all(8),
        child: Row(
          children: [
            IconButton(
              tooltip: l10n.attachImageOrVideo,
              icon: const Icon(Icons.attach_file),
              onPressed: onAttach,
            ),
            IconButton(
              tooltip: l10n.pasteText,
              icon: const Icon(Icons.content_paste),
              onPressed: onPaste,
            ),
            Expanded(
              child: TextField(
                controller: controller,
                minLines: 1,
                maxLines: 4,
                textInputAction: TextInputAction.send,
                onSubmitted: (_) => onSend(),
                decoration: InputDecoration(
                  hintText: l10n.messageHint,
                  border: const OutlineInputBorder(),
                  contentPadding:
                      const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                ),
              ),
            ),
            const SizedBox(width: 8),
            IconButton.filled(
              onPressed: onSend,
              icon: const Icon(Icons.send),
            ),
          ],
        ),
      ),
    );
  }
}
