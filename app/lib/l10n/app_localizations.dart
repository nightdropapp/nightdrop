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
  /// **'Night Drop found data saved on this device but couldn’t open it — often this is just Tor not connecting yet, so trying again usually works. Your existing data is preserved and won’t be overwritten automatically.'**
  String get loadErrorBody;

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

  /// Banner while the onion descriptor is still publishing.
  ///
  /// In en, this message translates to:
  /// **'Publishing your address to Tor (1–3 min). Others can’t pair with you until this finishes — keep the app open.'**
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
  /// **'Keep receiving messages while Night Drop is in the background. This runs a foreground service with a persistent notification and checks for messages over Tor — no push provider, nothing leaves your device to a server.'**
  String get backgroundDeliveryBody;

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

  /// Explanation in the relay editor dialog.
  ///
  /// In en, this message translates to:
  /// **'Extra relays that also hold your mailbox. Contacts fan your offline mail out to these in addition to the shared default, so a message still reaches you if one relay is down or blocked. Relays only ever see opaque, end-to-end-encrypted blobs. One address per line.'**
  String get editRelaysBody;

  /// Snackbar when saving relays fails.
  ///
  /// In en, this message translates to:
  /// **'Could not save relays: {error}'**
  String couldNotSaveRelays(String error);

  /// Snackbar when the extra-relay list is cleared.
  ///
  /// In en, this message translates to:
  /// **'Using the default relay only'**
  String get usingDefaultRelayOnly;

  /// Snackbar confirming how many extra relays are advertised.
  ///
  /// In en, this message translates to:
  /// **'Advertising {count, plural, =1{1 extra relay} other{{count} extra relays}}'**
  String advertisingExtraRelays(int count);

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
  /// **'Delete messages on both devices after a set time.'**
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
