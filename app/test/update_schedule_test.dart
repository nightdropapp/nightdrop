import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/update_schedule.dart';

void main() {
  final t0 = DateTime.utc(2026, 10, 5, 12);

  test('the first check is due at once', () {
    expect(updateCheckDue(0, t0), isTrue);
  });

  test('an answered check makes the next one due a day later', () {
    final stored = lastCheckedToStore(t0, answered: true);
    expect(
        updateCheckDue(stored, t0.add(const Duration(hours: 23, minutes: 59))),
        isFalse);
    expect(updateCheckDue(stored, t0.add(const Duration(hours: 24))), isTrue);
  });

  test('an unanswered check is retried after a few hours, not a day', () {
    final stored = lastCheckedToStore(t0, answered: false);
    expect(updateCheckDue(stored, t0.add(const Duration(minutes: 1))), isFalse,
        reason: 'not a retry on every launch');
    expect(
        updateCheckDue(stored,
            t0.add(updateRetryAfterFailure - const Duration(minutes: 1))),
        isFalse);
    expect(updateCheckDue(stored, t0.add(updateRetryAfterFailure)), isTrue);
  });

  test('a running app ticks often enough to notice within the hour', () {
    expect(updateDueTick < updateRetryAfterFailure, isTrue);
  });
}
