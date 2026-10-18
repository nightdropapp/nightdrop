import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:intl/intl.dart' as intl;

import 'app_localizations_en.dart';

// ignore_for_file: type=lint

/// Callers can lookup localized strings with an instance of AppLocalizations
/// returned by `AppLocalizations.of(context)`.
///
/// Applications need to include `AppLocalizations.delegate()` in their app's
/// `localizationDelegates` list, and the locales they support in the app's
/// `supportedLocales` list. For example:
///
/// ```dart
/// import 'l10n/app_localizations.dart';
///
/// return MaterialApp(
///   localizationsDelegates: AppLocalizations.localizationsDelegates,
///   supportedLocales: AppLocalizations.supportedLocales,
///   home: MyApplicationHome(),
/// );
/// ```
///
/// ## Update pubspec.yaml
///
/// Please make sure to update your pubspec.yaml to include the following
/// packages:
///
/// ```yaml
/// dependencies:
///   # Internationalization support.
///   flutter_localizations:
///     sdk: flutter
///   intl: any # Use the pinned version from flutter_localizations
///
///   # Rest of dependencies
/// ```
///
/// ## iOS Applications
///
/// iOS applications define key application metadata, including supported
/// locales, in an Info.plist file that is built into the application bundle.
/// To configure the locales supported by your app, you’ll need to edit this
/// file.
///
/// First, open your project’s ios/Runner.xcworkspace Xcode workspace file.
/// Then, in the Project Navigator, open the Info.plist file under the Runner
/// project’s Runner folder.
///
/// Next, select the Information Property List item, select Add Item from the
/// Editor menu, then select Localizations from the pop-up menu.
///
/// Select and expand the newly-created Localizations item then, for each
/// locale your application supports, add a new item and select the locale
/// you wish to add from the pop-up menu in the Value field. This list should
/// be consistent with the languages listed in the AppLocalizations.supportedLocales
/// property.
abstract class AppLocalizations {
  AppLocalizations(String locale)
      : localeName = intl.Intl.canonicalizedLocale(locale.toString());

  final String localeName;

  static AppLocalizations? of(BuildContext context) {
    return Localizations.of<AppLocalizations>(context, AppLocalizations);
  }

  static const LocalizationsDelegate<AppLocalizations> delegate =
      _AppLocalizationsDelegate();

  /// A list of this localizations delegate along with the default localizations
  /// delegates.
  ///
  /// Returns a list of localizations delegates containing this delegate along with
  /// GlobalMaterialLocalizations.delegate, GlobalCupertinoLocalizations.delegate,
  /// and GlobalWidgetsLocalizations.delegate.
  ///
  /// Additional delegates can be added by appending to this list in
  /// MaterialApp. This list does not have to be used at all if a custom list
  /// of delegates is preferred or required.
  static const List<LocalizationsDelegate<dynamic>> localizationsDelegates =
      <LocalizationsDelegate<dynamic>>[
    delegate,
    GlobalMaterialLocalizations.delegate,
    GlobalCupertinoLocalizations.delegate,
    GlobalWidgetsLocalizations.delegate,
  ];

  /// A list of this localizations delegate's supported locales.
  static const List<Locale> supportedLocales = <Locale>[Locale('en')];

  /// The application name, shown as the window/app title.
  ///
  /// In en, this message translates to:
  /// **'Night Drop'**
  String get appTitle;

  /// Title of the pairing screen where a user starts a new 1:1 chat.
  ///
  /// In en, this message translates to:
  /// **'New chat'**
  String get newChat;

  /// Tab label: show a QR / short code to invite someone.
  ///
  /// In en, this message translates to:
  /// **'Invite'**
  String get pairInvite;

  /// Tab label: enter a short code / invite link to join.
  ///
  /// In en, this message translates to:
  /// **'Join'**
  String get pairJoin;

  /// Button that starts the pairing handshake with the entered code.
  ///
  /// In en, this message translates to:
  /// **'Connect'**
  String get connect;

  /// Overlay label shown while the pairing handshake runs.
  ///
  /// In en, this message translates to:
  /// **'Connecting…'**
  String get connecting;

  /// Button/screen title to open the camera and scan a pairing QR code.
  ///
  /// In en, this message translates to:
  /// **'Scan invite QR'**
  String get scanInviteQr;

  /// Button that pairs using an invite link from the clipboard.
  ///
  /// In en, this message translates to:
  /// **'Paste invite link'**
  String get pasteInviteLink;

  /// Text field label for the pairing code / link input.
  ///
  /// In en, this message translates to:
  /// **'Short code or invite link'**
  String get shortCodeOrInviteLink;

  /// Error shown when Paste invite link is tapped with an empty clipboard.
  ///
  /// In en, this message translates to:
  /// **'Clipboard is empty — copy the invite link first.'**
  String get clipboardEmpty;

  /// Instruction above a pre-authorized invite QR code.
  ///
  /// In en, this message translates to:
  /// **'Have them scan this QR (pre-authorized):'**
  String get inviteScanPreAuth;

  /// Instruction above an invite QR that also has a short code.
  ///
  /// In en, this message translates to:
  /// **'Have them scan this QR — or enter the code below:'**
  String get inviteScanOrCode;

  /// Snackbar after copying the invite link to the clipboard.
  ///
  /// In en, this message translates to:
  /// **'Invite link copied'**
  String get inviteLinkCopied;

  /// Button: send the invite link through the system share sheet, not the clipboard.
  ///
  /// In en, this message translates to:
  /// **'Share invite link'**
  String get shareInviteLink;

  /// Button to copy the invite link to the clipboard.
  ///
  /// In en, this message translates to:
  /// **'Copy invite link'**
  String get copyInviteLink;

  /// Hint under the copy-invite-link button for camera-less pairing.
  ///
  /// In en, this message translates to:
  /// **'Can’t scan? Send this link over a trusted channel; they paste it under Join.'**
  String get cantScanHint;

  /// Heading above the short code + secret words on the invite screen.
  ///
  /// In en, this message translates to:
  /// **'…or read out this code and your secret words:'**
  String get orReadOutCode;

  /// Explanation of the short code's slot vs. secret words.
  ///
  /// In en, this message translates to:
  /// **'The first number is just a meeting slot. The words are your shared secret — say them in person or over a trusted channel. They are never sent to any server, and they’re what stops an imposter.'**
  String get secretWordsExplanation;

  /// Title of the donations screen.
  ///
  /// In en, this message translates to:
  /// **'Support Night Drop'**
  String get supportNightDrop;

  /// Button to copy a cryptocurrency donation address to the clipboard.
  ///
  /// In en, this message translates to:
  /// **'Copy address'**
  String get copyAddress;

  /// Snackbar confirming a donation address was copied.
  ///
  /// In en, this message translates to:
  /// **'{ticker} address copied'**
  String addressCopied(String ticker);

  /// Accessibility label for a donation-address QR code.
  ///
  /// In en, this message translates to:
  /// **'{name} donation address QR code'**
  String donationQrLabel(String name);

  /// Title of the safety-number verification screen.
  ///
  /// In en, this message translates to:
  /// **'Verify {name}'**
  String verifyTitle(String name);

  /// Explanatory text at the top of the verification screen.
  ///
  /// In en, this message translates to:
  /// **'Compare this safety number with {name} over a channel you trust (in person or a call). If both of you see the same number, no one is intercepting this chat. You can also scan their code, or show them yours.'**
  String verifyIntro(String name);

  /// Snackbar shown when a scanned safety code matches.
  ///
  /// In en, this message translates to:
  /// **'Verified — the codes match.'**
  String get verifiedMatch;

  /// Snackbar shown when a scanned safety code does not match.
  ///
  /// In en, this message translates to:
  /// **'No match — this is not {name}’s code.'**
  String verifiedNoMatch(String name);

  /// Accessibility label for the safety-number QR code.
  ///
  /// In en, this message translates to:
  /// **'Safety number verification QR code'**
  String get safetyQrLabel;

  /// Caption under the user's own safety-number QR code.
  ///
  /// In en, this message translates to:
  /// **'Your safety code — let them scan it'**
  String get yourSafetyCodeHint;

  /// Button to scan the other person's safety code.
  ///
  /// In en, this message translates to:
  /// **'Scan their code'**
  String get scanTheirCode;

  /// Title of the scanner when scanning a safety code (not a pairing QR).
  ///
  /// In en, this message translates to:
  /// **'Scan their safety code'**
  String get scanTheirSafetyCode;

  /// Button to clear the verified state of a contact.
  ///
  /// In en, this message translates to:
  /// **'Mark as unverified'**
  String get markAsUnverified;

  /// Button to manually mark a contact as verified.
  ///
  /// In en, this message translates to:
  /// **'Mark as verified'**
  String get markAsVerified;

  /// Chip/badge label for a verified contact.
  ///
  /// In en, this message translates to:
  /// **'Verified'**
  String get verified;

  /// Chip/badge label for an unverified contact.
  ///
  /// In en, this message translates to:
  /// **'Not verified'**
  String get notVerified;

  /// Informational banner shown when the peer has signaled they verified the safety number. Must not imply we are verified.
  ///
  /// In en, this message translates to:
  /// **'{name} marked this chat verified on their device. That’s only what they told you — compare the safety number yourself to be sure.'**
  String peerVerifiedNote(String name);

  /// Shown on desktop where there is no camera backend for scanning.
  ///
  /// In en, this message translates to:
  /// **'Camera scanning isn’t available on this device.\n\nOn the other device, tap “Copy invite link” and paste it here instead.'**
  String get cameraUnavailable;

  /// Shown when the camera failed to start / stalled.
  ///
  /// In en, this message translates to:
  /// **'The camera didn’t start.\n\nThis is usually temporary — another app may have just been using it. Try again.'**
  String get cameraDidntStart;

  /// Button to re-initialize the camera after a stalled start.
  ///
  /// In en, this message translates to:
  /// **'Retry camera'**
  String get retryCamera;

  /// Fallback hint shown under the camera-error retry button.
  ///
  /// In en, this message translates to:
  /// **'Still not working? Have them copy the invite link and paste it under Join.'**
  String get cameraErrorFallback;

  /// Shown when camera permission is permanently denied / restricted.
  ///
  /// In en, this message translates to:
  /// **'Night Drop needs camera access to scan an invite QR, but it’s been turned off. Enable Camera for Night Drop in Settings.'**
  String get cameraPermissionPermanent;

  /// Shown when camera permission has not yet been granted.
  ///
  /// In en, this message translates to:
  /// **'Night Drop needs camera access to scan an invite QR.'**
  String get cameraPermissionNeeded;

  /// Button that opens system app settings to enable a permission.
  ///
  /// In en, this message translates to:
  /// **'Open settings'**
  String get openSettings;

  /// Button that re-requests the camera permission in-app.
  ///
  /// In en, this message translates to:
  /// **'Allow camera'**
  String get allowCamera;

  /// Fallback hint shown under the camera-permission request.
  ///
  /// In en, this message translates to:
  /// **'Prefer not to? Have them copy the invite link and paste it under Join.'**
  String get cameraPermissionFallback;

  /// Overlay label the instant a valid QR is decoded.
  ///
  /// In en, this message translates to:
  /// **'QR code detected — connecting…'**
  String get qrDetectedConnecting;

  /// Hint overlaid on the live camera preview while scanning.
  ///
  /// In en, this message translates to:
  /// **'Point the camera at your contact’s invite QR.'**
  String get pointCameraHint;

  /// Toast when a QR decodes cleanly but is not a pairing payload.
  ///
  /// In en, this message translates to:
  /// **'That QR isn’t a Night Drop invite. Ask them to open New chat → Invite.'**
  String get qrNotRecognised;

  /// Toast when no QR could be decoded before the scan deadline.
  ///
  /// In en, this message translates to:
  /// **'Couldn’t read a QR code. Try more light, hold steadier, or move a little closer.'**
  String get qrNoneFound;

  /// Action on the scan-failed toast that restarts scanning.
  ///
  /// In en, this message translates to:
  /// **'Try again'**
  String get qrTryAgain;

  /// Generic Cancel button, used across dialogs.
  ///
  /// In en, this message translates to:
  /// **'Cancel'**
  String get cancel;

  /// Confirm button in a restore-from-backup dialog.
  ///
  /// In en, this message translates to:
  /// **'Restore'**
  String get restore;

  /// Subtitle on the first-run onboarding screen.
  ///
  /// In en, this message translates to:
  /// **'Anonymous, end-to-end encrypted, no accounts.\nYour identity lives only on this device.'**
  String get onboardingTagline;

  /// Primary button to generate a new anonymous identity.
  ///
  /// In en, this message translates to:
  /// **'Create my identity'**
  String get createMyIdentity;

  /// Button to restore an identity from a local backup file.
  ///
  /// In en, this message translates to:
  /// **'Restore from a backup file'**
  String get restoreFromBackupFile;

  /// Title of the restore-from-file dialog.
  ///
  /// In en, this message translates to:
  /// **'Restore from backup'**
  String get restoreFromBackup;

  /// Button/dialog title to restore from the opt-in server backup.
  ///
  /// In en, this message translates to:
  /// **'Restore from server backup'**
  String get restoreFromServerBackup;

  /// Text-field label for the backup recovery password.
  ///
  /// In en, this message translates to:
  /// **'Recovery password'**
  String get recoveryPassword;

  /// Prompt above the password field when restoring a specific backup file.
  ///
  /// In en, this message translates to:
  /// **'Enter the recovery password for:\n{path}'**
  String enterRecoveryPasswordFor(String path);

  /// Prompt when restoring from the server backup.
  ///
  /// In en, this message translates to:
  /// **'Enter the recovery password you recorded when you enabled server backup. The encrypted copy is fetched from the server; only your password can open it.'**
  String get enterServerRecoveryPassword;

  /// Snackbar when a local backup restore fails.
  ///
  /// In en, this message translates to:
  /// **'Could not restore backup — check the password and file. ({error})'**
  String couldNotRestoreBackup(String error);

  /// Snackbar when a local backup restore fails for a reason that is NOT a bad password or damaged file (e.g. Tor could not start), so the message must not blame the password.
  ///
  /// In en, this message translates to:
  /// **'Could not restore backup. ({error})'**
  String couldNotRestoreBackupFailed(String error);

  /// Snackbar when a server backup restore fails for a reason that is NOT a bad password (e.g. Tor could not start), so the message must not blame the password.
  ///
  /// In en, this message translates to:
  /// **'Could not restore from server. ({error})'**
  String couldNotRestoreServerFailed(String error);

  /// Snackbar when a server backup restore fails.
  ///
  /// In en, this message translates to:
  /// **'Could not restore from server — check the password. ({error})'**
  String couldNotRestoreServer(String error);

  /// Bootstrap progress line.
  ///
  /// In en, this message translates to:
  /// **'Generating your anonymous identity…'**
  String get stageGeneratingIdentity;

  /// Bootstrap progress line.
  ///
  /// In en, this message translates to:
  /// **'Connecting to the Tor network…'**
  String get stageConnectingTor;

  /// Bootstrap progress line.
  ///
  /// In en, this message translates to:
  /// **'Building a private circuit…'**
  String get stageBuildingCircuit;

  /// Bootstrap progress line.
  ///
  /// In en, this message translates to:
  /// **'Publishing your onion address…'**
  String get stagePublishingOnion;

  /// Bootstrap progress line.
  ///
  /// In en, this message translates to:
  /// **'Almost ready — this can take up to a minute…'**
  String get stageAlmostReady;

  /// Launch splash progress line.
  ///
  /// In en, this message translates to:
  /// **'Restoring your secure session…'**
  String get splashRestoringSession;

  /// Launch splash progress line.
  ///
  /// In en, this message translates to:
  /// **'Reconnecting to the Tor network…'**
  String get splashReconnectingTor;

  /// Launch splash progress line.
  ///
  /// In en, this message translates to:
  /// **'Rebuilding your private circuit…'**
  String get splashRebuildingCircuit;

  /// Launch splash progress line.
  ///
  /// In en, this message translates to:
  /// **'Almost there — this can take up to a minute…'**
  String get splashAlmostThere;

  /// Title of the launch-time load-failure recovery screen.
  ///
  /// In en, this message translates to:
  /// **'Couldn’t open your saved session'**
  String get loadErrorTitle;

  /// Body text of the load-failure recovery screen.
  ///
  /// In en, this message translates to:
  /// **'Night Drop found data saved on this device but couldn’t open it. Two common, fixable causes: Tor hasn’t connected yet, or your system keyring/wallet (e.g. KDE Wallet or GNOME Keyring) is locked — unlock it, then tap Try again. Your existing data is preserved and won’t be overwritten automatically.'**
  String get loadErrorBody;

  /// Android/iOS wording for the same screen. No mention of desktop keyrings; the phone equivalent is not something the user can unlock by hand.
  ///
  /// In en, this message translates to:
  /// **'Night Drop found data saved on this device but couldn’t open it. Usually Tor simply hasn’t connected yet — tap Try again. If it keeps failing, the key that unlocks this data is no longer on the device, which can happen after clearing the app’s storage or moving to a new phone; in that case only a backup can bring it back. Your existing data is preserved and won’t be overwritten automatically.'**
  String get loadErrorBodyMobile;

  /// Button to retry loading the saved session.
  ///
  /// In en, this message translates to:
  /// **'Try again'**
  String get tryAgain;

  /// Button to deliberately continue to onboarding after a load failure.
  ///
  /// In en, this message translates to:
  /// **'Set up a new identity or restore a backup'**
  String get setUpNewIdentity;

  /// Reassurance under the set-up button on the load-failure screen.
  ///
  /// In en, this message translates to:
  /// **'Setting up won’t delete the preserved copy of your old data.'**
  String get loadErrorFootnote;

  /// Header of the Lite/Full backup chooser.
  ///
  /// In en, this message translates to:
  /// **'What to back up'**
  String get backupWhatTitle;

  /// Subtitle under the backup chooser header.
  ///
  /// In en, this message translates to:
  /// **'Lite keeps you reachable without copying your messages.'**
  String get backupWhatSubtitle;

  /// Lite backup option title.
  ///
  /// In en, this message translates to:
  /// **'Lite — identity & contacts only'**
  String get backupLiteTitle;

  /// Lite backup option subtitle.
  ///
  /// In en, this message translates to:
  /// **'No message history or media'**
  String get backupLiteSubtitle;

  /// Full backup option title.
  ///
  /// In en, this message translates to:
  /// **'Full — everything'**
  String get backupFullTitle;

  /// Full backup option subtitle.
  ///
  /// In en, this message translates to:
  /// **'Includes message history and media'**
  String get backupFullSubtitle;

  /// Error when the retyped recovery password is wrong.
  ///
  /// In en, this message translates to:
  /// **'That doesn’t match. Check what you wrote down.'**
  String get passwordMismatch;

  /// Title of the reveal stage of the password dialog.
  ///
  /// In en, this message translates to:
  /// **'Your recovery password'**
  String get yourRecoveryPassword;

  /// Button advancing from reveal to confirm-by-retype.
  ///
  /// In en, this message translates to:
  /// **'I’ve written it down'**
  String get writtenItDown;

  /// Title of the confirm stage of the password dialog.
  ///
  /// In en, this message translates to:
  /// **'Confirm your recovery password'**
  String get confirmRecoveryPassword;

  /// Instruction in the confirm-by-retype stage.
  ///
  /// In en, this message translates to:
  /// **'Type it back to make sure it’s saved correctly. There’s no way to recover it later if it’s wrong.'**
  String get typeItBack;

  /// Button to flip back and re-read the password.
  ///
  /// In en, this message translates to:
  /// **'Show it again'**
  String get showItAgain;

  /// Confirm button in the password dialog.
  ///
  /// In en, this message translates to:
  /// **'Confirm'**
  String get confirm;

  /// Loader message while deriving the backup key.
  ///
  /// In en, this message translates to:
  /// **'Preparing backup…'**
  String get preparingBackup;

  /// Snackbar when backup preparation fails.
  ///
  /// In en, this message translates to:
  /// **'Could not prepare backup: {error}'**
  String couldNotPrepareBackup(String error);

  /// Intro shown above a freshly generated file-backup password.
  ///
  /// In en, this message translates to:
  /// **'Write this down and keep it safe. It is shown only once and is the only way to restore this backup — we cannot recover it for you.'**
  String get backupIntroFile;

  /// Snackbar confirming where the backup file was written.
  ///
  /// In en, this message translates to:
  /// **'Backup saved to {path}'**
  String backupSavedTo(String path);

  /// Snackbar when writing the backup file fails.
  ///
  /// In en, this message translates to:
  /// **'Could not save backup: {error}'**
  String couldNotSaveBackup(String error);

  /// Title of the merge-a-chat-backup dialog.
  ///
  /// In en, this message translates to:
  /// **'Merge a chat backup'**
  String get mergeChatBackupTitle;

  /// Confirm button in the merge-chat-backup dialog.
  ///
  /// In en, this message translates to:
  /// **'Merge'**
  String get merge;

  /// Loader message while a backup is being merged.
  ///
  /// In en, this message translates to:
  /// **'Restoring backup…'**
  String get restoringBackup;

  /// Snackbar confirming a merged chat backup and how many messages were added.
  ///
  /// In en, this message translates to:
  /// **'Merged chat backup — {count, plural, =1{1 message added.} other{{count} messages added.}}'**
  String mergedChatBackup(int count);

  /// Snackbar when merging a chat backup fails.
  ///
  /// In en, this message translates to:
  /// **'Could not merge backup — check the password and file. ({error})'**
  String couldNotMergeBackup(String error);

  /// Generic Save button.
  ///
  /// In en, this message translates to:
  /// **'Save'**
  String get save;

  /// Generic Close button.
  ///
  /// In en, this message translates to:
  /// **'Close'**
  String get close;

  /// Button to snooze the backup reminder.
  ///
  /// In en, this message translates to:
  /// **'Later'**
  String get later;

  /// Short button to open the relay editor from a banner.
  ///
  /// In en, this message translates to:
  /// **'Relays'**
  String get relaysShort;

  /// Tooltip to decline an incoming chat request.
  ///
  /// In en, this message translates to:
  /// **'Decline'**
  String get decline;

  /// Tooltip to approve an incoming chat request.
  ///
  /// In en, this message translates to:
  /// **'Approve'**
  String get approve;

  /// Switch label for enabling a feature.
  ///
  /// In en, this message translates to:
  /// **'Enabled'**
  String get enabled;

  /// Home screen (conversation list) title.
  ///
  /// In en, this message translates to:
  /// **'Chats'**
  String get chats;

  /// Backup menu tooltip / backup-reminder action button.
  ///
  /// In en, this message translates to:
  /// **'Back up'**
  String get backUp;

  /// Backup menu: save a local backup file.
  ///
  /// In en, this message translates to:
  /// **'Save backup file'**
  String get saveBackupFile;

  /// Backup menu: opt-in server backup.
  ///
  /// In en, this message translates to:
  /// **'Back up to server (24h)'**
  String get backUpToServer24h;

  /// Backup menu: merge a single-chat backup.
  ///
  /// In en, this message translates to:
  /// **'Merge a chat backup…'**
  String get mergeChatBackupMenu;

  /// Menu item / dialog title showing the device identity.
  ///
  /// In en, this message translates to:
  /// **'My identity'**
  String get myIdentity;

  /// Menu item opening background-delivery settings.
  ///
  /// In en, this message translates to:
  /// **'Background delivery…'**
  String get backgroundDeliveryMenu;

  /// Menu item opening the relay editor.
  ///
  /// In en, this message translates to:
  /// **'My relays…'**
  String get myRelaysMenu;

  /// Menu item that drops the Tor entry guards and reconnects.
  ///
  /// In en, this message translates to:
  /// **'Reset Tor connection…'**
  String get resetTorMenu;

  /// Title of the reset-Tor confirmation dialog.
  ///
  /// In en, this message translates to:
  /// **'Reset Tor connection?'**
  String get resetTorTitle;

  /// Explains what resetting the Tor connection does, and that nothing is lost.
  ///
  /// In en, this message translates to:
  /// **'If messages keep being held for delivery instead of arriving directly, this device\'s route into Tor may be stuck. This picks a fresh route and reconnects.\n\nYour identity, your address and your chats are not affected. Reconnecting takes a minute or two.'**
  String get resetTorBody;

  /// Confirm button on the reset-Tor dialog.
  ///
  /// In en, this message translates to:
  /// **'Reset'**
  String get resetTorConfirm;

  /// Snackbar shown while the Tor connection is being reset.
  ///
  /// In en, this message translates to:
  /// **'Reconnecting to Tor — this takes a minute or two.'**
  String get resetTorRunning;

  /// Shown in the About dialog. Discloses the one outbound connection the app makes on its own behalf, so it is stated somewhere durable rather than only in a banner nobody is looking at.
  ///
  /// In en, this message translates to:
  /// **'Update checks: this build asks the Night Drop onion site, over Tor, at most once a day whether a newer version exists, and only ever tells you — it never installs anything. Copies installed from F-Droid don’t do this at all, because F-Droid updates them.'**
  String get aboutUpdateChecks;

  /// Menu item opening the about dialog (app name, version, license).
  ///
  /// In en, this message translates to:
  /// **'About Night Drop'**
  String get aboutMenu;

  /// Menu item that disconnects from Tor and closes the app, keeping the identity (issue #15).
  ///
  /// In en, this message translates to:
  /// **'Exit'**
  String get exitMenu;

  /// Title of the Exit confirmation.
  ///
  /// In en, this message translates to:
  /// **'Exit Night Drop?'**
  String get exitTitle;

  /// Body of the Exit confirmation: what stops, and what is kept.
  ///
  /// In en, this message translates to:
  /// **'Disconnects from Tor and closes the app. Your identity and chats stay on this device.\n\nMessages sent to you while it’s closed wait on the relay for up to 24 hours. Open Night Drop within that time to receive them — after that they expire, and the sender sees them as not delivered.'**
  String get exitBody;

  /// Confirm button of the Exit dialog.
  ///
  /// In en, this message translates to:
  /// **'Exit'**
  String get exitConfirm;

  /// Shown while Exit shuts Tor down, which can take a few seconds.
  ///
  /// In en, this message translates to:
  /// **'Disconnecting from Tor…'**
  String get exitDisconnecting;

  /// Menu item to log out and delete the identity.
  ///
  /// In en, this message translates to:
  /// **'Log out / delete identity'**
  String get logoutDeleteMenu;

  /// Empty-state text on the conversation list.
  ///
  /// In en, this message translates to:
  /// **'No chats yet.\nTap “New chat” to pair by QR or short code.'**
  String get noChatsYet;

  /// Empty-state text on the conversation list on direct only, where short codes are unavailable (they need a relay).
  ///
  /// In en, this message translates to:
  /// **'No chats yet.\nTap “New chat” to pair by QR code.'**
  String get noChatsYetQrOnly;

  /// Chat subtitle when remote storage is on.
  ///
  /// In en, this message translates to:
  /// **'Stored on server (24h)'**
  String get storedOnServer24h;

  /// Chat subtitle when messages are local-only.
  ///
  /// In en, this message translates to:
  /// **'Stored on this device'**
  String get storedOnThisDevice;

  /// Banner while the onion descriptor is still publishing. Deliberately gives no numeric range: it said 1-3 min and an F-Droid reviewer measured ~5 on a Redmi Note 8T, matching our own device logs of 4 and 6 minutes. The wait depends on the network, so a number we cannot keep is worse than none.
  ///
  /// In en, this message translates to:
  /// **'Publishing your address to Tor. This can take several minutes. Others can’t pair with you until it finishes — keep the app open.'**
  String get publishingAddressTor;

  /// Relay-health banner when a single relay is unreachable (trailing space intentional).
  ///
  /// In en, this message translates to:
  /// **'Your relay {names} looks offline. '**
  String relayOfflineOne(String names);

  /// Relay-health banner when multiple relays are unreachable (trailing space intentional).
  ///
  /// In en, this message translates to:
  /// **'Your relays {names} look offline. '**
  String relayOfflineMany(String names);

  /// Relay-health advice when a backup relay is still up.
  ///
  /// In en, this message translates to:
  /// **'Contacts can still reach you through your other relay for now.'**
  String get relayAdviceHasBackup;

  /// Relay-health advice when no backup relay is up.
  ///
  /// In en, this message translates to:
  /// **'Add a backup relay so people can still reach you while it’s down.'**
  String get relayAdviceNoBackup;

  /// Body of the dismissible backup-reminder banner.
  ///
  /// In en, this message translates to:
  /// **'Back up your identity so you don’t lose your chats. There’s no account to recover from — if you lose this device without a backup, it’s gone.'**
  String get backupReminderBody;

  /// Home-screen notice shown in the last 0.1.x releases: 0.2 changes the protocol, so 0.1.x and 0.2 apps cannot message each other.
  ///
  /// In en, this message translates to:
  /// **'Night Drop 0.2 is coming, and it won’t be able to message 0.1 apps. Update when it arrives, and ask your contacts to update too.'**
  String get protocolBreakNotice;

  /// Button that dismisses an informational notice.
  ///
  /// In en, this message translates to:
  /// **'Got it'**
  String get gotIt;

  /// Body of the update-available banner. {version} is the newer release, e.g. 0.1.18.
  ///
  /// In en, this message translates to:
  /// **'Version {version} is available. This build is out of date.'**
  String updateAvailableBody(String version);

  /// The app's own version in the About dialog. {version} is e.g. 0.1.29.
  ///
  /// In en, this message translates to:
  /// **'Version {version}'**
  String aboutVersion(String version);

  /// Shown while the on-demand update check runs.
  ///
  /// In en, this message translates to:
  /// **'Checking for updates…'**
  String get updateChecking;

  /// Shown when the on-demand update check could not reach the onion site — deliberately distinct from being up to date.
  ///
  /// In en, this message translates to:
  /// **'Could not reach the update site. Try again later.'**
  String get updateCheckFailed;

  /// Shown when the on-demand check finds no newer release.
  ///
  /// In en, this message translates to:
  /// **'Night Drop is up to date.'**
  String get updateUpToDate;

  /// Button that starts the verified download of a newer build.
  ///
  /// In en, this message translates to:
  /// **'Download'**
  String get updateDownload;

  /// Button that hides the update banner until a newer version exists.
  ///
  /// In en, this message translates to:
  /// **'Hide'**
  String get updateHide;

  /// Shown when the system (not the user) ended the foreground service, e.g. a foreground-service-type time budget running out. The point is that the user could not otherwise tell.
  ///
  /// In en, this message translates to:
  /// **'Android stopped background delivery, so messages may not have arrived. Reopen Night Drop to start it again.'**
  String get backgroundStoppedBySystem;

  /// One-line form of peerCapturesSilentBanner; tapping it shows the full text.
  ///
  /// In en, this message translates to:
  /// **'Their screenshots aren\'t reported to you'**
  String get peerCapturesSilentSummary;

  /// Shown to the sender when the peer's device (Android below 14) cannot report screen captures, so the peer's silence carries no information.
  ///
  /// In en, this message translates to:
  /// **'This person\'s device can\'t tell them about screenshots, so it won\'t tell you either. If they capture what you send, you won\'t hear about it.'**
  String get peerCapturesSilentBanner;

  /// One-line form of peerOnOldVersionBanner; tapping it shows the full text.
  ///
  /// In en, this message translates to:
  /// **'They\'re on an older version — ask them to update'**
  String get peerOnOldVersionSummary;

  /// Shown when the peer's app predates private mailbox addressing. The peer's older app cannot display this, so the text asks the reader to tell them.
  ///
  /// In en, this message translates to:
  /// **'This person is using an older version of Night Drop, so messages that wait on a relay for either of you are addressed less privately. Their app can\'t tell them — ask them to update. Support for their version will end in a future release.'**
  String get peerOnOldVersionBanner;

  /// Shown while the update APK is downloading.
  ///
  /// In en, this message translates to:
  /// **'Downloading over Tor…'**
  String get updateDownloading;

  /// Shown while the update APK is downloading, with a percentage once one is known. The placeholder already includes its leading space and is empty until the first bytes arrive, so the string must read correctly without it.
  ///
  /// In en, this message translates to:
  /// **'Downloading over Tor…{percent}'**
  String updateDownloadingPercent(String percent);

  /// Shown when the update APK finished downloading and its hash matched.
  ///
  /// In en, this message translates to:
  /// **'Downloaded and verified. Open it to install.'**
  String get updateDownloaded;

  /// Shown when the update download failed or its hash did not match.
  ///
  /// In en, this message translates to:
  /// **'Could not download the update.'**
  String get updateFailed;

  /// Menu item that checks for and downloads an update.
  ///
  /// In en, this message translates to:
  /// **'Update app'**
  String get updateApp;

  /// Button on the update banner; opens instructions for updating.
  ///
  /// In en, this message translates to:
  /// **'How'**
  String get updateHow;

  /// Title of the delete-chat confirmation.
  ///
  /// In en, this message translates to:
  /// **'Delete this chat?'**
  String get deleteThisChat;

  /// Body of the delete-chat confirmation.
  ///
  /// In en, this message translates to:
  /// **'This permanently deletes your conversation with {name}. {name} will be told the chat was deleted. To talk again you will both need to create a new chat.'**
  String deleteChatBody(String name);

  /// Confirm button that deletes a chat.
  ///
  /// In en, this message translates to:
  /// **'Delete chat'**
  String get deleteChat;

  /// Title of the background-delivery settings dialog.
  ///
  /// In en, this message translates to:
  /// **'Background delivery'**
  String get backgroundDelivery;

  /// Explanation in the background-delivery dialog.
  ///
  /// In en, this message translates to:
  /// **'Keep receiving messages while Night Drop is closed. It checks over Tor and shows a permanent notification. No Google push service. To tuck the notification away, long-press it and choose “Minimize” (or turn off “Background delivery status”); messages keep arriving.\n\nTo keep receiving right after a restart, set Night Drop’s battery use to “Unrestricted” in Android’s app settings. On “Optimized”, it can take about 15 minutes.'**
  String get backgroundDeliveryBody;

  /// Title of the one setup page shown before an identity is created.
  ///
  /// In en, this message translates to:
  /// **'Before you start'**
  String get setupTitle;

  /// Intro text on the setup page.
  ///
  /// In en, this message translates to:
  /// **'These change what Night Drop does on the network. Each option says what it costs.'**
  String get setupIntro;

  /// Switch label on the setup page for cover traffic (dummy mail to yourself).
  ///
  /// In en, this message translates to:
  /// **'Cover traffic'**
  String get setupCoverTitle;

  /// Note at the bottom of the setup page.
  ///
  /// In en, this message translates to:
  /// **'You can change these later in the menu.'**
  String get setupChangeLater;

  /// Title of the background-delivery offer shown during onboarding.
  ///
  /// In en, this message translates to:
  /// **'Receive messages in the background?'**
  String get onboardingBackgroundTitle;

  /// Explanation of what background delivery does, its privacy properties and its costs, shown during onboarding.
  ///
  /// In en, this message translates to:
  /// **'Without this, Android pauses Night Drop when it’s off screen, and messages arrive only when you open it.\n\nWhen on, Night Drop keeps checking over Tor and shows a permanent notification. No Google push service. Uses some battery. To tuck the notification away, long-press it and choose “Minimize” (or turn off “Background delivery status”); messages keep arriving.\n\nTo keep receiving right after a restart, set Night Drop’s battery use to “Unrestricted” in Android’s app settings. On “Optimized”, it can take about 15 minutes.'**
  String get onboardingBackgroundBody;

  /// Button accepting background delivery during onboarding.
  ///
  /// In en, this message translates to:
  /// **'Turn on'**
  String get onboardingBackgroundEnable;

  /// Button declining background delivery during onboarding.
  ///
  /// In en, this message translates to:
  /// **'Not now'**
  String get onboardingBackgroundSkip;

  /// Snackbar when notification permission is missing.
  ///
  /// In en, this message translates to:
  /// **'Notification permission is required for background delivery.'**
  String get notificationPermissionRequired;

  /// Snackbar confirming background delivery was enabled.
  ///
  /// In en, this message translates to:
  /// **'Background delivery on'**
  String get backgroundDeliveryOn;

  /// Snackbar confirming background delivery was disabled.
  ///
  /// In en, this message translates to:
  /// **'Background delivery off'**
  String get backgroundDeliveryOff;

  /// Title of the relay editor dialog.
  ///
  /// In en, this message translates to:
  /// **'My relays'**
  String get myRelays;

  /// Snackbar when saving relays fails.
  ///
  /// In en, this message translates to:
  /// **'Could not save relays: {error}'**
  String couldNotSaveRelays(String error);

  /// Loader while uploading a server backup.
  ///
  /// In en, this message translates to:
  /// **'Backing up to server…'**
  String get backingUpToServer;

  /// Snackbar when a server backup fails.
  ///
  /// In en, this message translates to:
  /// **'Could not back up to server: {error}'**
  String couldNotBackUpToServer(String error);

  /// Intro above the server-backup password.
  ///
  /// In en, this message translates to:
  /// **'An encrypted copy is now stored on the server. Write this password down — it is shown only once, never leaves your device, and is the ONLY way to restore. We cannot recover it for you.'**
  String get serverBackupIntro;

  /// Footer noting when the server copy expires.
  ///
  /// In en, this message translates to:
  /// **'The server copy is deleted at {expiry}. After that, only your recorded password + a local or file backup can restore this identity.'**
  String serverBackupFooter(String expiry);

  /// Explanation in the My identity dialog.
  ///
  /// In en, this message translates to:
  /// **'This is your anonymous, device-held identity — no name, no account. Others see this id once you pair.'**
  String get myIdentityBody;

  /// Title of the logout confirmation.
  ///
  /// In en, this message translates to:
  /// **'Log out & delete this identity?'**
  String get logoutTitle;

  /// Body of the logout confirmation.
  ///
  /// In en, this message translates to:
  /// **'This permanently erases your identity, all contacts, and all messages from this device. There is no undo.\n\n• The ONLY way back into these chats is restoring a backup — so make one first if you haven’t.\n• Everyone you chat with will have to pair with you again.\n\nContinue?'**
  String get logoutBody;

  /// Confirm button for logout + delete.
  ///
  /// In en, this message translates to:
  /// **'Log out & delete'**
  String get logoutDelete;

  /// Snackbar after logout listing contacts that may not have been told.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{1 contact may not have been notified that this chat is gone.} other{{count} contacts may not have been notified that these chats are gone.}}'**
  String logoutNotNotified(int count);

  /// Title of an incoming chat-request tile.
  ///
  /// In en, this message translates to:
  /// **'Chat request'**
  String get chatRequest;

  /// Chat-request subtitle while approval is in flight.
  ///
  /// In en, this message translates to:
  /// **'Approving over Tor — this can take a few seconds…'**
  String get approvingOverTor;

  /// Chat-request subtitle showing the sender's short id.
  ///
  /// In en, this message translates to:
  /// **'From {id}. Approve to start chatting.'**
  String requestFrom(String id);

  /// Snackbar when approving a chat request fails.
  ///
  /// In en, this message translates to:
  /// **'Could not approve: {error}'**
  String couldNotApprove(String error);

  /// Snackbar when declining a chat request fails.
  ///
  /// In en, this message translates to:
  /// **'Could not decline: {error}'**
  String couldNotDecline(String error);

  /// Generic Delete confirm button.
  ///
  /// In en, this message translates to:
  /// **'Delete'**
  String get delete;

  /// Edit action in the message long-press menu.
  ///
  /// In en, this message translates to:
  /// **'Edit'**
  String get edit;

  /// Short call-to-action to open safety-number verification.
  ///
  /// In en, this message translates to:
  /// **'Verify'**
  String get verify;

  /// Label for a video attachment tile.
  ///
  /// In en, this message translates to:
  /// **'Video'**
  String get video;

  /// Label for a generic file attachment tile.
  ///
  /// In en, this message translates to:
  /// **'File'**
  String get file;

  /// Tooltip for the chat overflow menu.
  ///
  /// In en, this message translates to:
  /// **'More'**
  String get more;

  /// Send refused while short-code pairing is still waiting on the other person to accept.
  ///
  /// In en, this message translates to:
  /// **'They haven’t accepted the chat yet. Your message hasn’t been sent — it’s still in the box, so you can send it once they accept.'**
  String get cantSendUntilAccepted;

  /// Send failure when the peer is offline and no relay accepted the message.
  ///
  /// In en, this message translates to:
  /// **'Couldn’t send — they’re offline and no relay could hold the message. Check your relay settings and try again; your message is still here.'**
  String get couldntSendOffline;

  /// Generic send-failure snackbar.
  ///
  /// In en, this message translates to:
  /// **'Couldn’t send: {error}'**
  String couldntSend(String error);

  /// Snackbar when a picked attachment exceeds the size limit.
  ///
  /// In en, this message translates to:
  /// **'That file is {size} — the limit is {limit}.'**
  String fileTooLarge(String size, String limit);

  /// Snackbar when Paste is tapped with an empty clipboard.
  ///
  /// In en, this message translates to:
  /// **'Nothing to paste.'**
  String get nothingToPaste;

  /// Snackbar when a chosen picture is in a format neither the app nor the system can decode. Night Drop never sends a picture it could not clean of its metadata.
  ///
  /// In en, this message translates to:
  /// **'This picture couldn\'t be read, so it wasn\'t sent. Save it as JPEG or PNG and try again.'**
  String get imageUnreadable;

  /// Banner shown after the device switches networks, while Tor rebuilds its connections.
  ///
  /// In en, this message translates to:
  /// **'Network changed — reconnecting to Tor. Messages and new chats can take a minute or two.'**
  String get networkChangedReconnecting;

  /// Menu item for the opt-in setting that blocks screenshots and screen recording of the app (Android).
  ///
  /// In en, this message translates to:
  /// **'Block screenshots'**
  String get blockCaptureMenu;

  /// Title of the Block screenshots dialog.
  ///
  /// In en, this message translates to:
  /// **'Block screenshots'**
  String get blockCaptureTitle;

  /// Explains the Block screenshots setting and its limit.
  ///
  /// In en, this message translates to:
  /// **'When on, nobody can take a screenshot of Night Drop, record its screen or cast it on this phone, including apps you installed. Recent apps never show its contents either way. Your contacts\' Night Drop blocks them too in their chat with you, where their device can.\n\nOn by default. Turn it off to allow screenshots: your contacts are then told when you screenshot a chat or record your screen with it open (Android 15 and later). A photo of the screen gets past any block.'**
  String get blockCaptureBody;

  /// Chat menu: override the contact's request to block screenshots, on this phone.
  ///
  /// In en, this message translates to:
  /// **'Allow screenshots in this chat'**
  String get captureOverrideOnMenu;

  /// Chat menu: honour the contact's request again.
  ///
  /// In en, this message translates to:
  /// **'Block screenshots in this chat again'**
  String get captureOverrideOffMenu;

  /// Title of the confirmation before overriding the contact's block.
  ///
  /// In en, this message translates to:
  /// **'Allow screenshots here?'**
  String get captureOverrideTitle;

  /// Body of the confirmation before overriding the contact's block.
  ///
  /// In en, this message translates to:
  /// **'This person asked that this chat not be captured. You can still allow screenshots and screen recording of it on your phone — to keep evidence of abuse, for instance. They will be told that you did, and told about each screenshot you take.'**
  String get captureOverrideBody;

  /// Confirm button: override and notify the contact.
  ///
  /// In en, this message translates to:
  /// **'Allow and tell them'**
  String get captureOverrideConfirm;

  /// One line when the user overrode the contact's block in this chat.
  ///
  /// In en, this message translates to:
  /// **'You allow screenshots here; they were told'**
  String get captureOverrideSummary;

  /// Full text behind captureOverrideSummary.
  ///
  /// In en, this message translates to:
  /// **'This person blocks screenshots, but you allowed them in this chat on your phone. They were told, and they are told about each screenshot you take. Block them again from the chat menu.'**
  String get captureOverrideBanner;

  /// One line: the contact has Block screenshots on, so this app blocks captures in their chat.
  ///
  /// In en, this message translates to:
  /// **'They block screenshots of this chat'**
  String get peerBlocksCaptureSummary;

  /// Full text behind peerBlocksCaptureSummary.
  ///
  /// In en, this message translates to:
  /// **'This person turned on Block screenshots, so Night Drop blocks screenshots, screen recording and casting here too while this chat is open. A photo of the screen still gets past it.'**
  String get peerBlocksCaptureBanner;

  /// One line, on a device that cannot block captures (desktop): the contact has Block screenshots on.
  ///
  /// In en, this message translates to:
  /// **'They asked that this chat not be captured'**
  String get peerBlocksCaptureUnenforcedSummary;

  /// Full text behind peerBlocksCaptureUnenforcedSummary.
  ///
  /// In en, this message translates to:
  /// **'This person turned on Block screenshots. This device can\'t block screenshots or screen recording, so it\'s up to you to respect that: don\'t capture this chat.'**
  String get peerBlocksCaptureUnenforcedBanner;

  /// Snackbar after turning Block screenshots on.
  ///
  /// In en, this message translates to:
  /// **'Screenshots are blocked.'**
  String get blockCaptureOn;

  /// Snackbar after turning Block screenshots off.
  ///
  /// In en, this message translates to:
  /// **'Screenshots are allowed again.'**
  String get blockCaptureOff;

  /// Snackbar when sending a media attachment fails.
  ///
  /// In en, this message translates to:
  /// **'Could not send attachment: {error}'**
  String couldNotSendAttachment(String error);

  /// Title of the edit-message dialog.
  ///
  /// In en, this message translates to:
  /// **'Edit message'**
  String get editMessageTitle;

  /// Snackbar when editing a message fails.
  ///
  /// In en, this message translates to:
  /// **'Could not edit: {error}'**
  String couldNotEdit(String error);

  /// Unsend action in the message long-press menu.
  ///
  /// In en, this message translates to:
  /// **'Delete for everyone'**
  String get deleteForEveryone;

  /// Title of the unsend confirmation.
  ///
  /// In en, this message translates to:
  /// **'Delete for everyone?'**
  String get deleteForEveryoneTitle;

  /// Body of the unsend confirmation.
  ///
  /// In en, this message translates to:
  /// **'This removes the message from both devices. If it hasn’t been delivered yet, the other person never receives it.'**
  String get unsendBody;

  /// Snackbar when unsending a message fails.
  ///
  /// In en, this message translates to:
  /// **'Could not delete: {error}'**
  String couldNotDelete(String error);

  /// Title of the rename-self dialog.
  ///
  /// In en, this message translates to:
  /// **'Your name in this chat'**
  String get yourNameInChat;

  /// Disappearing-messages option: off.
  ///
  /// In en, this message translates to:
  /// **'Off'**
  String get disappearingOff;

  /// Disappearing-messages option: 1 hour.
  ///
  /// In en, this message translates to:
  /// **'1 hour'**
  String get disappearing1Hour;

  /// Disappearing-messages option: 1 day.
  ///
  /// In en, this message translates to:
  /// **'1 day'**
  String get disappearing1Day;

  /// Disappearing-messages option: 1 week.
  ///
  /// In en, this message translates to:
  /// **'1 week'**
  String get disappearing1Week;

  /// Header of the disappearing-messages chooser.
  ///
  /// In en, this message translates to:
  /// **'Disappearing messages'**
  String get disappearingMessages;

  /// Subtitle of the disappearing-messages chooser.
  ///
  /// In en, this message translates to:
  /// **'New messages are deleted on both devices after this long. Messages already here stay.'**
  String get disappearingSubtitle;

  /// Snackbar when setting the disappearing timer fails.
  ///
  /// In en, this message translates to:
  /// **'Could not set timer: {error}'**
  String couldNotSetTimer(String error);

  /// Tooltip for the verify-safety-number app-bar action.
  ///
  /// In en, this message translates to:
  /// **'Verify safety number'**
  String get verifySafetyNumber;

  /// Tooltip for the rename-self app-bar action.
  ///
  /// In en, this message translates to:
  /// **'Rename yourself in this chat'**
  String get renameYourselfTooltip;

  /// Tooltip when server storage is on.
  ///
  /// In en, this message translates to:
  /// **'Stored on server (24h) — tap to disable'**
  String get storedServerTooltipOn;

  /// Tooltip when server storage is off.
  ///
  /// In en, this message translates to:
  /// **'Stored on this device — tap to enable 24h server storage'**
  String get storedServerTooltipOff;

  /// Tooltip when a disappearing timer is set.
  ///
  /// In en, this message translates to:
  /// **'Disappearing messages: {label} — tap to change'**
  String disappearingTooltipOn(String label);

  /// Tooltip when no disappearing timer is set.
  ///
  /// In en, this message translates to:
  /// **'Disappearing messages: off — tap to set'**
  String get disappearingTooltipOff;

  /// Tooltip for the delete-chat app-bar action.
  ///
  /// In en, this message translates to:
  /// **'Delete this chat'**
  String get deleteThisChatTooltip;

  /// Overflow-menu item to back up a single chat.
  ///
  /// In en, this message translates to:
  /// **'Back up this chat…'**
  String get backUpThisChat;

  /// Empty-state text for a new chat with no messages.
  ///
  /// In en, this message translates to:
  /// **'Say hi 👻'**
  String get sayHi;

  /// Remote-storage warning banner (healthy).
  ///
  /// In en, this message translates to:
  /// **'Messages in this chat are stored on the server (encrypted) for up to 24 hours so both devices use less space.'**
  String get remoteStorageBannerHealthy;

  /// Remote-storage warning banner (relay unreachable).
  ///
  /// In en, this message translates to:
  /// **'Server storage is on, but the relay couldn’t be reached — recent messages were delivered but not stored on the server. They’ll be stored again once a relay is reachable.'**
  String get remoteStorageBannerUnhealthy;

  /// Body of the unverified-chat nudge banner.
  ///
  /// In en, this message translates to:
  /// **'Not verified — confirm this contact’s safety number to be sure no one’s in the middle.'**
  String get unverifiedBannerBody;

  /// Transparency banner: the peer keeps a Full backup.
  ///
  /// In en, this message translates to:
  /// **'The other person keeps a backup of this chat, so your messages may persist in their backup.'**
  String get peerBackupBanner;

  /// Banner shown to the joiner until the chat is approved.
  ///
  /// In en, this message translates to:
  /// **'Waiting for the other person to accept the chat. Messages you send won’t be delivered until they do.'**
  String get awaitingApprovalBanner;

  /// Placeholder text for an unsent/tombstoned message.
  ///
  /// In en, this message translates to:
  /// **'Message deleted'**
  String get messageDeleted;

  /// Marker left in place of a burn message that was never opened within 24 hours; its content is gone.
  ///
  /// In en, this message translates to:
  /// **'Burn message expired unopened'**
  String get burnExpiredUnopened;

  /// Small tag marking an edited message.
  ///
  /// In en, this message translates to:
  /// **'edited'**
  String get editedTag;

  /// Delivery status: queued on a relay.
  ///
  /// In en, this message translates to:
  /// **'Held for delivery'**
  String get deliveryHeld;

  /// Delivery status: held on this device because no relay connects the two people and the contact was offline. It is sent directly as soon as they are online.
  ///
  /// In en, this message translates to:
  /// **'Waiting for {name} to come online'**
  String deliveryWaiting(String name);

  /// Delivery status: expired unread.
  ///
  /// In en, this message translates to:
  /// **'Not delivered (expired)'**
  String get deliveryExpired;

  /// Delivery status: delivered.
  ///
  /// In en, this message translates to:
  /// **'Delivered'**
  String get deliveryDelivered;

  /// Delivery status: handed to the peer's onion service, which is not the same as their device having the message. Deliberately not the word 'Sent': that reads as a completed step, and a message can still be lost at this point. It resolves to Delivered when their device confirms this exact message, or to 'Held for delivery' when it goes to a relay instead.
  ///
  /// In en, this message translates to:
  /// **'Not confirmed yet'**
  String get deliverySent;

  /// Shown when an image attachment can't be decoded.
  ///
  /// In en, this message translates to:
  /// **'Image unavailable'**
  String get imageUnavailable;

  /// Caption under an image attachment.
  ///
  /// In en, this message translates to:
  /// **'Image • {size}'**
  String mediaImage(String size);

  /// Caption under an image attachment while it uploads.
  ///
  /// In en, this message translates to:
  /// **'Image • {size} • sending…'**
  String mediaImageSending(String size);

  /// Snackbar when the system player can't open a file.
  ///
  /// In en, this message translates to:
  /// **'Could not open the file: {error}'**
  String couldNotOpenFile(String error);

  /// Snackbar when opening an attachment throws.
  ///
  /// In en, this message translates to:
  /// **'Could not open: {error}'**
  String couldNotOpen(String error);

  /// Video tile status: uploading.
  ///
  /// In en, this message translates to:
  /// **'sending…'**
  String get mediaStatusSending;

  /// Video tile status: downloading.
  ///
  /// In en, this message translates to:
  /// **'incoming…'**
  String get mediaStatusIncoming;

  /// Video tile status: ready to play.
  ///
  /// In en, this message translates to:
  /// **'tap to play'**
  String get mediaStatusTapToPlay;

  /// Tooltip for the composer attach button.
  ///
  /// In en, this message translates to:
  /// **'Attach image or video'**
  String get attachImageOrVideo;

  /// Tooltip for the composer paste button.
  ///
  /// In en, this message translates to:
  /// **'Paste text'**
  String get pasteText;

  /// Placeholder text in the message composer.
  ///
  /// In en, this message translates to:
  /// **'Message'**
  String get messageHint;

  /// No description provided for @appLockMenu.
  ///
  /// In en, this message translates to:
  /// **'App lock'**
  String get appLockMenu;

  /// No description provided for @lockedTitle.
  ///
  /// In en, this message translates to:
  /// **'Night Drop is locked'**
  String get lockedTitle;

  /// No description provided for @lockedBody.
  ///
  /// In en, this message translates to:
  /// **'Enter your PIN or passphrase to open your messages.'**
  String get lockedBody;

  /// No description provided for @lockedField.
  ///
  /// In en, this message translates to:
  /// **'PIN or passphrase'**
  String get lockedField;

  /// No description provided for @lockedFailed.
  ///
  /// In en, this message translates to:
  /// **'That didn\'t unlock it. Try again.'**
  String get lockedFailed;

  /// Lock screen, after five or more wrong PINs/passphrases in a row: how long until another try is accepted. The wait survives closing the app.
  ///
  /// In en, this message translates to:
  /// **'Too many wrong tries. Try again in {time}.'**
  String lockedWait(String time);

  /// No description provided for @lockedUnlock.
  ///
  /// In en, this message translates to:
  /// **'Unlock'**
  String get lockedUnlock;

  /// No description provided for @lockedNoRecovery.
  ///
  /// In en, this message translates to:
  /// **'There is no way to recover this. Without it, the messages on this device stay encrypted for good.'**
  String get lockedNoRecovery;

  /// No description provided for @appLockOffTitle.
  ///
  /// In en, this message translates to:
  /// **'App lock is off'**
  String get appLockOffTitle;

  /// No description provided for @appLockOffBody.
  ///
  /// In en, this message translates to:
  /// **'Your messages are unlocked whenever the app opens. Turn on a lock to require a PIN or passphrase first.'**
  String get appLockOffBody;

  /// No description provided for @appLockOnTitle.
  ///
  /// In en, this message translates to:
  /// **'App lock is on'**
  String get appLockOnTitle;

  /// No description provided for @appLockOnBody.
  ///
  /// In en, this message translates to:
  /// **'Your messages need your PIN or passphrase before they can be opened.'**
  String get appLockOnBody;

  /// No description provided for @appLockChoosePin.
  ///
  /// In en, this message translates to:
  /// **'Use a PIN'**
  String get appLockChoosePin;

  /// No description provided for @appLockChoosePinBody.
  ///
  /// In en, this message translates to:
  /// **'Quick to type. Stops someone who picks up your unlocked phone — but a short PIN can be broken by someone who copies the data off your device.'**
  String get appLockChoosePinBody;

  /// No description provided for @appLockChoosePassphrase.
  ///
  /// In en, this message translates to:
  /// **'Use a passphrase'**
  String get appLockChoosePassphrase;

  /// No description provided for @appLockChoosePassphraseBody.
  ///
  /// In en, this message translates to:
  /// **'Longer to type. The only option that also protects your messages if someone copies the data off your device.'**
  String get appLockChoosePassphraseBody;

  /// No description provided for @appLockEnable.
  ///
  /// In en, this message translates to:
  /// **'Turn on app lock'**
  String get appLockEnable;

  /// No description provided for @appLockDisable.
  ///
  /// In en, this message translates to:
  /// **'Turn off app lock'**
  String get appLockDisable;

  /// No description provided for @appLockNewSecret.
  ///
  /// In en, this message translates to:
  /// **'New PIN or passphrase'**
  String get appLockNewSecret;

  /// No description provided for @appLockConfirmSecret.
  ///
  /// In en, this message translates to:
  /// **'Enter it again'**
  String get appLockConfirmSecret;

  /// No description provided for @appLockMismatch.
  ///
  /// In en, this message translates to:
  /// **'Those don\'t match.'**
  String get appLockMismatch;

  /// No description provided for @appLockTooShortPin.
  ///
  /// In en, this message translates to:
  /// **'Use at least 6 digits.'**
  String get appLockTooShortPin;

  /// No description provided for @appLockTooShortPassphrase.
  ///
  /// In en, this message translates to:
  /// **'Use at least 12 characters.'**
  String get appLockTooShortPassphrase;

  /// No description provided for @appLockWarnNoRecovery.
  ///
  /// In en, this message translates to:
  /// **'If you forget this, your messages are gone. There is no reset and no recovery — not by us, not by anyone.'**
  String get appLockWarnNoRecovery;

  /// No description provided for @appLockIUnderstand.
  ///
  /// In en, this message translates to:
  /// **'I understand — turn it on'**
  String get appLockIUnderstand;

  /// No description provided for @appLockEnabled.
  ///
  /// In en, this message translates to:
  /// **'App lock is on.'**
  String get appLockEnabled;

  /// No description provided for @appLockDisabled.
  ///
  /// In en, this message translates to:
  /// **'App lock is off.'**
  String get appLockDisabled;

  /// No description provided for @appLockWrongSecret.
  ///
  /// In en, this message translates to:
  /// **'That didn\'t match. Nothing was changed.'**
  String get appLockWrongSecret;

  /// No description provided for @appLockCurrentSecret.
  ///
  /// In en, this message translates to:
  /// **'Current PIN or passphrase'**
  String get appLockCurrentSecret;

  /// No description provided for @appLockBgNote.
  ///
  /// In en, this message translates to:
  /// **'Background delivery is on, so your key stays in memory while the app is locked — otherwise messages couldn\'t arrive. Turn background delivery off if you\'d rather the key be forgotten each time you lock.'**
  String get appLockBgNote;

  /// No description provided for @duressSet.
  ///
  /// In en, this message translates to:
  /// **'Set a wipe code'**
  String get duressSet;

  /// No description provided for @duressReplace.
  ///
  /// In en, this message translates to:
  /// **'Replace it'**
  String get duressReplace;

  /// No description provided for @duressRemove.
  ///
  /// In en, this message translates to:
  /// **'Remove the wipe code'**
  String get duressRemove;

  /// No description provided for @duressSkip.
  ///
  /// In en, this message translates to:
  /// **'Not now'**
  String get duressSkip;

  /// No description provided for @duressMenu.
  ///
  /// In en, this message translates to:
  /// **'Wipe code'**
  String get duressMenu;

  /// No description provided for @duressOnBody.
  ///
  /// In en, this message translates to:
  /// **'A wipe code is set. Entering it at the lock screen deletes your identity and messages instead of opening them.'**
  String get duressOnBody;

  /// No description provided for @duressOffBody.
  ///
  /// In en, this message translates to:
  /// **'No wipe code is set. You can add a second code that deletes your identity and messages instead of opening them.'**
  String get duressOffBody;

  /// No description provided for @duressOfferBody.
  ///
  /// In en, this message translates to:
  /// **'You can also set a second code that wipes instead of unlocking. Entering it at the lock screen deletes your identity and messages and leaves the app looking freshly installed. You can add one later under “Wipe code”.'**
  String get duressOfferBody;

  /// No description provided for @duressNeedsLock.
  ///
  /// In en, this message translates to:
  /// **'Set an app lock first. The wipe code is a second code for the same lock screen.'**
  String get duressNeedsLock;

  /// No description provided for @duressTitle.
  ///
  /// In en, this message translates to:
  /// **'Wipe code'**
  String get duressTitle;

  /// No description provided for @duressBody.
  ///
  /// In en, this message translates to:
  /// **'A second code for the lock screen. Entering it does not open your messages — it deletes them, along with your identity, and leaves the app looking like it was just installed.'**
  String get duressBody;

  /// No description provided for @duressNew.
  ///
  /// In en, this message translates to:
  /// **'New wipe code'**
  String get duressNew;

  /// No description provided for @duressConfirm.
  ///
  /// In en, this message translates to:
  /// **'Enter it again'**
  String get duressConfirm;

  /// No description provided for @duressSame.
  ///
  /// In en, this message translates to:
  /// **'This must be different from the code that opens your messages.'**
  String get duressSame;

  /// No description provided for @duressWarnTitle.
  ///
  /// In en, this message translates to:
  /// **'Before you set this'**
  String get duressWarnTitle;

  /// No description provided for @duressWarnBody.
  ///
  /// In en, this message translates to:
  /// **'You cannot practise this. Entering the wipe code destroys everything on this device, every time, with no confirmation and no undo — that is what makes it work under pressure.\n\nNight Drop checks that the code works when it sets it. You will not be reminded that a wipe code is set: showing it anywhere would tell whoever picks up your phone that it exists.\n\nYour contacts are not told anything. The wipe code deliberately gives the app no way to read your messages, which also leaves it no way to send on your behalf — so your contacts simply stop hearing from you. Agree on another way to reach each other before you need it.'**
  String get duressWarnBody;

  /// No description provided for @duressUnderstand.
  ///
  /// In en, this message translates to:
  /// **'I understand — set it'**
  String get duressUnderstand;

  /// No description provided for @duressDone.
  ///
  /// In en, this message translates to:
  /// **'Wipe code set.'**
  String get duressDone;

  /// Shown when the user backs out of the wipe-code warning; nothing was set or changed.
  ///
  /// In en, this message translates to:
  /// **'Cancelled — your wipe code was not changed.'**
  String get duressCancelled;

  /// No description provided for @duressCleared.
  ///
  /// In en, this message translates to:
  /// **'Wipe code removed.'**
  String get duressCleared;

  /// No description provided for @duressCurrentSecret.
  ///
  /// In en, this message translates to:
  /// **'Your normal PIN or passphrase'**
  String get duressCurrentSecret;

  /// Onboarding link to the bridge editor, for users whose network blocks Tor and who therefore cannot create an identity at all until a bridge is configured.
  ///
  /// In en, this message translates to:
  /// **'Tor blocked? Set up a bridge'**
  String get onboardingTorBlocked;

  /// No description provided for @bridgesMenu.
  ///
  /// In en, this message translates to:
  /// **'Tor bridges'**
  String get bridgesMenu;

  /// No description provided for @bridgesTitle.
  ///
  /// In en, this message translates to:
  /// **'Tor bridges'**
  String get bridgesTitle;

  /// No description provided for @bridgesBody.
  ///
  /// In en, this message translates to:
  /// **'If your network blocks Tor, a bridge is an unlisted way in. Paste the bridge lines you were given — one per line, exactly as they came.'**
  String get bridgesBody;

  /// No description provided for @bridgesLimit.
  ///
  /// In en, this message translates to:
  /// **'Bridges get past a network that blocks the public list of Tor relays. Getting past one that inspects traffic and blocks Tor by how it looks needs a WebTunnel bridge, which carries Tor inside ordinary HTTPS — Night Drop will say so if this build cannot use one. Even then it is not a guarantee: we have tested it against blocked networks and against intrusion-detection tools, never against a national firewall, and the amount of data a chat moves is not disguised. If being identified as a Tor user is itself dangerous where you are, do not rely on this alone.'**
  String get bridgesLimit;

  /// No description provided for @bridgesHint.
  ///
  /// In en, this message translates to:
  /// **'203.0.113.5:9001 A1B2C3…'**
  String get bridgesHint;

  /// No description provided for @bridgesWhereToGet.
  ///
  /// In en, this message translates to:
  /// **'Get bridges from bridges.torproject.org — add ?transport=webtunnel for a WebTunnel bridge, which looks like ordinary HTTPS traffic. Or email bridges@torproject.org with \"get transport none\" in the body.'**
  String get bridgesWhereToGet;

  /// Button on the bridges screen that requests bridges from the Tor Project's bridge service (moat).
  ///
  /// In en, this message translates to:
  /// **'Get WebTunnel bridges from the Tor Project'**
  String get bridgesFetchButton;

  /// Title of the consent dialog before fetching bridges.
  ///
  /// In en, this message translates to:
  /// **'Get bridges from the Tor Project?'**
  String get bridgesFetchTitle;

  /// Consent text before fetching bridges. Must say plainly: not through Tor, who sees the IP address, nothing about the user, nothing sent to Night Drop.
  ///
  /// In en, this message translates to:
  /// **'Night Drop will ask the Tor Project\'s bridge service for WebTunnel bridges.\n\nThis request does not go through Tor — it\'s meant for when Tor can\'t connect. Your network sees a connection to a content-delivery network. That network and the Tor Project see your IP address; the Tor Project uses it to choose bridges for your country.\n\nThe request contains nothing about you, and nothing is sent to Night Drop. Once you save the bridges, your Tor connection goes through them.'**
  String get bridgesFetchBody;

  /// Label of the optional two-letter country code field in the fetch-bridges dialog.
  ///
  /// In en, this message translates to:
  /// **'Country (optional)'**
  String get bridgesFetchCountryLabel;

  /// Helper under the country field.
  ///
  /// In en, this message translates to:
  /// **'Two letters, e.g. ir. Leave empty to let the Tor Project detect it.'**
  String get bridgesFetchCountryHint;

  /// Confirm button in the fetch-bridges consent dialog.
  ///
  /// In en, this message translates to:
  /// **'Get bridges'**
  String get bridgesFetchGo;

  /// Shown while bridges are being fetched.
  ///
  /// In en, this message translates to:
  /// **'Contacting the Tor Project…'**
  String get bridgesFetching;

  /// After a successful fetch; the lines are in the editor but not saved yet.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{Added 1 WebTunnel bridge. Tap Save to use it.} other{Added {count} WebTunnel bridges. Tap Save to use them.}}'**
  String bridgesFetchAdded(int count);

  /// Shown when fetching bridges failed. {reason} is the technical reason.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t get bridges: {reason}\n\nIf this network blocks that too, use one of the other ways below.'**
  String bridgesFetchFailed(String reason);

  /// No description provided for @bridgesSaved.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =0{No bridges saved} =1{1 bridge saved} other{{count} bridges saved}}'**
  String bridgesSaved(int count);

  /// No description provided for @bridgesRejected.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{1 line was not understood} other{{count} lines were not understood}}'**
  String bridgesRejected(int count);

  /// No description provided for @bridgesRestartTitle.
  ///
  /// In en, this message translates to:
  /// **'Reconnect now?'**
  String get bridgesRestartTitle;

  /// No description provided for @bridgesRestartBody.
  ///
  /// In en, this message translates to:
  /// **'Bridges are used when Night Drop connects to Tor, so they take effect on the next connection. Reconnecting now takes a minute or two.'**
  String get bridgesRestartBody;

  /// No description provided for @bridgesRestartNow.
  ///
  /// In en, this message translates to:
  /// **'Reconnect'**
  String get bridgesRestartNow;

  /// Desktop home-menu item: close-to-tray and start-at-login settings.
  ///
  /// In en, this message translates to:
  /// **'Keep running when closed…'**
  String get desktopRunMenu;

  /// No description provided for @desktopRunTitle.
  ///
  /// In en, this message translates to:
  /// **'Keep running when closed'**
  String get desktopRunTitle;

  /// No description provided for @desktopRunBody.
  ///
  /// In en, this message translates to:
  /// **'Night Drop can only receive messages while it is running. Closing the window normally ends it, and messages wait (on the relay, for up to 24 hours) until you open it again.'**
  String get desktopRunBody;

  /// No description provided for @desktopCloseToTray.
  ///
  /// In en, this message translates to:
  /// **'Close to the tray'**
  String get desktopCloseToTray;

  /// No description provided for @desktopCloseToTrayHint.
  ///
  /// In en, this message translates to:
  /// **'Closing the window leaves Night Drop running in the system tray, so messages keep arriving and you are notified. Click the tray icon to open it; quit with Exit in the menu. If you set an app lock, your key stays in memory while Night Drop runs in the tray, so it can decrypt what arrives.'**
  String get desktopCloseToTrayHint;

  /// No description provided for @desktopNoTray.
  ///
  /// In en, this message translates to:
  /// **'Your desktop does not show tray icons, so a closed window would have no way back. On GNOME, the AppIndicator extension adds them.'**
  String get desktopNoTray;

  /// No description provided for @desktopStartAtLogin.
  ///
  /// In en, this message translates to:
  /// **'Start when you log in'**
  String get desktopStartAtLogin;

  /// No description provided for @desktopStartAtLoginHint.
  ///
  /// In en, this message translates to:
  /// **'With close to the tray on, it starts in the tray without opening a window.'**
  String get desktopStartAtLoginHint;

  /// No description provided for @desktopRunSaved.
  ///
  /// In en, this message translates to:
  /// **'Saved.'**
  String get desktopRunSaved;

  /// No description provided for @desktopRunFailed.
  ///
  /// In en, this message translates to:
  /// **'Could not save: {error}'**
  String desktopRunFailed(String error);

  /// No description provided for @coverTrafficMenu.
  ///
  /// In en, this message translates to:
  /// **'Cover traffic'**
  String get coverTrafficMenu;

  /// No description provided for @coverTrafficTitle.
  ///
  /// In en, this message translates to:
  /// **'Cover traffic'**
  String get coverTrafficTitle;

  /// No description provided for @coverTrafficBody.
  ///
  /// In en, this message translates to:
  /// **'Sends dummy messages to yourself now and then, so the relay can’t tell when you’re really active.'**
  String get coverTrafficBody;

  /// No description provided for @coverTrafficLimit.
  ///
  /// In en, this message translates to:
  /// **'It makes watching you harder but does not stop it, and costs battery and data.'**
  String get coverTrafficLimit;

  /// No description provided for @coverTrafficOn.
  ///
  /// In en, this message translates to:
  /// **'Cover traffic is on.'**
  String get coverTrafficOn;

  /// No description provided for @coverTrafficOff.
  ///
  /// In en, this message translates to:
  /// **'Cover traffic is off.'**
  String get coverTrafficOff;

  /// No description provided for @turnOn.
  ///
  /// In en, this message translates to:
  /// **'Turn on'**
  String get turnOn;

  /// No description provided for @turnOff.
  ///
  /// In en, this message translates to:
  /// **'Turn off'**
  String get turnOff;

  /// No description provided for @nameContactTitle.
  ///
  /// In en, this message translates to:
  /// **'Name this contact'**
  String get nameContactTitle;

  /// No description provided for @nameContactTooltip.
  ///
  /// In en, this message translates to:
  /// **'Name this contact (only you see it)'**
  String get nameContactTooltip;

  /// Explains the local nickname and the identity tag. Must not imply the tag verifies anyone: it is short enough to grind, so the safety number remains the real check.
  ///
  /// In en, this message translates to:
  /// **'A name only you see — it is never sent, and they can’t see or change it. Until you set one, this contact shows as {tag}, which comes from their key: it changes if their identity does. It is not proof of who they are — compare the safety number for that.'**
  String nameContactBody(String tag);

  /// Shown in a chat when nothing authenticated has arrived from the peer for a long time. Deliberately states the observation only: the app cannot tell a wiped identity from a lost phone or a holiday, and must not imply it can.
  ///
  /// In en, this message translates to:
  /// **'No sign of this person for {days} days. Your messages are still being held for them. There are many reasons someone goes quiet — if it matters, reach them another way.'**
  String silenceBanner(int days);

  /// Header of the menu opened by long-pressing (or right-clicking) the send button.
  ///
  /// In en, this message translates to:
  /// **'Burn after reading'**
  String get burnMenuTitle;

  /// Honest limitation shown in the burn menu. Must never be softened into a guarantee — the feature is a courtesy, not a control (SECURITY.md).
  ///
  /// In en, this message translates to:
  /// **'Hidden until opened, then deleted. A screenshot or a photo of the screen still works.'**
  String get burnCaveat;

  /// A burn duration in seconds.
  ///
  /// In en, this message translates to:
  /// **'{count} seconds'**
  String burnSeconds(int count);

  /// A burn duration in minutes.
  ///
  /// In en, this message translates to:
  /// **'{count} minutes'**
  String burnMinutes(int count);

  /// The one-minute burn duration.
  ///
  /// In en, this message translates to:
  /// **'1 minute'**
  String get burnOneMinute;

  /// Shown instead of the duration list when the peer has not announced burn support. Sending is refused rather than silently downgraded.
  ///
  /// In en, this message translates to:
  /// **'This contact\'s app version can\'t burn messages — it would keep this one. Ask them to update.'**
  String get burnUnsupported;

  /// Label on a blurred, not-yet-opened burn message.
  ///
  /// In en, this message translates to:
  /// **'Tap to reveal'**
  String get burnTapToReveal;

  /// Label on the sender's own copy of a burn message.
  ///
  /// In en, this message translates to:
  /// **'Burn message'**
  String get burnSentLabel;

  /// Explains to the SENDER why their copy lingers and why there is no read receipt — so its behaviour is not mistaken for a bug.
  ///
  /// In en, this message translates to:
  /// **'Deleted from your device after 24 hours. You won\'t be told when it\'s opened.'**
  String get burnSenderNote;

  /// Countdown on an unopened burn message approaching its 24h expiry.
  ///
  /// In en, this message translates to:
  /// **'Disappears in {time}'**
  String burnExpiresIn(String time);

  /// Shown in the burn menu only when opt-in server storage is on. The relay copy is removed by the recipient's own drain (Request::Fetch removes), so it normally clears within minutes — 24h is the worst case where they never come back online, not the usual one. Do not restate it as 'stays 24 hours'.
  ///
  /// In en, this message translates to:
  /// **'Server storage is on, so a copy sits on the relay until their app next collects it — up to 24 hours if it never does.'**
  String get burnServerStorageCaveat;

  /// Home menu entry for the burn-view receipt setting.
  ///
  /// In en, this message translates to:
  /// **'Burn read receipts'**
  String get burnReceiptsMenu;

  /// Title of the burn-receipt settings dialog.
  ///
  /// In en, this message translates to:
  /// **'Tell senders when you open a burn message'**
  String get burnReceiptsTitle;

  /// What the burn-receipt setting does.
  ///
  /// In en, this message translates to:
  /// **'Off by default. When on, opening a burn message tells the sender, so their copy disappears at that moment instead of waiting out its 24 hours.'**
  String get burnReceiptsBody;

  /// The honest cost of the setting. The early deletion IS the disclosure — it cannot be separated — so this must be stated before the user turns it on, not implied.
  ///
  /// In en, this message translates to:
  /// **'This tells them WHEN you read it. Their copy vanishing is itself the signal, so there is no way to have the early deletion without them learning the time you opened it. With this off, their copy simply lives its full 24 hours.'**
  String get burnReceiptsLimit;

  /// Confirmation snackbar when enabled.
  ///
  /// In en, this message translates to:
  /// **'Senders will be told when you open a burn message.'**
  String get burnReceiptsOn;

  /// Confirmation snackbar when disabled.
  ///
  /// In en, this message translates to:
  /// **'Senders are no longer told when you open a burn message.'**
  String get burnReceiptsOff;

  /// Intro on the My relays screen.
  ///
  /// In en, this message translates to:
  /// **'Where messages wait for you while you\'re offline. Your contacts are told this list automatically, end-to-end encrypted. Relays can\'t read your messages.'**
  String get relaysIntro;

  /// The Night Drop relay entry in the relay list.
  ///
  /// In en, this message translates to:
  /// **'Night Drop relay'**
  String get relayNightDropName;

  /// Subtitle of the Night Drop relay entry.
  ///
  /// In en, this message translates to:
  /// **'Run by the Night Drop project. Encrypted messages wait up to 24 hours.'**
  String get relayNightDropBody;

  /// Subtitle of a user-added relay.
  ///
  /// In en, this message translates to:
  /// **'A relay you added'**
  String get relayOwnBody;

  /// Hint in the field for adding a relay.
  ///
  /// In en, this message translates to:
  /// **'Relay address (…onion)'**
  String get relayAddHint;

  /// Button: add the relay typed in the field.
  ///
  /// In en, this message translates to:
  /// **'Add'**
  String get relayAdd;

  /// Button: put the Night Drop relay back in the list.
  ///
  /// In en, this message translates to:
  /// **'Add the Night Drop relay'**
  String get relayAddNightDrop;

  /// Action: remove a relay from the list.
  ///
  /// In en, this message translates to:
  /// **'Remove'**
  String get relayRemove;

  /// Action: show this device's access key for a private relay.
  ///
  /// In en, this message translates to:
  /// **'Access key'**
  String get relayAccessKey;

  /// Explains the private-relay access key.
  ///
  /// In en, this message translates to:
  /// **'If this relay is private, give its operator this key. They allow it with \"nightdrop-relay authorize-client\". A public relay doesn\'t need it.'**
  String get relayAccessKeyBody;

  /// Snackbar after copying the access key.
  ///
  /// In en, this message translates to:
  /// **'Access key copied'**
  String get relayAccessKeyCopied;

  /// Button: copy to clipboard.
  ///
  /// In en, this message translates to:
  /// **'Copy'**
  String get copy;

  /// Shown when the list is full.
  ///
  /// In en, this message translates to:
  /// **'You can have up to {max} relays.'**
  String relayLimitReached(String max);

  /// Validation error for a relay address.
  ///
  /// In en, this message translates to:
  /// **'That isn\'t an onion address. It should be 56 letters and digits ending in .onion.'**
  String get relayInvalidOnion;

  /// Note on the My relays screen.
  ///
  /// In en, this message translates to:
  /// **'A relay you remove is still checked for messages for 24 hours.'**
  String get relayRemovedStillChecked;

  /// Snackbar after saving the relay list.
  ///
  /// In en, this message translates to:
  /// **'Saved. Your contacts are being told.'**
  String get relaysSaved;

  /// Name of the no-relay mode.
  ///
  /// In en, this message translates to:
  /// **'Direct only'**
  String get directOnlyName;

  /// Shown on My relays when the list is empty.
  ///
  /// In en, this message translates to:
  /// **'No relays: direct only. You receive messages only while Night Drop is online.'**
  String get directOnlyListEmpty;

  /// Title of the confirmation before going direct only.
  ///
  /// In en, this message translates to:
  /// **'Remove your last relay?'**
  String get directOnlyConfirmTitle;

  /// Lists what turns off without a relay.
  ///
  /// In en, this message translates to:
  /// **'With no relay, this device never talks to a server:\n• messages reach you only while your app is online, and wait on your contacts\' devices until then\n• new contacts can be added by QR code only\n• no server backup and no server storage\n• no cover traffic\n\nYou can add a relay again at any time.'**
  String get directOnlyConfirmBody;

  /// Button confirming direct only.
  ///
  /// In en, this message translates to:
  /// **'Use direct only'**
  String get directOnlyConfirm;

  /// Home-screen note while the relay list is empty.
  ///
  /// In en, this message translates to:
  /// **'Direct only: you receive messages only while Night Drop is online.'**
  String get homeDirectOnlyNote;

  /// Chat banner when the contact uses no relay.
  ///
  /// In en, this message translates to:
  /// **'{name} receives messages only while their app is online. Messages wait on this device until you\'re both online.'**
  String chatPeerDirectOnly(String name);

  /// Chat banner while the contact's relay set is unknown.
  ///
  /// In en, this message translates to:
  /// **'{name} hasn\'t shared where they receive messages yet. Messages wait on this device until they do.'**
  String chatPeerRelaysUnknown(String name);

  /// Chat banner while we are direct only.
  ///
  /// In en, this message translates to:
  /// **'You\'re on direct only: messages reach you only while your app is online.'**
  String get chatSelfDirectOnly;

  /// Why server storage is unavailable.
  ///
  /// In en, this message translates to:
  /// **'Server storage needs a relay, and you\'re on direct only.'**
  String get serverStorageNeedsRelaySelf;

  /// Why server storage is unavailable.
  ///
  /// In en, this message translates to:
  /// **'Server storage needs a relay on both sides, and {name} doesn\'t use one.'**
  String serverStorageNeedsRelayPeer(String name);

  /// Why server backup is unavailable.
  ///
  /// In en, this message translates to:
  /// **'Server backup is kept on the Night Drop relay, which isn\'t in your list. Save a backup file instead, or add the Night Drop relay in My relays.'**
  String get serverBackupUnavailable;

  /// Why cover traffic is unavailable.
  ///
  /// In en, this message translates to:
  /// **'Cover traffic hides your activity on a relay. You\'re on direct only, so there\'s no relay to cover.'**
  String get coverTrafficUnavailable;

  /// Why short codes are unavailable.
  ///
  /// In en, this message translates to:
  /// **'Short codes need a relay. On direct only, add contacts by QR code.'**
  String get shortCodesUnavailable;

  /// Button opening the My relays screen.
  ///
  /// In en, this message translates to:
  /// **'My relays'**
  String get openMyRelays;

  /// Setup page: relay choice heading.
  ///
  /// In en, this message translates to:
  /// **'How should messages reach you while you\'re offline?'**
  String get setupRelayTitle;

  /// Setup option.
  ///
  /// In en, this message translates to:
  /// **'Night Drop relay'**
  String get setupRelayNightDrop;

  /// Badge on the recommended option.
  ///
  /// In en, this message translates to:
  /// **'Recommended'**
  String get setupRecommended;

  /// Setup option body.
  ///
  /// In en, this message translates to:
  /// **'Encrypted messages wait up to 24 hours on our relay. It can\'t read them.'**
  String get setupRelayNightDropBody;

  /// Setup option.
  ///
  /// In en, this message translates to:
  /// **'My own relay'**
  String get setupRelayOwn;

  /// Setup option body.
  ///
  /// In en, this message translates to:
  /// **'A relay you or someone you trust runs.'**
  String get setupRelayOwnBody;

  /// Setup option.
  ///
  /// In en, this message translates to:
  /// **'Direct only'**
  String get setupRelayDirect;

  /// Setup option body.
  ///
  /// In en, this message translates to:
  /// **'No server at all. You receive messages only while the app is online, and you can add contacts by QR code only.'**
  String get setupRelayDirectBody;

  /// Subtitle of a relay that did not answer our last check.
  ///
  /// In en, this message translates to:
  /// **'Not answering. Messages to you may be delayed.'**
  String get relayNotAnswering;

  /// One-line summary of the chat banner for a contact with no relay; tap for more.
  ///
  /// In en, this message translates to:
  /// **'{name} receives only while online'**
  String chatPeerDirectOnlySummary(String name);

  /// One-line summary while the contact's relay set is unknown; tap for more.
  ///
  /// In en, this message translates to:
  /// **'Messages to {name} wait on this device'**
  String chatPeerRelaysUnknownSummary(String name);

  /// One-line summary of the chat banner while we are direct only; tap for more.
  ///
  /// In en, this message translates to:
  /// **'You\'re on direct only'**
  String get chatSelfDirectOnlySummary;

  /// One-line summary of the chat notice for a contact still on 0.1; tap for more.
  ///
  /// In en, this message translates to:
  /// **'{name} needs to update Night Drop'**
  String chatPeerOutdatedSummary(String name);

  /// Chat notice for a contact still on 0.1.x, which cannot exchange messages with 0.2.
  ///
  /// In en, this message translates to:
  /// **'{name} is on an older version of Night Drop (0.1), which can\'t talk to yours. Your messages wait on this device and go out by themselves once they update. Ask them to update.'**
  String chatPeerOutdated(String name);

  /// Delivery status: held on this device because the contact's app (0.1) can't read it; sent once they update.
  ///
  /// In en, this message translates to:
  /// **'Waiting for {name} to update'**
  String deliveryUpdate(String name);
}

class _AppLocalizationsDelegate
    extends LocalizationsDelegate<AppLocalizations> {
  const _AppLocalizationsDelegate();

  @override
  Future<AppLocalizations> load(Locale locale) {
    return SynchronousFuture<AppLocalizations>(lookupAppLocalizations(locale));
  }

  @override
  bool isSupported(Locale locale) =>
      <String>['en'].contains(locale.languageCode);

  @override
  bool shouldReload(_AppLocalizationsDelegate old) => false;
}

AppLocalizations lookupAppLocalizations(Locale locale) {
  // Lookup logic when only language code is specified.
  switch (locale.languageCode) {
    case 'en':
      return AppLocalizationsEn();
  }

  throw FlutterError(
      'AppLocalizations.delegate failed to load unsupported locale "$locale". This is likely '
      'an issue with the localizations generation tool. Please file an issue '
      'on GitHub with a reproducible sample app and the gen-l10n configuration '
      'that was used.');
}
