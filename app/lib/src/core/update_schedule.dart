/// When the app asks our onion site whether a newer release exists (`ARCHITECTURE.md` §10a).
///
/// Daily is often enough to matter for a security fix and rare enough that the request is not a
/// heartbeat: a beacon every launch would let anyone counting requests infer how many installs
/// exist and how often they run. A check that got no answer (Tor still starting, the site briefly
/// down) is retried after [updateRetryAfterFailure] instead of a whole day, which on its own left a
/// desktop app on 0.1.24 for days after 0.1.27 shipped (2026-10-05).
library;

/// After a check the site answered, the next is this far away.
const updateCheckInterval = Duration(hours: 24);

/// After a check that got no answer.
const updateRetryAfterFailure = Duration(hours: 3);

/// How often a running app looks at whether a check is due. Cheap (a stored timestamp); the
/// intervals above decide when it actually asks. Without it a window left open for days never
/// checked again after launch.
const updateDueTick = Duration(hours: 1);

/// Whether a check is due, given the stored "last checked" time (ms since epoch, 0 = never).
bool updateCheckDue(int lastCheckedMs, DateTime now) =>
    lastCheckedMs == 0 ||
    now.millisecondsSinceEpoch - lastCheckedMs >=
        updateCheckInterval.inMilliseconds;

/// What to store as "last checked" after a check at [now]: [now] when the site answered, or a time
/// placed so that the next check falls due [updateRetryAfterFailure] from now when it did not.
int lastCheckedToStore(DateTime now, {required bool answered}) => answered
    ? now.millisecondsSinceEpoch
    : now
        .subtract(updateCheckInterval - updateRetryAfterFailure)
        .millisecondsSinceEpoch;
