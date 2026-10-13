//! What an attachment is allowed to claim to be, and what a decrypted one may be opened as.
//!
//! An attachment's `kind` and `mime` arrive from the contact, and used to reach the filesystem
//! and the OS unchecked: the app named the decrypted file after the last segment of the mime and
//! handed it to the system by extension. A contact could send a "video" as `video/bat` (Windows
//! ran it on tap), `video/html` or `video/m3u` (the browser or player then fetched the
//! contact's URL over clearnet, revealing the recipient's IP), or put `\` in it to place the
//! file outside the cache on Windows. Self-audit 2026-10-13, finding 1.
//!
//! Two rules close it:
//!
//! * On receipt, `kind` is `image` or `video` and `mime` comes from a fixed list; anything else
//!   is replaced, never stored as sent ([`sanitize`]).
//! * The file handed to another app is named from **its own bytes**, never from anything the
//!   contact said: it must start like a video container we know, and its extension comes from
//!   that container ([`video_ext`]). A player sniffs content, so a trusted extension on a
//!   playlist would still leak; checking the bytes is what makes the name honest.

/// Mime types an attachment may carry, per kind. Matches what the app sends (`_mimeFor` in
/// `chat_screen.dart`); images are re-encoded to JPEG except GIFs.
const IMAGE_MIMES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/heic",
];
const VIDEO_MIMES: &[&str] = &[
    "video/mp4",
    "video/quicktime",
    "video/webm",
    "video/x-matroska",
    "video/x-msvideo",
    "video/x-m4v",
    "video/3gpp",
    "video/octet-stream",
];

/// Normalise a received attachment's `(kind, mime)`: an unknown kind becomes `image` (shown
/// in-app only, never opened externally), and a mime outside the kind's list becomes the kind's
/// neutral default.
pub(crate) fn sanitize(kind: String, mime: String) -> (String, String) {
    let (kind, allowed, fallback): (&str, &[&str], &str) = if kind == "video" {
        ("video", VIDEO_MIMES, "video/octet-stream")
    } else {
        ("image", IMAGE_MIMES, "image/jpeg")
    };
    let mime = mime.trim().to_ascii_lowercase();
    let mime = if allowed.contains(&mime.as_str()) {
        mime
    } else {
        fallback.to_string()
    };
    (kind.to_string(), mime)
}

/// The extension to give a decrypted video handed to another app, read from the file's own
/// leading bytes. `None` for anything that is not a container we recognise: such a file is not
/// opened externally at all.
pub(crate) fn video_ext(data: &[u8]) -> Option<&'static str> {
    // ISO base media (MP4, MOV, M4V, 3GP): a box whose type is `ftyp` at offset 4. QuickTime
    // files may lead with other atoms; accept the ones a .mov really starts with.
    if data.len() >= 12 {
        match &data[4..8] {
            b"ftyp" => {
                return Some(match &data[8..12] {
                    b"qt  " => "mov",
                    b"3gp4" | b"3gp5" | b"3gp6" | b"3gg6" | b"3gp7" => "3gp",
                    _ => "mp4",
                });
            }
            b"moov" | b"mdat" | b"wide" | b"free" | b"skip" => return Some("mov"),
            _ => {}
        }
    }
    // Matroska / WebM: the EBML magic. The DocType that follows says which.
    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let head = &data[..data.len().min(64)];
        return Some(if head.windows(4).any(|w| w == b"webm") {
            "webm"
        } else {
            "mkv"
        });
    }
    // AVI: a RIFF container of form type `AVI `.
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"AVI " {
        return Some("avi");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(kind: &str, mime: &str) -> (String, String) {
        sanitize(kind.to_string(), mime.to_string())
    }

    #[test]
    fn known_kinds_and_mimes_pass_through() {
        assert_eq!(
            s("video", "video/mp4"),
            ("video".into(), "video/mp4".into())
        );
        assert_eq!(
            s("image", "image/gif"),
            ("image".into(), "image/gif".into())
        );
        assert_eq!(
            s("video", " Video/WebM "),
            ("video".into(), "video/webm".into())
        );
    }

    #[test]
    fn hostile_mimes_are_replaced() {
        for m in [
            "video/bat",
            "video/html",
            "video/m3u",
            "video/x\\..\\..\\evil.bat",
            "x/exe",
            "",
            "video/mp4\u{0}",
        ] {
            assert_eq!(
                s("video", m),
                ("video".into(), "video/octet-stream".into()),
                "{m:?}"
            );
        }
        assert_eq!(
            s("image", "text/html"),
            ("image".into(), "image/jpeg".into())
        );
    }

    #[test]
    fn an_unknown_kind_becomes_an_image_never_a_video() {
        assert_eq!(
            s("file", "video/mp4"),
            ("image".into(), "image/jpeg".into())
        );
        assert_eq!(s("Video", "video/mp4").0, "image");
    }

    #[test]
    fn video_ext_comes_from_the_bytes() {
        let mut mp4 = vec![0, 0, 0, 0x20];
        mp4.extend_from_slice(b"ftypisom\0\0\0\0");
        assert_eq!(video_ext(&mp4), Some("mp4"));
        let mut mov = vec![0, 0, 0, 0x14];
        mov.extend_from_slice(b"ftypqt  \0\0\0\0");
        assert_eq!(video_ext(&mov), Some("mov"));
        let mut webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x84];
        webm.extend_from_slice(b"webm");
        assert_eq!(video_ext(&webm), Some("webm"));
        assert_eq!(
            video_ext(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]),
            Some("mkv")
        );
        let mut avi = b"RIFF\0\0\0\0AVI ".to_vec();
        avi.extend_from_slice(b"LIST");
        assert_eq!(video_ext(&avi), Some("avi"));
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            &b"#EXTM3U\nhttp://198.51.100.7/x.mp4\n"[..],
            b"<html><img src=http://198.51.100.7/></html>",
            b"@echo off\r\ncalc\r\n",
            b"MZ\x90\0\x03\0\0\0\x04\0\0\0",
            b"",
            b"ftyp",
        ] {
            assert_eq!(video_ext(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }
}
