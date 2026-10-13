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
        let ext = match &data[4..8] {
            b"ftyp" => match &data[8..12] {
                b"qt  " => Some("mov"),
                b"3gp4" | b"3gp5" | b"3gp6" | b"3gg6" | b"3gp7" => Some("3gp"),
                _ => Some("mp4"),
            },
            b"moov" | b"mdat" | b"wide" | b"free" | b"skip" => Some("mov"),
            _ => None,
        };
        if let Some(ext) = ext {
            // The right magic is not enough: a QuickTime reference movie or an external data
            // reference makes a player (VLC, QuickTime) fetch a URL the contact chose.
            return iso_bmff_is_self_contained(data).then_some(ext);
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

/// Box types that hold other boxes on the way to anything that can point outside the file.
const ISO_CONTAINERS: &[&[u8; 4]] = &[
    b"moov", b"trak", b"mdia", b"minf", b"dinf", b"edts", b"udta",
];

/// Whether an ISO base media / QuickTime file can play without fetching anything: it has a
/// `moov`, every box size adds up, `moov` holds no reference movie (`rmra`/`rmda`/`rdrf`, which
/// name URLs players follow), and every data reference (`dref`) entry is self-contained (flag 1).
/// Anything unparseable fails closed — the cost is only that such a file is not opened outside
/// the app.
fn iso_bmff_is_self_contained(data: &[u8]) -> bool {
    let Some(top) = boxes(data) else {
        return false;
    };
    let mut saw_moov = false;
    for (kind, body) in top {
        if kind == *b"moov" {
            saw_moov = true;
            // Belt and braces for the reference-movie atoms, wherever they sit inside moov
            // (QuickTime nests them in ways the walk below does not descend into).
            if [b"rmra", b"rmda", b"rdrf"]
                .iter()
                .any(|tag| body.windows(4).any(|w| w == *tag))
            {
                return false;
            }
            if !drefs_self_contained(body) {
                return false;
            }
        }
    }
    saw_moov
}

/// Walk `body`'s children down through [`ISO_CONTAINERS`]; every `dref` found must reference only
/// the file itself.
fn drefs_self_contained(body: &[u8]) -> bool {
    let Some(children) = boxes(body) else {
        return false;
    };
    children.into_iter().all(|(kind, inner)| {
        if kind == *b"dref" {
            dref_self_contained(inner)
        } else if ISO_CONTAINERS.contains(&&kind) {
            drefs_self_contained(inner)
        } else {
            true
        }
    })
}

/// A `dref` full box: version/flags (4), entry count (4), then entry boxes whose own
/// version/flags must have the self-contained bit (1) set.
fn dref_self_contained(body: &[u8]) -> bool {
    let Some(rest) = body.get(8..) else {
        return false;
    };
    let Some(entries) = boxes(rest) else {
        return false;
    };
    entries
        .iter()
        .all(|(_, entry)| entry.get(3).is_some_and(|flags| flags & 1 == 1))
}

/// Split `data` into ISO boxes `(type, body)`. `None` if a size does not fit: 32-bit size, `1` for
/// a 64-bit size after the type, `0` for "to the end".
fn boxes(mut data: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let size32 = u32::from_be_bytes(data.get(0..4)?.try_into().ok()?) as u64;
        let kind: [u8; 4] = data.get(4..8)?.try_into().ok()?;
        let (header, size) = match size32 {
            0 => (8u64, data.len() as u64),
            1 => (16, u64::from_be_bytes(data.get(8..16)?.try_into().ok()?)),
            n => (8, n),
        };
        if size < header || size > data.len() as u64 {
            return None;
        }
        out.push((kind, &data[header as usize..size as usize]));
        data = &data[size as usize..];
    }
    Some(out)
}

/// Test helper: an ISO box.
#[cfg(test)]
pub(crate) fn iso_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

/// Test helper: a small but well-formed MP4 whose one data reference is the file itself.
#[cfg(test)]
pub(crate) fn sample_mp4(brand: &[u8; 4], dref_flags: u8, extra_in_moov: &[u8]) -> Vec<u8> {
    let url = iso_box(b"url ", &[0, 0, 0, dref_flags]);
    let mut dref_body = vec![0, 0, 0, 0, 0, 0, 0, 1];
    dref_body.extend_from_slice(&url);
    let dinf = iso_box(b"dinf", &iso_box(b"dref", &dref_body));
    let trak = iso_box(b"trak", &iso_box(b"mdia", &iso_box(b"minf", &dinf)));
    let mut moov_body = iso_box(b"mvhd", &[0u8; 20]);
    moov_body.extend_from_slice(&trak);
    moov_body.extend_from_slice(extra_in_moov);
    let mut ftyp_body = brand.to_vec();
    ftyp_body.extend_from_slice(&[0, 0, 0, 0]);
    let mut out = iso_box(b"ftyp", &ftyp_body);
    out.extend_from_slice(&iso_box(b"moov", &moov_body));
    out.extend_from_slice(&iso_box(b"mdat", b"frames"));
    out
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
        assert_eq!(video_ext(&sample_mp4(b"isom", 1, b"")), Some("mp4"));
        assert_eq!(video_ext(&sample_mp4(b"qt  ", 1, b"")), Some("mov"));
        assert_eq!(video_ext(&sample_mp4(b"3gp5", 1, b"")), Some("3gp"));
        // moov first, no ftyp (old QuickTime).
        let mut mov = iso_box(b"moov", &iso_box(b"mvhd", &[0u8; 20]));
        mov.extend_from_slice(&iso_box(b"mdat", b"x"));
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
    fn an_mp4_that_points_outside_itself_is_refused() {
        // A QuickTime reference movie: players follow the URL in it.
        let rmra = iso_box(
            b"rmra",
            &iso_box(
                b"rmda",
                &iso_box(b"rdrf", b"\0\0\0\0url http://198.51.100.7/a.mov"),
            ),
        );
        assert_eq!(video_ext(&sample_mp4(b"qt  ", 1, &rmra)), None);
        // A data reference that is not the file itself (flag 0, a location follows).
        assert_eq!(video_ext(&sample_mp4(b"isom", 0, b"")), None);
        // No moov at all, and a box whose size runs past the end.
        assert_eq!(video_ext(&iso_box(b"ftyp", b"isom\0\0\0\0")), None);
        let mut cut = sample_mp4(b"isom", 1, b"");
        cut.truncate(cut.len() - 3);
        assert_eq!(video_ext(&cut), None);
    }

    /// Real files: `NIGHTDROP_VIDEO_DIR=<dir> cargo test -p nightdrop real_videos -- --ignored`.
    /// Every file there must be recognised (a refused real video is a regression).
    #[test]
    #[ignore]
    fn real_videos_are_recognised() {
        let dir = std::env::var("NIGHTDROP_VIDEO_DIR").expect("NIGHTDROP_VIDEO_DIR");
        let mut refused = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let data = std::fs::read(entry.path()).unwrap();
            let got = video_ext(&data);
            eprintln!("{:?} -> {got:?}", entry.file_name());
            if got.is_none() {
                refused.push(entry.file_name());
            }
        }
        assert!(refused.is_empty(), "refused real videos: {refused:?}");
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
