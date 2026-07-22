//! Opt-in operational diagnostics for field debugging (`TODO.md` #6/#7).
//!
//! **How this differs from `devlog!`** (`node.rs`), and why both exist: `devlog!` prints
//! identity keys, invite codes, and decrypted display names, so it is compiled out of release
//! builds entirely — on Android those would land in logcat, which persists and is readable by
//! any `adb`-connected observer. That rule is not relaxed here.
//!
//! These lines are built to be safe in a release build instead: they record **what happened, not
//! who with**. Counts, outcomes, and which leg of a protocol ran — never identity keys, onion
//! addresses, invite codes, slots, secret words, or names. Anything identity-linked belongs in
//! `devlog!`, not here.
//!
//! Even so they are **off by default** and must be turned on explicitly for a debugging build
//! ([`set_enabled`], wired to `NIGHTDROP_DIAG` in the app). A normal release is silent.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn diagnostics on/off at runtime. Off unless the app explicitly enables it.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether diagnostics are currently emitted.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Emit one diagnostic line. Prefer the [`diag!`](crate::diag) macro, which skips formatting
/// entirely when diagnostics are off.
///
/// Redacts onion addresses as a last line of defense: error strings that bubble up here (e.g. a
/// relay dial failure) can carry one in their context, and the channel's guarantee — no onion
/// addresses ever reach a release log — should be enforced here, not left to each call site.
pub fn emit(line: &str) {
    let line = redact_onions(line);
    #[cfg(target_os = "android")]
    android::write("nd-diag", &line);
    #[cfg(not(target_os = "android"))]
    eprintln!("[nd-diag] {line}");
}

/// Emit one line of arti's own tracing (guard/circuit/dir-download/bootstrap progress) under a
/// separate `nd-tor` tag, so field debugging of a stuck Tor bootstrap can see *why* — never linked
/// to a chat. Only reached when diagnostics are on (see [`crate::transport`] tracing install).
/// Onion addresses are still redacted defensively.
pub fn emit_tor(line: &str) {
    if line.is_empty() {
        return;
    }
    let line = redact_onions(line);
    #[cfg(target_os = "android")]
    android::write("nd-tor", &line);
    #[cfg(not(target_os = "android"))]
    eprintln!("[nd-tor] {line}");
}

/// Replace any `<base32>.onion` label with `<onion>`. Defensive and format-agnostic: it works on
/// an arbitrary string (including one embedded in an error chain) so no call site can leak an
/// address through this channel by accident.
fn redact_onions(s: &str) -> std::borrow::Cow<'_, str> {
    let Some(_) = s.find(".onion") else {
        return std::borrow::Cow::Borrowed(s);
    };
    let is_b32 = |c: char| c.is_ascii_lowercase() || ('2'..='7').contains(&c);
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(".onion") {
            // Drop the base32 label we just copied out, then skip the ".onion" suffix.
            while out.ends_with(is_b32) {
                out.pop();
            }
            out.push_str("<onion>");
            i += ".onion".len();
        } else {
            // Advance one UTF-8 char (diagnostic strings are ASCII, but stay correct regardless).
            let mut n = 1;
            while i + n < s.len() && (bytes[i + n] & 0xC0) == 0x80 {
                n += 1;
            }
            out.push_str(&s[i..i + n]);
            i += n;
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Android needs an explicit hop to liblog: a Rust `eprintln!` goes to a stderr nobody reads,
/// so diagnostics would silently vanish on the one platform we most need them on.
#[cfg(target_os = "android")]
mod android {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    const ANDROID_LOG_INFO: c_int = 4;

    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    pub fn write(tag: &str, line: &str) {
        // Interior NULs can't cross into C; drop the line rather than fail a debug aid.
        let (Ok(tag), Ok(text)) = (CString::new(tag), CString::new(line)) else {
            return;
        };
        // SAFETY: both pointers are NUL-terminated and live for the duration of the call.
        unsafe {
            __android_log_write(ANDROID_LOG_INFO, tag.as_ptr(), text.as_ptr());
        }
    }
}

/// Emit a diagnostic line when diagnostics are enabled (see [`crate::diag`]).
///
/// Never pass identity keys, onion addresses, invite codes, slots, secret words, or display
/// names — use `devlog!` for those, which never reaches a release build.
#[macro_export]
macro_rules! diag {
    ($($t:tt)*) => {
        if $crate::diag::enabled() {
            $crate::diag::emit(&format!($($t)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `ENABLED` is process-wide; keep the two tests from racing each other.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn diagnostics_are_off_until_asked_for() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(false);
        assert!(!enabled(), "a normal release build must stay silent");
    }

    #[test]
    fn the_macro_does_not_evaluate_its_arguments_while_disabled() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(false);
        // If `diag!` formatted eagerly, this would panic — proving disabled really is free, and
        // that a secret passed by mistake is never even rendered while off.
        crate::diag!("{}", panic_if_formatted());
        set_enabled(true);
        assert!(enabled());
        set_enabled(false);
    }

    fn panic_if_formatted() -> &'static str {
        panic!("diag! must not format its arguments when diagnostics are disabled");
    }

    #[test]
    fn onion_addresses_are_redacted() {
        // A real v3 onion (56 base32 chars) embedded in an error chain, with a trailing port.
        let onion = "bzcqxuxwvtmrmvprsoscnronkjf5wknfuj5ozxiq5fr6qowvnkwrwwad.onion";
        let input = format!("join: post FAILED: relay connect {onion}:9001");
        let got = redact_onions(&input);
        assert_eq!(got, "join: post FAILED: relay connect <onion>:9001");
        assert!(!got.contains(".onion"), "no onion label may survive: {got}");
    }

    #[test]
    fn redaction_leaves_ordinary_lines_untouched() {
        let line = "join: opener posted to 0/1 relays";
        assert!(matches!(redact_onions(line), std::borrow::Cow::Borrowed(_)));
    }
}
