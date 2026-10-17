import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

/// Every text field tells the keyboard not to learn what is typed in it (self-audit 2026-10-17,
/// P8). Without `enableIMEPersonalizedLearning: false`, Gboard and other keyboards keep words
/// from messages, names and codes in their own dictionaries — and some sync them to the cloud.
/// A source check rather than a widget test, so a field added anywhere later is caught too.
void main() {
  test('no text field lets the keyboard learn from it', () {
    final field = RegExp(r'\b(TextField|TextFormField|CupertinoTextField)\(');
    final missing = <String>[];
    for (final f
        in Directory('lib').listSync(recursive: true).whereType<File>()) {
      if (!f.path.endsWith('.dart')) continue;
      final src = f.readAsStringSync();
      for (final m in field.allMatches(src)) {
        // The constructor's own arguments: up to the matching close paren.
        var depth = 0, end = m.end - 1;
        for (; end < src.length; end++) {
          if (src[end] == '(') depth++;
          if (src[end] == ')' && --depth == 0) break;
        }
        if (!src
            .substring(m.end, end)
            .contains('enableIMEPersonalizedLearning: false')) {
          final line = '\n'.allMatches(src.substring(0, m.start)).length + 1;
          missing.add('${f.path}:$line');
        }
      }
    }
    expect(missing, isEmpty);
  });
}
