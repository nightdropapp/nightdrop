import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/sensitive_clipboard.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  String? clip;

  setUp(() {
    clip = null;
    SensitiveClipboard.isAndroid = () => false;
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(SystemChannels.platform, (call) async {
      if (call.method == 'Clipboard.setData') {
        clip = (call.arguments as Map)['text'] as String?;
      }
      if (call.method == 'Clipboard.getData') return {'text': clip};
      return null;
    });
  });

  testWidgets('an invite link is cleared from the clipboard after a minute', (tester) async {
    await SensitiveClipboard.copy('nightdrop://pair?secret');
    expect(clip, 'nightdrop://pair?secret');
    await tester.pump(SensitiveClipboard.clearAfter + const Duration(seconds: 1));
    expect(clip, '');
  });

  testWidgets('something copied since is never touched', (tester) async {
    await SensitiveClipboard.copy('nightdrop://pair?secret');
    clip = 'my own note';
    await tester.pump(SensitiveClipboard.clearAfter + const Duration(seconds: 1));
    expect(clip, 'my own note');
  });
}
