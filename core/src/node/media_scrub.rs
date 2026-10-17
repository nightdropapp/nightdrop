//! Remove what a video says about where, when and on what it was recorded, before it is sent
//! (self-audit 2026-10-17, P1).
//!
//! Phones write the recording's GPS position (MP4 `©xyz`, QuickTime
//! `com.apple.quicktime.location.ISO6709`), the device make and model, the software version and
//! the exact recording time into the file, and Night Drop used to send videos byte for byte. A
//! contact — or anyone a contact forwards it to — learned where the sender was standing.
//!
//! **Edited in place, never remuxed.** A metadata box becomes padding of the same size (`free` in
//! ISO base media, `Void` in Matroska, `JUNK` in AVI) with its contents zeroed, and timestamps in
//! headers are zeroed. Nothing moves, so the sample offsets that point into the file stay right and
//! the picture and sound are bit for bit what was recorded; players skip padding by design.
//!
//! **Fails closed.** A file whose container cannot be walked far enough to find its metadata is
//! not sent: the cost is a refused attachment, never a leaked location.

/// What [`scrub_video`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Scrubbed {
    /// Metadata boxes or elements turned into padding.
    pub blanked: usize,
    /// Timestamps zeroed.
    pub times: usize,
}

/// Scrub `data` (a video about to be sent) in place. `Err` if it is not a container we can walk:
/// the caller then refuses to send it.
pub(crate) fn scrub_video(data: &mut [u8]) -> anyhow::Result<Scrubbed> {
    let mut done = Scrubbed {
        blanked: 0,
        times: 0,
    };
    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        scrub_matroska(data, &mut done)?;
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"AVI " {
        scrub_avi(data, &mut done)?;
    } else {
        scrub_iso(data, &mut done)?;
    }
    Ok(done)
}

/// A sent photo, with every metadata segment removed: the app re-encodes photos to JPEG without
/// EXIF already, and this makes that a property of the core rather than of one encoder. Keeps only
/// what decoding needs: JFIF (`APP0`), Adobe's colour-transform flag (`APP14`) and the coding
/// segments; drops `APP1`–`APP13` and `APP15` (EXIF with GPS, XMP, ICC with device names, maker
/// notes) and comments. `Err` if it is not a JPEG the recipient would draw
/// ([`displayable_image`](super::media_kind::displayable_image)) — sending it would only arrive
/// as "unavailable".
pub(crate) fn scrub_jpeg(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        super::media_kind::displayable_image(data),
        "only JPEG photos can be sent"
    );
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&[0xFF, 0xD8]);
    let mut p = 2;
    loop {
        let bad = || anyhow::anyhow!("only JPEG photos can be sent");
        if *data.get(p).ok_or_else(bad)? != 0xFF {
            anyhow::bail!("only JPEG photos can be sent");
        }
        while *data.get(p).ok_or_else(bad)? == 0xFF {
            p += 1;
        }
        let marker = data[p];
        p += 1;
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            out.extend_from_slice(&[0xFF, marker]);
            continue;
        }
        if marker == 0xDA {
            // The first scan: everything from here on is image data, kept as it is.
            out.extend_from_slice(&[0xFF, marker]);
            out.extend_from_slice(&data[p..]);
            return Ok(out);
        }
        let len = u16::from_be_bytes([
            *data.get(p).ok_or_else(bad)?,
            *data.get(p + 1).ok_or_else(bad)?,
        ]) as usize;
        let seg = data.get(p..p + len).ok_or_else(bad)?;
        let metadata = matches!(marker, 0xE1..=0xED | 0xEF | 0xFE);
        if !metadata {
            out.extend_from_slice(&[0xFF, marker]);
            out.extend_from_slice(seg);
        }
        p += len;
    }
}

// ---------------------------------------------------------------- ISO base media (MP4, MOV, 3GP)

/// One box: its type, where it starts, where its body starts, where it ends.
struct IsoBox {
    kind: [u8; 4],
    start: usize,
    body: usize,
    end: usize,
}

/// The boxes in `data[from..to]`. Stops (returning what it found and `false`) at the first size
/// that does not fit — trailing bytes after the last box, such as a camera's own trailer.
fn iso_boxes(data: &[u8], from: usize, to: usize) -> (Vec<IsoBox>, bool) {
    let mut out = Vec::new();
    let mut p = from;
    while p < to {
        let Some(head) = data.get(p..p + 8) else {
            return (out, false);
        };
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let kind = [head[4], head[5], head[6], head[7]];
        let (header, size) = match size32 {
            0 => (8u64, (to - p) as u64),
            1 => match data.get(p + 8..p + 16) {
                Some(b) => (16, u64::from_be_bytes(b.try_into().unwrap_or_default())),
                None => return (out, false),
            },
            n => (8, n),
        };
        if size < header || size > (to - p) as u64 {
            return (out, false);
        }
        out.push(IsoBox {
            kind,
            start: p,
            body: p + header as usize,
            end: p + size as usize,
        });
        p += size as usize;
    }
    (out, true)
}

/// Boxes that only describe the recording: user data (`©xyz` location, `©mak`/`©mod` device,
/// Samsung's `smta`/`auth`…), metadata (`keys`/`ilst`, where Apple keeps location, make, model and
/// software), and `uuid` boxes outside fragments (XMP).
const ISO_METADATA: &[&[u8; 4]] = &[b"udta", b"meta", b"uuid", b"XMP_"];

fn blank_iso(data: &mut [u8], b: &IsoBox, done: &mut Scrubbed) {
    // Keep the size field as it is (8 or 16 bytes of header); only the type changes.
    data[b.start + 4..b.start + 8].copy_from_slice(b"free");
    data[b.body..b.end].fill(0);
    done.blanked += 1;
}

/// Zero the creation and modification times of an `mvhd`, `tkhd` or `mdhd` full box. Version 0
/// stores each in 4 bytes after the version/flags word, version 1 in 8.
fn zero_iso_times(data: &mut [u8], b: &IsoBox, done: &mut Scrubbed) {
    let Some(&version) = data.get(b.body) else {
        return;
    };
    let width = if version == 1 { 8 } else { 4 };
    let times = b.body + 4..b.body + 4 + 2 * width;
    if times.end <= b.end {
        data[times].fill(0);
        done.times += 2;
    }
}

fn scrub_iso(data: &mut [u8], done: &mut Scrubbed) -> anyhow::Result<()> {
    let (top, _) = iso_boxes(data, 0, data.len());
    anyhow::ensure!(
        top.first().is_some_and(|b| matches!(
            &b.kind,
            b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip"
        )),
        "not a video this app can clean"
    );
    let mut saw_moov = false;
    for b in &top {
        match &b.kind {
            b"moov" => {
                saw_moov = true;
                scrub_moov(data, b, done)?;
            }
            k if ISO_METADATA.contains(&k) => blank_iso(data, b, done),
            _ => {}
        }
    }
    anyhow::ensure!(saw_moov, "the video's header could not be read");
    // Whatever follows the last box (a camera's own trailer — Samsung appends one) is not part of
    // the movie: players never read it. Zero it, since it can only be metadata — unless it starts
    // like a box of the movie itself, which means a file cut short, not a trailer.
    let last = top.last().map_or(0, |b| b.end);
    let truncated_box = data
        .get(last + 4..last + 8)
        .is_some_and(|k| matches!(k, b"mdat" | b"moof" | b"mfra" | b"moov"));
    if last < data.len() && !truncated_box {
        data[last..].fill(0);
        done.blanked += 1;
    }
    Ok(())
}

fn scrub_moov(data: &mut [u8], moov: &IsoBox, done: &mut Scrubbed) -> anyhow::Result<()> {
    let (children, whole) = iso_boxes(data, moov.body, moov.end);
    anyhow::ensure!(whole, "the video's header could not be read");
    for b in &children {
        match &b.kind {
            b"mvhd" => zero_iso_times(data, b, done),
            b"trak" => {
                let (inner, whole) = iso_boxes(data, b.body, b.end);
                anyhow::ensure!(whole, "the video's header could not be read");
                for t in &inner {
                    match &t.kind {
                        b"tkhd" => zero_iso_times(data, t, done),
                        b"mdia" => {
                            let (m, whole) = iso_boxes(data, t.body, t.end);
                            anyhow::ensure!(whole, "the video's header could not be read");
                            for x in m.iter().filter(|x| &x.kind == b"mdhd") {
                                zero_iso_times(data, x, done);
                            }
                        }
                        k if ISO_METADATA.contains(&k) => blank_iso(data, t, done),
                        _ => {}
                    }
                }
            }
            k if ISO_METADATA.contains(&k) => blank_iso(data, b, done),
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- Matroska / WebM

const EBML_HEADER: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEG_INFO: u32 = 0x1549_A966;
const SEG_TAGS: u32 = 0x1254_C367;
const SEG_ATTACHMENTS: u32 = 0x1941_A469;
const INFO_DATE_UTC: u32 = 0x4461;
const INFO_TITLE: u32 = 0x7BA9;

/// An element ID (1–4 bytes, marker bits kept, as the spec writes them) at `p`: `(id, length)`.
fn ebml_id(data: &[u8], p: usize) -> Option<(u32, usize)> {
    let first = *data.get(p)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return None;
    }
    let bytes = data.get(p..p + len)?;
    Some((bytes.iter().fold(0u32, |a, &b| a << 8 | b as u32), len))
}

/// An element size (1–8 bytes) at `p`: `(size, length)`, size `None` for "unknown" (all ones).
fn ebml_size(data: &[u8], p: usize) -> Option<(Option<u64>, usize)> {
    let first = *data.get(p)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let bytes = data.get(p..p + len)?;
    let mut v = (first as u64) & (0xFF >> len);
    for &b in &bytes[1..] {
        v = v << 8 | b as u64;
    }
    let unknown = v == (1u64 << (7 * len)) - 1;
    Some(((!unknown).then_some(v), len))
}

/// One element: ID, where it starts, where its body starts, where it ends (`None`: unknown size).
struct Ebml {
    id: u32,
    start: usize,
    body: usize,
    end: Option<usize>,
}

fn ebml_at(data: &[u8], p: usize, limit: usize) -> Option<Ebml> {
    let (id, il) = ebml_id(data, p)?;
    let (size, sl) = ebml_size(data, p + il)?;
    let body = p + il + sl;
    let end = match size {
        Some(s) => {
            let end = body.checked_add(usize::try_from(s).ok()?)?;
            (end <= limit).then_some(end)?;
            Some(end)
        }
        None => None,
    };
    Some(Ebml {
        id,
        start: p,
        body,
        end,
    })
}

/// Turn `data[start..end]` into a Void element of exactly that length, contents zeroed.
fn ebml_void(data: &mut [u8], start: usize, end: usize, done: &mut Scrubbed) {
    let len = end - start;
    data[start..end].fill(0);
    data[start] = 0xEC;
    if len >= 9 {
        // An 8-byte size: 0x01 then the remaining length in 7 bytes.
        let rest = (len - 9) as u64;
        data[start + 1] = 0x01;
        data[start + 2..start + 9].copy_from_slice(&rest.to_be_bytes()[1..]);
    } else if len >= 2 {
        data[start + 1] = 0x80 | (len - 2) as u8;
    }
    done.blanked += 1;
}

fn scrub_matroska(data: &mut [u8], done: &mut Scrubbed) -> anyhow::Result<()> {
    let bad = || anyhow::anyhow!("not a video this app can clean");
    let header = ebml_at(data, 0, data.len()).ok_or_else(bad)?;
    anyhow::ensure!(header.id == EBML_HEADER, "not a video this app can clean");
    let seg_at = header.end.ok_or_else(bad)?;
    let seg = ebml_at(data, seg_at, data.len()).ok_or_else(bad)?;
    anyhow::ensure!(seg.id == SEGMENT, "not a video this app can clean");
    let seg_end = seg.end.unwrap_or(data.len());
    // Walk the segment's top level. A live recording (a browser's MediaRecorder) writes clusters
    // of unknown size; the walk stops there, and such files put no tags before their clusters.
    let mut p = seg.body;
    while p < seg_end {
        let Some(el) = ebml_at(data, p, seg_end) else {
            break;
        };
        let Some(end) = el.end else {
            break;
        };
        match el.id {
            SEG_TAGS | SEG_ATTACHMENTS => ebml_void(data, el.start, end, done),
            SEG_INFO => {
                let mut q = el.body;
                while q < end {
                    let Some(c) = ebml_at(data, q, end) else {
                        break;
                    };
                    let Some(c_end) = c.end else {
                        break;
                    };
                    match c.id {
                        INFO_DATE_UTC => {
                            data[c.body..c_end].fill(0);
                            done.times += 1;
                        }
                        INFO_TITLE => ebml_void(data, c.start, c_end, done),
                        _ => {}
                    }
                    q = c_end;
                }
            }
            _ => {}
        }
        p = end;
    }
    Ok(())
}

// ---------------------------------------------------------------- AVI

/// RIFF chunks in `data[from..to]`: `(fourcc, start, body, end)`, bodies padded to even length.
fn riff_chunks(data: &[u8], from: usize, to: usize) -> Vec<([u8; 4], usize, usize, usize)> {
    let mut out = Vec::new();
    let mut p = from;
    while let Some(head) = data.get(p..p + 8) {
        if p + 8 > to {
            break;
        }
        let id = [head[0], head[1], head[2], head[3]];
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let end = p + 8 + size;
        if end > to {
            break;
        }
        out.push((id, p, p + 8, end));
        p = end + (size & 1);
    }
    out
}

fn junk(data: &mut [u8], start: usize, body: usize, end: usize, done: &mut Scrubbed) {
    data[start..start + 4].copy_from_slice(b"JUNK");
    data[body..end].fill(0);
    done.blanked += 1;
}

/// AVI keeps its descriptive text in `LIST INFO` (title, software, date) and the recording date in
/// `IDIT` inside `LIST hdrl`. Both become `JUNK`, the format's own padding.
fn scrub_avi(data: &mut [u8], done: &mut Scrubbed) -> anyhow::Result<()> {
    let riff_end = (8 + u32::from_le_bytes(data[4..8].try_into()?) as usize).min(data.len());
    for (id, start, body, end) in riff_chunks(data, 12, riff_end) {
        if &id != b"LIST" || body + 4 > end {
            continue;
        }
        match &data[body..body + 4] {
            b"INFO" => junk(data, start, body, end, done),
            b"hdrl" => {
                for (cid, cs, cb, ce) in riff_chunks(data, body + 4, end) {
                    if &cid == b"IDIT" {
                        junk(data, cs, cb, ce, done);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A real (tiny) JPEG for tests that send a photo.
#[cfg(test)]
pub(crate) const TEST_JPEG: &[u8] = include_bytes!("../../tests/fixtures/small-baseline.jpg");

/// A minimal well-formed MP4 for tests that send a video.
#[cfg(test)]
pub(crate) fn test_mp4() -> Vec<u8> {
    super::media_kind::sample_mp4(b"isom", 1, b"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::media_kind::{iso_box, video_ext};

    fn full_box(kind: &[u8; 4], version: u8, rest: &[u8]) -> Vec<u8> {
        let mut body = vec![version, 0, 0, 0];
        body.extend_from_slice(rest);
        iso_box(kind, &body)
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// A phone-shaped MP4: location and device in `moov/udta`, Apple-style `meta`, a track with its
    /// own `udta`, times in every header, a camera trailer after `mdat`.
    fn phone_mp4() -> Vec<u8> {
        let times_v0 = [0x11u8; 8]; // creation + modification, 4 bytes each
        let times_v1 = [0x22u8; 16];
        let udta = iso_box(
            b"udta",
            &[
                iso_box(b"\xA9xyz", b"\x00\x12\x15\xC7+45.4215-075.6972/"),
                iso_box(b"\xA9mod", b"SM-S931W"),
            ]
            .concat(),
        );
        let meta = full_box(b"meta", 0, &iso_box(b"keys", b"com.apple.quicktime.make"));
        let mdhd = full_box(b"mdhd", 1, &[&times_v1[..], &[0u8; 12]].concat());
        let dinf = iso_box(
            b"dinf",
            &iso_box(
                b"dref",
                &[
                    &[0u8, 0, 0, 0, 0, 0, 0, 1][..],
                    &iso_box(b"url ", &[0, 0, 0, 1]),
                ]
                .concat(),
            ),
        );
        let mdia = iso_box(b"mdia", &[mdhd, iso_box(b"minf", &dinf)].concat());
        let tkhd = full_box(b"tkhd", 0, &[&times_v0[..], &[0u8; 72]].concat());
        let trak = iso_box(
            b"trak",
            &[
                tkhd,
                mdia,
                iso_box(b"udta", &iso_box(b"name", b"Galaxy cam")),
            ]
            .concat(),
        );
        let mvhd = full_box(b"mvhd", 0, &[&times_v0[..], &[0u8; 88]].concat());
        let moov = iso_box(b"moov", &[mvhd, trak, udta, meta].concat());
        [
            iso_box(b"ftyp", b"mp42\0\0\0\0isommp42"),
            moov,
            iso_box(b"mdat", &[0xAB; 64]),
            b"SEFHsamsung-trailer".to_vec(),
        ]
        .concat()
    }

    #[test]
    fn an_mp4_loses_its_location_device_and_times_and_nothing_else() {
        let original = phone_mp4();
        let mut v = original.clone();
        let done = scrub_video(&mut v).unwrap();
        assert_eq!(v.len(), original.len(), "edited in place");
        for gone in [
            &b"+45.4215"[..],
            b"SM-S931W",
            b"quicktime.make",
            b"Galaxy cam",
            b"samsung",
        ] {
            assert!(
                !contains(&v, gone),
                "{:?} survived",
                String::from_utf8_lossy(gone)
            );
        }
        assert!(
            !contains(&v, &[0x11; 8]) && !contains(&v, &[0x22; 16]),
            "times zeroed"
        );
        assert_eq!(done.times, 6, "mvhd, tkhd, mdhd: two each");
        assert!(done.blanked >= 4, "{done:?}");
        // The movie data is untouched, and the file is still one the recipient will open.
        assert!(contains(&v, &[0xAB; 64]));
        assert_eq!(video_ext(&v), Some("mp4"));
    }

    #[test]
    fn a_photo_keeps_only_what_decoding_needs() {
        let real = include_bytes!("../../tests/fixtures/small-baseline.jpg");
        // Splice an EXIF segment with a location, an XMP one and a comment in after SOI.
        let seg = |m: u8, body: &[u8]| {
            let mut v = vec![0xFF, m];
            v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
            v.extend_from_slice(body);
            v
        };
        let tagged = [
            &real[..2],
            &seg(0xE1, b"Exif\0\0GPS +45.4215-075.6972 SAMSUNG")[..],
            &seg(0xE1, b"http://ns.adobe.com/xap/1.0/ <xmp>Galaxy</xmp>"),
            &seg(0xFE, b"taken at the cottage"),
            &real[2..],
        ]
        .concat();
        let out = scrub_jpeg(&tagged).unwrap();
        for gone in [&b"GPS"[..], b"SAMSUNG", b"Galaxy", b"cottage", b"Exif"] {
            assert!(!contains(&out, gone));
        }
        assert!(crate::node::media_kind::displayable_image(&out));
        // The real file had nothing to drop beyond what we added: same bytes back, but for its
        // own JFIF and coding segments, which are kept.
        assert_eq!(scrub_jpeg(real).unwrap(), real.to_vec());
        assert!(scrub_jpeg(include_bytes!("../../tests/fixtures/small.png")).is_err());
        for n in 0..tagged.len() {
            let _ = scrub_jpeg(&tagged[..n]);
        }
    }

    /// Real files: `NIGHTDROP_SCRUB_IN=<dir> NIGHTDROP_SCRUB_OUT=<dir> cargo test -p nightdrop
    /// real_videos_scrub -- --ignored` writes each cleaned file to OUT, for ffprobe/ffmpeg to check
    /// that the metadata is gone and the decoded frames are unchanged.
    #[test]
    #[ignore]
    fn real_videos_scrub() {
        let out = std::path::PathBuf::from(std::env::var("NIGHTDROP_SCRUB_OUT").unwrap());
        for e in std::fs::read_dir(std::env::var("NIGHTDROP_SCRUB_IN").unwrap()).unwrap() {
            let path = e.unwrap().path();
            let mut data = std::fs::read(&path).unwrap();
            match scrub_video(&mut data) {
                Ok(done) => {
                    eprintln!("{}: {done:?}", path.display());
                    std::fs::write(out.join(path.file_name().unwrap()), data).unwrap();
                }
                Err(e) => eprintln!("{}: REFUSED {e}", path.display()),
            }
        }
    }

    #[test]
    fn a_file_whose_header_cannot_be_read_is_refused() {
        assert!(scrub_video(&mut b"not a video at all".to_vec()).is_err());
        // An MP4 with no moov (cut off before it): nothing can be checked, so it is not sent.
        let mut cut = [
            iso_box(b"ftyp", b"mp42\0\0\0\0"),
            iso_box(b"mdat", &[1; 32]),
        ]
        .concat();
        assert!(scrub_video(&mut cut).is_err());
        // A moov whose children do not add up.
        let mut bad = [
            iso_box(b"ftyp", b"mp42\0\0\0\0"),
            iso_box(b"moov", &[0, 0, 0, 99, b'u']),
        ]
        .concat();
        assert!(scrub_video(&mut bad).is_err());
    }

    #[test]
    fn truncated_input_never_panics() {
        let full = phone_mp4();
        for n in 0..full.len() {
            let _ = scrub_video(&mut full[..n].to_vec());
        }
    }

    fn ebml(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.push(0x01); // 8-byte size
        v.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn a_matroska_file_loses_its_tags_title_and_date() {
        let info = ebml(
            &[0x15, 0x49, 0xA9, 0x66],
            &[
                ebml(&[0x44, 0x61], &[0x33; 8]),
                ebml(&[0x7B, 0xA9], b"Holiday at the cottage"),
                ebml(&[0x4D, 0x80], b"Lavf"),
            ]
            .concat(),
        );
        let tags = ebml(&[0x12, 0x54, 0xC3, 0x67], b"LOCATION +45.42-075.69");
        let cluster = ebml(&[0x1F, 0x43, 0xB6, 0x75], &[0xCD; 32]);
        let segment = ebml(&[0x18, 0x53, 0x80, 0x67], &[info, cluster, tags].concat());
        let original = [
            ebml(&[0x1A, 0x45, 0xDF, 0xA3], b"\x42\x82\x84webm"),
            segment,
        ]
        .concat();
        let mut v = original.clone();
        let done = scrub_video(&mut v).unwrap();
        assert_eq!(v.len(), original.len());
        for gone in [&b"cottage"[..], b"LOCATION", &[0x33; 8]] {
            assert!(!contains(&v, gone));
        }
        assert!(contains(&v, &[0xCD; 32]) && contains(&v, b"Lavf"));
        assert_eq!(
            done,
            Scrubbed {
                blanked: 2,
                times: 1
            }
        );
        // The voided elements still parse, at their old lengths.
        let seg = ebml_at(&v, ebml_at(&v, 0, v.len()).unwrap().end.unwrap(), v.len()).unwrap();
        let mut p = seg.body;
        while p < seg.end.unwrap() {
            p = ebml_at(&v, p, v.len()).unwrap().end.unwrap();
        }
        assert_eq!(p, v.len());
    }

    #[test]
    fn an_avi_loses_its_info_and_recording_date() {
        let chunk = |id: &[u8; 4], body: &[u8]| {
            let mut v = id.to_vec();
            v.extend_from_slice(&(body.len() as u32).to_le_bytes());
            v.extend_from_slice(body);
            if body.len() % 2 == 1 {
                v.push(0);
            }
            v
        };
        let hdrl = chunk(
            b"LIST",
            &[&b"hdrl"[..], &chunk(b"IDIT", b"Mon Oct 13 2026\0")].concat(),
        );
        let info = chunk(
            b"LIST",
            &[&b"INFO"[..], &chunk(b"ISFT", b"CameraSoft 1.2")].concat(),
        );
        let movi = chunk(b"LIST", &[&b"movi"[..], &[0xEE; 16]].concat());
        let body = [&b"AVI "[..], &hdrl, &info, &movi].concat();
        let mut v = chunk(b"RIFF", &body);
        scrub_video(&mut v).unwrap();
        assert!(!contains(&v, b"Oct 13") && !contains(&v, b"CameraSoft"));
        assert!(contains(&v, &[0xEE; 16]));
        assert_eq!(video_ext(&v), Some("avi"));
    }
}
