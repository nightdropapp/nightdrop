//! What an attachment is allowed to claim to be, and what a decrypted one may be opened as.
//!
//! An attachment's `kind` and `mime` arrive from the contact. Trusted, they decide how the OS opens
//! the file: a "video" sent as `video/bat` runs on tap on Windows, `video/html` or `video/m3u`
//! makes the browser or player fetch the contact's URL over clearnet (revealing the recipient's
//! IP), and a `\` in it places the file outside the cache on Windows.
//!
//! Two rules close it:
//!
//! * On receipt, `kind` is `image` or `video` and `mime` comes from a fixed list; anything else
//!   is replaced, never stored as sent ([`sanitize`]).
//! * The file handed to another app is named from **its own bytes**, never from anything the
//!   contact said: it must start like a video container we know, and its extension comes from
//!   that container ([`video_ext`]). A player sniffs content, so a trusted extension on a
//!   playlist would still leak; checking the bytes is what makes the name honest.
//!
//! And one for what the app itself draws: an image or a video's preview is decoded in-process, by
//! whichever of the engine's decoders the bytes select (GIF, WebP, BMP, ICO, HEIF…), as soon as the
//! chat is open — the place messengers' zero-click bugs live. Every sender re-encodes to JPEG, so
//! only a plain JPEG of sane size is handed over for drawing ([`displayable_image`]); anything else
//! shows as unavailable.

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

/// Largest side and pixel count of an image the app will draw. Senders downscale to 1600 px; the
/// slack is for later versions. The pixel cap bounds the decode buffer (40 MP × 4 B = 160 MB at the
/// very worst; the app decodes previews at screen size).
const MAX_IMAGE_SIDE: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 40_000_000;

/// Whether `data` may be handed to the app to draw: a JPEG (Huffman-coded baseline, extended or
/// progressive; one or three components) whose frame header gives a size within
/// [`MAX_IMAGE_SIDE`] and [`MAX_IMAGE_PIXELS`]. Reads only marker headers, never pixel data.
pub(crate) fn displayable_image(data: &[u8]) -> bool {
    matches!(jpeg_frame(data), Some((w, h, comps))
        if (1..=MAX_IMAGE_SIDE).contains(&w)
            && (1..=MAX_IMAGE_SIDE).contains(&h)
            && u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS
            && (comps == 1 || comps == 3))
}

/// `(width, height, components)` from a JPEG's frame header, walking the marker segments that
/// precede it. `None` for anything that is not a JPEG, has no frame header before the scan, or
/// uses a coding other than Huffman baseline/extended/progressive (arithmetic, lossless,
/// hierarchical: decoders' rarely-used paths, and nothing a camera or our encoder writes).
fn jpeg_frame(data: &[u8]) -> Option<(u32, u32, u8)> {
    if data.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut p = 2;
    loop {
        // Markers may be preceded by any number of 0xFF fill bytes.
        if *data.get(p)? != 0xFF {
            return None;
        }
        while *data.get(p)? == 0xFF {
            p += 1;
        }
        let marker = *data.get(p)?;
        p += 1;
        match marker {
            // Restart markers and TEM stand alone; none belongs before the frame header.
            0x01 | 0xD0..=0xD7 => continue,
            // A scan or the end before any frame header.
            0xD8..=0xDA => return None,
            _ => {}
        }
        let len = u16::from_be_bytes([*data.get(p)?, *data.get(p + 1)?]) as usize;
        if len < 2 {
            return None;
        }
        let seg = data.get(p + 2..p + len)?;
        match marker {
            0xC0..=0xC2 => {
                if seg.len() < 6 {
                    return None;
                }
                let h = u16::from_be_bytes([seg[1], seg[2]]) as u32;
                let w = u16::from_be_bytes([seg[3], seg[4]]) as u32;
                return Some((w, h, seg[5]));
            }
            // Any other start-of-frame: arithmetic, lossless or hierarchical coding.
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            _ => p += len,
        }
    }
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

    const BASELINE: &[u8] = include_bytes!("../../tests/fixtures/small-baseline.jpg");
    const PROGRESSIVE: &[u8] = include_bytes!("../../tests/fixtures/small-progressive.jpg");
    const CMYK: &[u8] = include_bytes!("../../tests/fixtures/small-cmyk.jpg");
    const PNG: &[u8] = include_bytes!("../../tests/fixtures/small.png");

    /// A JPEG that is all header: SOI, a frame header of the given marker and size, then EOI.
    fn jpeg_header(sof: u8, w: u16, h: u16, comps: u8) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, sof, 0, 8, 8];
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.push(comps);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn only_plain_jpegs_of_sane_size_are_drawn() {
        assert_eq!(jpeg_frame(BASELINE), Some((64, 48, 3)));
        assert_eq!(jpeg_frame(PROGRESSIVE), Some((64, 48, 3)));
        assert!(displayable_image(BASELINE));
        assert!(displayable_image(PROGRESSIVE));
        assert!(displayable_image(&jpeg_header(0xC0, 1600, 1067, 1)));
        assert!(displayable_image(&jpeg_header(0xC2, 8192, 4882, 3)));

        assert!(!displayable_image(PNG), "PNG");
        assert!(!displayable_image(CMYK), "four components");
        assert!(!displayable_image(b"GIF89a\x01\x00\x01\x00"), "GIF");
        assert!(!displayable_image(b"RIFF\0\0\0\0WEBPVP8 "), "WebP");
        assert!(!displayable_image(&[]));
        assert!(
            !displayable_image(&jpeg_header(0xC9, 64, 48, 3)),
            "arithmetic"
        );
        assert!(
            !displayable_image(&jpeg_header(0xC3, 64, 48, 3)),
            "lossless"
        );
        assert!(
            !displayable_image(&jpeg_header(0xC0, 8193, 10, 3)),
            "too wide"
        );
        assert!(
            !displayable_image(&jpeg_header(0xC0, 8000, 8000, 3)),
            "too many pixels"
        );
        assert!(!displayable_image(&jpeg_header(0xC0, 0, 48, 3)), "empty");
        // A JPEG whose scan starts before any frame header.
        assert!(!displayable_image(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]));
    }

    #[test]
    fn a_truncated_or_garbled_header_is_refused_without_panicking() {
        for real in [BASELINE, PROGRESSIVE] {
            for n in 0..real.len() {
                let _ = displayable_image(&real[..n]);
            }
            let mut flipped = real.to_vec();
            for i in 0..flipped.len().min(200) {
                flipped[i] ^= 0xFF;
                let _ = displayable_image(&flipped);
                flipped[i] ^= 0xFF;
            }
        }
        // A segment length running past the end.
        assert!(!displayable_image(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF, 0]));
        // Fill bytes up to the end.
        assert!(!displayable_image(&[0xFF, 0xD8, 0xFF, 0xFF, 0xFF]));
    }

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

    /// Real photos: `NIGHTDROP_JPEG_LIST=<file of paths> cargo test -p nightdrop real_jpegs -- --ignored`.
    /// Every listed JPEG of three components and sane size must be drawable; prints any refused,
    /// with the reason, so a real camera's output is never silently hidden.
    #[test]
    #[ignore]
    fn real_jpegs_are_drawn() {
        let list = std::fs::read_to_string(std::env::var("NIGHTDROP_JPEG_LIST").unwrap()).unwrap();
        let (mut ok, mut refused) = (0, 0);
        for path in list.lines().filter(|l| !l.is_empty()) {
            let data = std::fs::read(path).unwrap();
            if displayable_image(&data) {
                ok += 1;
                // With NIGHTDROP_JPEG_OUT set, also write what sending would make of it.
                if let Ok(out) = std::env::var("NIGHTDROP_JPEG_OUT") {
                    let clean = crate::node::media_scrub::scrub_jpeg(&data).unwrap();
                    std::fs::write(std::path::Path::new(&out).join(format!("{ok}.jpg")), clean)
                        .unwrap();
                    std::fs::write(std::path::Path::new(&out).join(format!("{ok}.src")), path)
                        .unwrap();
                }
            } else {
                refused += 1;
                eprintln!("refused {:?}: {path}", jpeg_frame(&data));
            }
        }
        eprintln!("{ok} drawn, {refused} refused");
        assert!(ok > 0);
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
