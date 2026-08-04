// ignore: unused_import
import 'package:intl/intl.dart' as intl;
import 'app_localizations.dart';

// ignore_for_file: type=lint

/// The translations for English (`en`).
class AppLocalizationsEn extends AppLocalizations {
  AppLocalizationsEn([String locale = 'en']) : super(locale);

  @override
  String get appTitle => 'Night Drop';

  @override
  String get newChat => 'New chat';

  @override
  String get pairInvite => 'Invite';

  @override
  String get pairJoin => 'Join';

  @override
  String get connect => 'Connect';

  @override
  String get connecting => 'Connecting…';

  @override
  String get scanInviteQr => 'Scan invite QR';

  @override
  String get pasteInviteLink => 'Paste invite link';

  @override
  String get shortCodeOrInviteLink => 'Short code or invite link';

  @override
  String get clipboardEmpty =>
      'Clipboard is empty — copy the invite link first.';

  @override
  String get inviteScanPreAuth => 'Have them scan this QR (pre-authorized):';

  @override
  String get inviteScanOrCode =>
      'Have them scan this QR — or enter the code below:';

  @override
  String get inviteLinkCopied => 'Invite link copied';

  @override
  String get copyInviteLink => 'Copy invite link';

  @override
  String get cantScanHint =>
      'Can’t scan? Send this link over a trusted channel; they paste it under Join.';

  @override
  String get orReadOutCode => '…or read out this code and your secret words:';

  @override
  String get secretWordsExplanation =>
      'The first number is just a meeting slot. The words are your shared secret — say them in person or over a trusted channel. They are never sent to any server, and they’re what stops an imposter.';

  @override
  String get supportNightDrop => 'Support Night Drop';

  @override
  String get copyAddress => 'Copy address';

  @override
  String addressCopied(String ticker) {
    return '$ticker address copied';
  }

  @override
  String donationQrLabel(String name) {
    return '$name donation address QR code';
  }

  @override
  String verifyTitle(String name) {
    return 'Verify $name';
  }

  @override
  String verifyIntro(String name) {
    return 'Compare this safety number with $name over a channel you trust (in person or a call). If both of you see the same number, no one is intercepting this chat. You can also scan their code, or show them yours.';
  }

  @override
  String get verifiedMatch => 'Verified — the codes match.';

  @override
  String verifiedNoMatch(String name) {
    return 'No match — this is not $name’s code.';
  }

  @override
  String get safetyQrLabel => 'Safety number verification QR code';

  @override
  String get yourSafetyCodeHint => 'Your safety code — let them scan it';

  @override
  String get scanTheirCode => 'Scan their code';

  @override
  String get scanTheirSafetyCode => 'Scan their safety code';

  @override
  String get markAsUnverified => 'Mark as unverified';

  @override
  String get markAsVerified => 'Mark as verified';

  @override
  String get verified => 'Verified';

  @override
  String get notVerified => 'Not verified';

  @override
  String peerVerifiedNote(String name) {
    return '$name marked this chat verified on their device. That’s only what they told you — compare the safety number yourself to be sure.';
  }

  @override
  String get cameraUnavailable =>
      'Camera scanning isn’t available on this device.\n\nOn the other device, tap “Copy invite link” and paste it here instead.';

  @override
  String get cameraDidntStart =>
      'The camera didn’t start.\n\nThis is usually temporary — another app may have just been using it. Try again.';

  @override
  String get retryCamera => 'Retry camera';

  @override
  String get cameraErrorFallback =>
      'Still not working? Have them copy the invite link and paste it under Join.';

  @override
  String get cameraPermissionPermanent =>
      'Night Drop needs camera access to scan an invite QR, but it’s been turned off. Enable Camera for Night Drop in Settings.';

  @override
  String get cameraPermissionNeeded =>
      'Night Drop needs camera access to scan an invite QR.';

  @override
  String get openSettings => 'Open settings';

  @override
  String get allowCamera => 'Allow camera';

  @override
  String get cameraPermissionFallback =>
      'Prefer not to? Have them copy the invite link and paste it under Join.';

  @override
  String get qrDetectedConnecting => 'QR code detected — connecting…';

  @override
  String get pointCameraHint => 'Point the camera at your contact’s invite QR.';

  @override
  String get qrNotRecognised =>
      'That QR isn’t a Night Drop invite. Ask them to open New chat → Invite.';

  @override
  String get qrNoneFound =>
      'Couldn’t read a QR code. Try more light, hold steadier, or move a little closer.';

  @override
  String get qrTryAgain => 'Try again';

  @override
  String get cancel => 'Cancel';

  @override
  String get restore => 'Restore';

  @override
  String get onboardingTagline =>
      'Anonymous, end-to-end encrypted, no accounts.\nYour identity lives only on this device.';

  @override
  String get createMyIdentity => 'Create my identity';

  @override
  String get restoreFromBackupFile => 'Restore from a backup file';

  @override
  String get restoreFromBackup => 'Restore from backup';

  @override
  String get restoreFromServerBackup => 'Restore from server backup';

  @override
  String get recoveryPassword => 'Recovery password';

  @override
  String enterRecoveryPasswordFor(String path) {
    return 'Enter the recovery password for:\n$path';
  }

  @override
  String get enterServerRecoveryPassword =>
      'Enter the recovery password you recorded when you enabled server backup. The encrypted copy is fetched from the server; only your password can open it.';

  @override
  String couldNotRestoreBackup(String error) {
    return 'Could not restore backup — check the password and file. ($error)';
  }

  @override
  String couldNotRestoreBackupFailed(String error) {
    return 'Could not restore backup. ($error)';
  }

  @override
  String couldNotRestoreServerFailed(String error) {
    return 'Could not restore from server. ($error)';
  }

  @override
  String couldNotRestoreServer(String error) {
    return 'Could not restore from server — check the password. ($error)';
  }

  @override
  String get stageGeneratingIdentity => 'Generating your anonymous identity…';

  @override
  String get stageConnectingTor => 'Connecting to the Tor network…';

  @override
  String get stageBuildingCircuit => 'Building a private circuit…';

  @override
  String get stagePublishingOnion => 'Publishing your onion address…';

  @override
  String get stageAlmostReady => 'Almost ready — this can take up to a minute…';

  @override
  String get splashRestoringSession => 'Restoring your secure session…';

  @override
  String get splashReconnectingTor => 'Reconnecting to the Tor network…';

  @override
  String get splashRebuildingCircuit => 'Rebuilding your private circuit…';

  @override
  String get splashAlmostThere =>
      'Almost there — this can take up to a minute…';

  @override
  String get loadErrorTitle => 'Couldn’t open your saved session';

  @override
  String get loadErrorBody =>
      'Night Drop found data saved on this device but couldn’t open it. Two common, fixable causes: Tor hasn’t connected yet, or your system keyring/wallet (e.g. KDE Wallet or GNOME Keyring) is locked — unlock it, then tap Try again. Your existing data is preserved and won’t be overwritten automatically.';

  @override
  String get loadErrorBodyMobile =>
      'Night Drop found data saved on this device but couldn’t open it. Usually Tor simply hasn’t connected yet — tap Try again. If it keeps failing, the key that unlocks this data is no longer on the device, which can happen after clearing the app’s storage or moving to a new phone; in that case only a backup can bring it back. Your existing data is preserved and won’t be overwritten automatically.';

  @override
  String get tryAgain => 'Try again';

  @override
  String get setUpNewIdentity => 'Set up a new identity or restore a backup';

  @override
  String get loadErrorFootnote =>
      'Setting up won’t delete the preserved copy of your old data.';

  @override
  String get backupWhatTitle => 'What to back up';

  @override
  String get backupWhatSubtitle =>
      'Lite keeps you reachable without copying your messages.';

  @override
  String get backupLiteTitle => 'Lite — identity & contacts only';

  @override
  String get backupLiteSubtitle => 'No message history or media';

  @override
  String get backupFullTitle => 'Full — everything';

  @override
  String get backupFullSubtitle => 'Includes message history and media';

  @override
  String get passwordMismatch =>
      'That doesn’t match. Check what you wrote down.';

  @override
  String get yourRecoveryPassword => 'Your recovery password';

  @override
  String get writtenItDown => 'I’ve written it down';

  @override
  String get confirmRecoveryPassword => 'Confirm your recovery password';

  @override
  String get typeItBack =>
      'Type it back to make sure it’s saved correctly. There’s no way to recover it later if it’s wrong.';

  @override
  String get showItAgain => 'Show it again';

  @override
  String get confirm => 'Confirm';

  @override
  String get preparingBackup => 'Preparing backup…';

  @override
  String couldNotPrepareBackup(String error) {
    return 'Could not prepare backup: $error';
  }

  @override
  String get backupIntroFile =>
      'Write this down and keep it safe. It is shown only once and is the only way to restore this backup — we cannot recover it for you.';

  @override
  String backupSavedTo(String path) {
    return 'Backup saved to $path';
  }

  @override
  String couldNotSaveBackup(String error) {
    return 'Could not save backup: $error';
  }

  @override
  String get mergeChatBackupTitle => 'Merge a chat backup';

  @override
  String get merge => 'Merge';

  @override
  String get restoringBackup => 'Restoring backup…';

  @override
  String mergedChatBackup(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other: '$count messages added.',
      one: '1 message added.',
    );
    return 'Merged chat backup — $_temp0';
  }

  @override
  String couldNotMergeBackup(String error) {
    return 'Could not merge backup — check the password and file. ($error)';
  }

  @override
  String get save => 'Save';

  @override
  String get close => 'Close';

  @override
  String get later => 'Later';

  @override
  String get relaysShort => 'Relays';

  @override
  String get decline => 'Decline';

  @override
  String get approve => 'Approve';

  @override
  String get enabled => 'Enabled';

  @override
  String get chats => 'Chats';

  @override
  String get backUp => 'Back up';

  @override
  String get saveBackupFile => 'Save backup file';

  @override
  String get backUpToServer24h => 'Back up to server (24h)';

  @override
  String get mergeChatBackupMenu => 'Merge a chat backup…';

  @override
  String get myIdentity => 'My identity';

  @override
  String get backgroundDeliveryMenu => 'Background delivery…';

  @override
  String get myRelaysMenu => 'My relays…';

  @override
  String get resetTorMenu => 'Reset Tor connection…';

  @override
  String get resetTorTitle => 'Reset Tor connection?';

  @override
  String get resetTorBody =>
      'If messages keep being held for delivery instead of arriving directly, this device\'s route into Tor may be stuck. This picks a fresh route and reconnects.\n\nYour identity, your address and your chats are not affected. Reconnecting takes a minute or two.';

  @override
  String get resetTorConfirm => 'Reset';

  @override
  String get resetTorRunning =>
      'Reconnecting to Tor — this takes a minute or two.';

  @override
  String get aboutMenu => 'About Night Drop';

  @override
  String get logoutDeleteMenu => 'Log out / delete identity';

  @override
  String get noChatsYet =>
      'No chats yet.\nTap “New chat” to pair by QR or short code.';

  @override
  String get storedOnServer24h => 'Stored on server (24h)';

  @override
  String get storedOnThisDevice => 'Stored on this device';

  @override
  String get publishingAddressTor =>
      'Publishing your address to Tor (1–3 min). Others can’t pair with you until this finishes — keep the app open.';

  @override
  String relayOfflineOne(String names) {
    return 'Your relay $names looks offline. ';
  }

  @override
  String relayOfflineMany(String names) {
    return 'Your relays $names look offline. ';
  }

  @override
  String get relayAdviceHasBackup =>
      'Contacts can still reach you through your other relay for now.';

  @override
  String get relayAdviceNoBackup =>
      'Add a backup relay so people can still reach you while it’s down.';

  @override
  String get backupReminderBody =>
      'Back up your identity so you don’t lose your chats. There’s no account to recover from — if you lose this device without a backup, it’s gone.';

  @override
  String get deleteThisChat => 'Delete this chat?';

  @override
  String deleteChatBody(String name) {
    return 'This permanently deletes your conversation with $name. $name will be told the chat was deleted. To talk again you will both need to create a new chat.';
  }

  @override
  String get deleteChat => 'Delete chat';

  @override
  String get backgroundDelivery => 'Background delivery';

  @override
  String get backgroundDeliveryBody =>
      'Keep receiving messages while Night Drop is in the background. This runs a foreground service with a persistent notification and checks for messages over Tor — no push provider, nothing leaves your device to a server.';

  @override
  String get onboardingBackgroundTitle => 'Receive messages in the background?';

  @override
  String get onboardingBackgroundBody =>
      'Android suspends Night Drop whenever it is not on screen, so without this, messages only arrive once you open the app.\n\nTurning it on keeps Night Drop running with a permanent notification and checks for messages over Tor. There is no push service — nothing is registered with Google, and nothing about you leaves your device.\n\nIt uses some battery. If you set an app lock later, your key stays in memory while locked so messages can still be decrypted.\n\nYou can change this any time under Background delivery in the menu.';

  @override
  String get onboardingBackgroundEnable => 'Turn on';

  @override
  String get onboardingBackgroundSkip => 'Not now';

  @override
  String get notificationPermissionRequired =>
      'Notification permission is required for background delivery.';

  @override
  String get backgroundDeliveryOn => 'Background delivery on';

  @override
  String get backgroundDeliveryOff => 'Background delivery off';

  @override
  String get myRelays => 'My relays';

  @override
  String get editRelaysBody =>
      'Extra relays that also hold your mailbox. Contacts fan your offline mail out to these in addition to the shared default, so a message still reaches you if one relay is down or blocked. Relays only ever see opaque, end-to-end-encrypted blobs. One address per line.';

  @override
  String couldNotSaveRelays(String error) {
    return 'Could not save relays: $error';
  }

  @override
  String get usingDefaultRelayOnly => 'Using the default relay only';

  @override
  String advertisingExtraRelays(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other: '$count extra relays',
      one: '1 extra relay',
    );
    return 'Advertising $_temp0';
  }

  @override
  String get backingUpToServer => 'Backing up to server…';

  @override
  String couldNotBackUpToServer(String error) {
    return 'Could not back up to server: $error';
  }

  @override
  String get serverBackupIntro =>
      'An encrypted copy is now stored on the server. Write this password down — it is shown only once, never leaves your device, and is the ONLY way to restore. We cannot recover it for you.';

  @override
  String serverBackupFooter(String expiry) {
    return 'The server copy is deleted at $expiry. After that, only your recorded password + a local or file backup can restore this identity.';
  }

  @override
  String get myIdentityBody =>
      'This is your anonymous, device-held identity — no name, no account. Others see this id once you pair.';

  @override
  String get logoutTitle => 'Log out & delete this identity?';

  @override
  String get logoutBody =>
      'This permanently erases your identity, all contacts, and all messages from this device. There is no undo.\n\n• The ONLY way back into these chats is restoring a backup — so make one first if you haven’t.\n• Everyone you chat with will have to pair with you again.\n\nContinue?';

  @override
  String get logoutDelete => 'Log out & delete';

  @override
  String logoutNotNotified(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other:
          '$count contacts may not have been notified that these chats are gone.',
      one: '1 contact may not have been notified that this chat is gone.',
    );
    return '$_temp0';
  }

  @override
  String get chatRequest => 'Chat request';

  @override
  String get approvingOverTor =>
      'Approving over Tor — this can take a few seconds…';

  @override
  String requestFrom(String id) {
    return 'From $id. Approve to start chatting.';
  }

  @override
  String couldNotApprove(String error) {
    return 'Could not approve: $error';
  }

  @override
  String couldNotDecline(String error) {
    return 'Could not decline: $error';
  }

  @override
  String get delete => 'Delete';

  @override
  String get edit => 'Edit';

  @override
  String get verify => 'Verify';

  @override
  String get video => 'Video';

  @override
  String get file => 'File';

  @override
  String get more => 'More';

  @override
  String get cantSendUntilAccepted =>
      'They haven’t accepted the chat yet. Your message hasn’t been sent — it’s still in the box, so you can send it once they accept.';

  @override
  String get couldntSendOffline =>
      'Couldn’t send — they’re offline and no relay could hold the message. Check your relay settings and try again; your message is still here.';

  @override
  String couldntSend(String error) {
    return 'Couldn’t send: $error';
  }

  @override
  String fileTooLarge(String size, String limit) {
    return 'That file is $size — the limit is $limit.';
  }

  @override
  String get nothingToPaste => 'Nothing to paste.';

  @override
  String couldNotSendAttachment(String error) {
    return 'Could not send attachment: $error';
  }

  @override
  String get editMessageTitle => 'Edit message';

  @override
  String couldNotEdit(String error) {
    return 'Could not edit: $error';
  }

  @override
  String get deleteForEveryone => 'Delete for everyone';

  @override
  String get deleteForEveryoneTitle => 'Delete for everyone?';

  @override
  String get unsendBody =>
      'This removes the message from both devices. If it hasn’t been delivered yet, the other person never receives it.';

  @override
  String couldNotDelete(String error) {
    return 'Could not delete: $error';
  }

  @override
  String get yourNameInChat => 'Your name in this chat';

  @override
  String get disappearingOff => 'Off';

  @override
  String get disappearing1Hour => '1 hour';

  @override
  String get disappearing1Day => '1 day';

  @override
  String get disappearing1Week => '1 week';

  @override
  String get disappearingMessages => 'Disappearing messages';

  @override
  String get disappearingSubtitle =>
      'Delete messages on both devices after a set time.';

  @override
  String couldNotSetTimer(String error) {
    return 'Could not set timer: $error';
  }

  @override
  String get verifySafetyNumber => 'Verify safety number';

  @override
  String get renameYourselfTooltip => 'Rename yourself in this chat';

  @override
  String get storedServerTooltipOn => 'Stored on server (24h) — tap to disable';

  @override
  String get storedServerTooltipOff =>
      'Stored on this device — tap to enable 24h server storage';

  @override
  String disappearingTooltipOn(String label) {
    return 'Disappearing messages: $label — tap to change';
  }

  @override
  String get disappearingTooltipOff =>
      'Disappearing messages: off — tap to set';

  @override
  String get deleteThisChatTooltip => 'Delete this chat';

  @override
  String get backUpThisChat => 'Back up this chat…';

  @override
  String get sayHi => 'Say hi 👻';

  @override
  String get remoteStorageBannerHealthy =>
      'Messages in this chat are stored on the server (encrypted) for up to 24 hours so both devices use less space.';

  @override
  String get remoteStorageBannerUnhealthy =>
      'Server storage is on, but the relay couldn’t be reached — recent messages were delivered but not stored on the server. They’ll be stored again once a relay is reachable.';

  @override
  String get unverifiedBannerBody =>
      'Not verified — confirm this contact’s safety number to be sure no one’s in the middle.';

  @override
  String get peerBackupBanner =>
      'The other person keeps a backup of this chat, so your messages may persist in their backup.';

  @override
  String get awaitingApprovalBanner =>
      'Waiting for the other person to accept the chat. Messages you send won’t be delivered until they do.';

  @override
  String get messageDeleted => 'Message deleted';

  @override
  String get editedTag => 'edited';

  @override
  String get deliveryHeld => 'Held for delivery';

  @override
  String get deliveryExpired => 'Not delivered (expired)';

  @override
  String get deliveryDelivered => 'Delivered';

  @override
  String get deliverySent => 'Not confirmed yet';

  @override
  String get imageUnavailable => 'Image unavailable';

  @override
  String mediaImage(String size) {
    return 'Image • $size';
  }

  @override
  String mediaImageSending(String size) {
    return 'Image • $size • sending…';
  }

  @override
  String couldNotOpenFile(String error) {
    return 'Could not open the file: $error';
  }

  @override
  String couldNotOpen(String error) {
    return 'Could not open: $error';
  }

  @override
  String get mediaStatusSending => 'sending…';

  @override
  String get mediaStatusIncoming => 'incoming…';

  @override
  String get mediaStatusTapToPlay => 'tap to play';

  @override
  String get attachImageOrVideo => 'Attach image or video';

  @override
  String get pasteText => 'Paste text';

  @override
  String get messageHint => 'Message';

  @override
  String get appLockMenu => 'App lock';

  @override
  String get lockedTitle => 'Night Drop is locked';

  @override
  String get lockedBody =>
      'Enter your PIN or passphrase to open your messages.';

  @override
  String get lockedField => 'PIN or passphrase';

  @override
  String get lockedFailed => 'That didn\'t unlock it. Try again.';

  @override
  String get lockedUnlock => 'Unlock';

  @override
  String get lockedNoRecovery =>
      'There is no way to recover this. Without it, the messages on this device stay encrypted for good.';

  @override
  String get appLockOffTitle => 'App lock is off';

  @override
  String get appLockOffBody =>
      'Your messages are unlocked whenever the app opens. Turn on a lock to require a PIN or passphrase first.';

  @override
  String get appLockOnTitle => 'App lock is on';

  @override
  String get appLockOnBody =>
      'Your messages need your PIN or passphrase before they can be opened.';

  @override
  String get appLockChoosePin => 'Use a PIN';

  @override
  String get appLockChoosePinBody =>
      'Quick to type. Stops someone who picks up your unlocked phone — but a short PIN can be broken by someone who copies the data off your device.';

  @override
  String get appLockChoosePassphrase => 'Use a passphrase';

  @override
  String get appLockChoosePassphraseBody =>
      'Longer to type. The only option that also protects your messages if someone copies the data off your device.';

  @override
  String get appLockEnable => 'Turn on app lock';

  @override
  String get appLockDisable => 'Turn off app lock';

  @override
  String get appLockNewSecret => 'New PIN or passphrase';

  @override
  String get appLockConfirmSecret => 'Enter it again';

  @override
  String get appLockMismatch => 'Those don\'t match.';

  @override
  String get appLockTooShortPin => 'Use at least 6 digits.';

  @override
  String get appLockTooShortPassphrase => 'Use at least 12 characters.';

  @override
  String get appLockWarnNoRecovery =>
      'If you forget this, your messages are gone. There is no reset and no recovery — not by us, not by anyone.';

  @override
  String get appLockIUnderstand => 'I understand — turn it on';

  @override
  String get appLockEnabled => 'App lock is on.';

  @override
  String get appLockDisabled => 'App lock is off.';

  @override
  String get appLockWrongSecret => 'That didn\'t match. Nothing was changed.';

  @override
  String get appLockCurrentSecret => 'Current PIN or passphrase';

  @override
  String get appLockBgNote =>
      'Background delivery is on, so your key stays in memory while the app is locked — otherwise messages couldn\'t arrive. Turn background delivery off if you\'d rather the key be forgotten each time you lock.';

  @override
  String get duressSet => 'Set a wipe code';

  @override
  String get duressReplace => 'Replace it';

  @override
  String get duressRemove => 'Remove the wipe code';

  @override
  String get duressSkip => 'Not now';

  @override
  String get duressMenu => 'Wipe code';

  @override
  String get duressOnBody =>
      'A wipe code is set. Entering it at the lock screen deletes your identity and messages instead of opening them.';

  @override
  String get duressOffBody =>
      'No wipe code is set. You can add a second code that deletes your identity and messages instead of opening them.';

  @override
  String get duressOfferBody =>
      'You can also set a second code that wipes instead of unlocking. Entering it at the lock screen deletes your identity and messages and leaves the app looking freshly installed. You can add one later under “Wipe code”.';

  @override
  String get duressNeedsLock =>
      'Set an app lock first. The wipe code is a second code for the same lock screen.';

  @override
  String get duressTitle => 'Wipe code';

  @override
  String get duressBody =>
      'A second code for the lock screen. Entering it does not open your messages — it deletes them, along with your identity, and leaves the app looking like it was just installed.';

  @override
  String get duressNew => 'New wipe code';

  @override
  String get duressConfirm => 'Enter it again';

  @override
  String get duressSame =>
      'This must be different from the code that opens your messages.';

  @override
  String get duressWarnTitle => 'Before you set this';

  @override
  String get duressWarnBody =>
      'You cannot practise this. Entering the wipe code destroys everything on this device, every time, with no confirmation and no undo — that is what makes it work under pressure.\n\nWe have checked that the code you just chose does work. You will not be reminded that a wipe code is set: showing it anywhere would tell whoever picks up your phone that it exists.\n\nYour contacts are not told anything. The wipe code deliberately gives the app no way to read your messages, which also leaves it no way to send on your behalf — so your contacts simply stop hearing from you. Agree on another way to reach each other before you need it.';

  @override
  String get duressUnderstand => 'I understand — set it';

  @override
  String get duressDone => 'Wipe code set.';

  @override
  String get duressCleared => 'Wipe code removed.';

  @override
  String get duressCurrentSecret => 'Your normal PIN or passphrase';

  @override
  String get coverTrafficMenu => 'Cover traffic';

  @override
  String get coverTrafficTitle => 'Cover traffic';

  @override
  String get coverTrafficBody =>
      'Sends occasional dummy mail to yourself, so the server that holds your offline messages sees activity it can\'t tell apart from real messages. Without it, that server can build a picture of when you\'re awake and active — not what you say, or to whom, but when.';

  @override
  String get coverTrafficLimit =>
      'This raises the cost of watching you; it does not stop it. Real messages are still sent on top of the dummy ones, so someone patient can still see when you\'re genuinely busy. It also costs battery and data, and only runs while the app is open (or with background delivery on).';

  @override
  String get coverTrafficOn => 'Cover traffic is on.';

  @override
  String get coverTrafficOff => 'Cover traffic is off.';

  @override
  String get turnOn => 'Turn on';

  @override
  String get turnOff => 'Turn off';

  @override
  String get nameContactTitle => 'Name this contact';

  @override
  String get nameContactTooltip => 'Name this contact (only you see it)';

  @override
  String nameContactBody(String tag) {
    return 'A name only you see — it is never sent, and they can\'\'t see or change it. Until you set one, this contact shows as $tag, which comes from their key: it changes if their identity does. It is not proof of who they are — compare the safety number for that.';
  }

  @override
  String silenceBanner(int days) {
    return 'No sign of this person for $days days. Your messages are still being held for them. There are many reasons someone goes quiet — if it matters, reach them another way.';
  }
}
