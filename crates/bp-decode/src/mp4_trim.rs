//! Gapless trim for AAC in MP4: the encoder's priming and padding frames, read from the edit list
//! or, failing that, the iTunes `iTunSMPB` tag.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// The playable window of a track, in source frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Trim {
    /// Priming frames dropped from the start.
    pub skip: u64,
    /// Frames played after `skip`.
    pub frames: u64,
}

/// The largest `moov` read into memory; real ones are a few kilobytes to a few megabytes.
const MAX_MOOV: u64 = 64 << 20;

/// Reads the `moov` box of the MP4 starting `offset` bytes into `file`, without loading `mdat`.
pub(crate) fn read_moov(file: &mut File, offset: u64) -> Option<Vec<u8>> {
    let len = file.metadata().ok()?.len();
    let mut at = offset;
    while at < len {
        file.seek(SeekFrom::Start(at)).ok()?;
        let mut head = [0u8; 16];
        let n = crate::read_up_to(file, &mut head).ok()?;
        let (size, _) = box_size(&head[..n], len - at)?;
        if &head[4..8] == b"moov" {
            if size > MAX_MOOV {
                return None;
            }
            let mut moov = vec![0u8; usize::try_from(size).ok()?];
            file.seek(SeekFrom::Start(at)).ok()?;
            file.read_exact(&mut moov).ok()?;
            return Some(moov);
        }
        at += size;
    }
    None
}

/// Size and header length of the box at the start of `b`, which has `room` bytes left in its
/// parent. `None` for a truncated header, or a size smaller than its header or larger than `room`.
fn box_size(b: &[u8], room: u64) -> Option<(u64, u64)> {
    let size = u64::from(u32::from_be_bytes(b.get(0..4)?.try_into().ok()?));
    b.get(4..8)?;
    let (size, header) = match size {
        0 => (room, 8),
        1 => (u64::from_be_bytes(b.get(8..16)?.try_into().ok()?), 16),
        n => (n, 8),
    };
    (size >= header && size <= room).then_some((size, header))
}

/// The child boxes of `b` as `(type, body)`; stops at the first malformed box.
fn boxes(mut b: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    std::iter::from_fn(move || {
        let (size, header) = box_size(b, b.len() as u64)?;
        let (this, rest) = b.split_at(size as usize);
        b = rest;
        Some((&this[4..8], &this[header as usize..]))
    })
}

fn child<'a>(b: &'a [u8], kind: &[u8]) -> Option<&'a [u8]> {
    boxes(b).find(|(k, _)| *k == kind).map(|(_, body)| body)
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Timescale of an `mvhd` or `mdhd` body (version 0 or 1).
fn timescale(body: &[u8]) -> Option<u64> {
    let at = if *body.first()? == 1 { 20 } else { 12 };
    be_u32(body, at).map(u64::from).filter(|&t| t > 0)
}

/// The single non-empty `(segment_duration, media_time)` of an `elst` body.
fn single_edit(body: &[u8]) -> Option<(u64, u64)> {
    let v1 = *body.first()? == 1;
    let count = be_u32(body, 4)? as usize;
    let size = if v1 { 20 } else { 12 };
    let mut real = (0..count).map(|i| {
        let at = 8 + i * size;
        if v1 {
            Some((be_u64(body, at)?, be_u64(body, at + 8)? as i64))
        } else {
            Some((
                u64::from(be_u32(body, at)?),
                i64::from(be_u32(body, at + 4)? as i32),
            ))
        }
    });
    let mut found = None;
    for e in &mut real {
        let (seg, media) = e?;
        if media == -1 {
            continue;
        }
        if found.is_some() || media < 0 {
            return None;
        }
        found = Some((seg, media as u64));
    }
    found
}

fn edit_list_trim(moov: &[u8], sample_rate: u64) -> Option<Trim> {
    let movie_ts = timescale(child(moov, b"mvhd")?)?;
    let trak = boxes(moov)
        .filter(|(k, _)| *k == b"trak")
        .map(|(_, t)| t)
        .find(|t| {
            child(t, b"mdia")
                .and_then(|m| child(m, b"hdlr"))
                .and_then(|h| h.get(8..12))
                == Some(b"soun")
        })?;
    let media_ts = timescale(child(child(trak, b"mdia")?, b"mdhd")?)?;
    let (seg, media) = single_edit(child(child(trak, b"edts")?, b"elst")?)?;
    Some(Trim {
        skip: u64::try_from(u128::from(media) * u128::from(sample_rate) / u128::from(media_ts))
            .ok()?,
        frames: u64::try_from(
            (u128::from(seg) * u128::from(sample_rate) + u128::from(movie_ts) / 2)
                / u128::from(movie_ts),
        )
        .ok()?,
    })
}

fn itunsmpb_trim(moov: &[u8]) -> Option<Trim> {
    let meta = child(child(moov, b"udta")?, b"meta")?;
    let ilst = child(meta.get(4..)?, b"ilst")?;
    let item = boxes(ilst)
        .filter(|(k, _)| *k == b"----")
        .map(|(_, i)| i)
        .find(|i| child(i, b"name").and_then(|n| n.get(4..)) == Some(b"iTunSMPB"))?;
    let text = std::str::from_utf8(child(item, b"data")?.get(8..)?).ok()?;
    let fields: Vec<u64> = text
        .split_ascii_whitespace()
        .map(|f| u64::from_str_radix(f, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    Some(Trim {
        skip: *fields.get(1)?,
        frames: *fields.get(3)?,
    })
}

/// The trim from an MP4 image whose top-level boxes include `moov`. A window that is empty
/// or ends past `u64::MAX` frames is ignored.
pub(crate) fn trim_from(bytes: &[u8], sample_rate: u32) -> Option<Trim> {
    let moov = child(bytes, b"moov")?;
    edit_list_trim(moov, u64::from(sample_rate))
        .or_else(|| itunsmpb_trim(moov))
        .filter(|t| t.frames > 0 && t.skip.checked_add(t.frames).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = (8 + body.len() as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// `mvhd` or `mdhd` version 0: version/flags, creation, modification, timescale, duration.
    fn header(kind: &[u8; 4], timescale: u32) -> Vec<u8> {
        let mut b = vec![0u8; 12];
        b.extend_from_slice(&timescale.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        bx(kind, &b)
    }

    fn hdlr(handler: &[u8; 4]) -> Vec<u8> {
        let mut b = vec![0u8; 8];
        b.extend_from_slice(handler);
        b.extend_from_slice(&[0u8; 13]);
        bx(b"hdlr", &b)
    }

    /// `elst` version 0 with `(segment_duration, media_time)` entries.
    fn elst(entries: &[(u32, i32)]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for &(seg, media) in entries {
            b.extend_from_slice(&seg.to_be_bytes());
            b.extend_from_slice(&media.to_be_bytes());
            b.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        }
        bx(b"elst", &b)
    }

    /// `elst` version 1 with one `(segment_duration, media_time)` entry.
    fn elst_v1(seg: u64, media: i64) -> Vec<u8> {
        let mut b = vec![1, 0, 0, 0];
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&seg.to_be_bytes());
        b.extend_from_slice(&media.to_be_bytes());
        b.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        bx(b"elst", &b)
    }

    fn trak(handler: &[u8; 4], edits: Option<&[(u32, i32)]>) -> Vec<u8> {
        trak_with(handler, edits.map(elst))
    }

    fn trak_with(handler: &[u8; 4], elst: Option<Vec<u8>>) -> Vec<u8> {
        let mdia = bx(b"mdia", &cat(&[header(b"mdhd", 44_100), hdlr(handler)]));
        let mut parts = vec![bx(b"tkhd", &[0u8; 84])];
        if let Some(e) = elst {
            parts.push(bx(b"edts", &e));
        }
        parts.push(mdia);
        bx(b"trak", &cat(&parts))
    }

    fn file(traks: &[Vec<u8>], udta: Option<Vec<u8>>) -> Vec<u8> {
        let mut moov = vec![header(b"mvhd", 1000)];
        moov.extend_from_slice(traks);
        moov.extend(udta);
        cat(&[
            bx(b"ftyp", b"isomiso2mp41"),
            bx(b"mdat", &[0u8; 64]),
            bx(b"moov", &cat(&moov)),
        ])
    }

    const FFMPEG: Trim = Trim {
        skip: 1024,
        frames: 22_050,
    };

    #[test]
    fn ffmpeg_edit_list() {
        let f = file(&[trak(b"soun", Some(&[(500, 1024)]))], None);
        assert_eq!(trim_from(&f, 44_100), Some(FFMPEG));
    }

    #[test]
    fn empty_edit_is_ignored() {
        let f = file(&[trak(b"soun", Some(&[(10, -1), (500, 1024)]))], None);
        assert_eq!(trim_from(&f, 44_100), Some(FFMPEG));
    }

    #[test]
    fn two_real_edits_give_none() {
        let f = file(&[trak(b"soun", Some(&[(250, 1024), (250, 30_000)]))], None);
        assert_eq!(trim_from(&f, 44_100), None);
    }

    #[test]
    fn overflowing_edit_list_gives_none() {
        let f = file(&[trak_with(b"soun", Some(elst_v1(500, i64::MAX)))], None);
        assert_eq!(trim_from(&f, 1_000_000), None);
    }

    #[test]
    fn itunsmpb_fallback() {
        let f = itunsmpb_file(b" 00000000 00000840 000001CA 0000000000005622 00000000 00000000");
        assert_eq!(
            trim_from(&f, 44_100),
            Some(Trim {
                skip: 0x840,
                frames: 0x5622
            })
        );
    }

    #[test]
    fn overflowing_itunsmpb_gives_none() {
        let f = itunsmpb_file(b" 00000000 FFFFFFFFFFFFFFFF 00000000 0000000000000010");
        assert_eq!(trim_from(&f, 44_100), None);
    }

    /// A file with no edit list whose `iTunSMPB` tag reads `text`.
    fn itunsmpb_file(text: &[u8]) -> Vec<u8> {
        let mut mean = vec![0u8; 4];
        mean.extend_from_slice(b"com.apple.iTunes");
        let mut name = vec![0u8; 4];
        name.extend_from_slice(b"iTunSMPB");
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(text);
        let item = bx(
            b"----",
            &cat(&[bx(b"mean", &mean), bx(b"name", &name), bx(b"data", &data)]),
        );
        let mut meta = vec![0u8; 4];
        meta.extend(hdlr(b"mdir"));
        meta.extend(bx(b"ilst", &item));
        let udta = bx(b"udta", &bx(b"meta", &meta));
        file(&[trak(b"soun", None)], Some(udta))
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(trim_from(&[], 44_100), None);
        assert_eq!(
            trim_from(&[0, 0, 0, 3, b'm', b'o', b'o', b'v'], 44_100),
            None
        );
        assert_eq!(
            trim_from(&[0, 0, 0, 0, b'f', b'r', b'e', b'e'], 44_100),
            None
        );
        // A child claiming more bytes than its parent holds.
        let mut moov = bx(b"moov", &bx(b"trak", &[0u8; 16]));
        moov[11] = 200;
        assert_eq!(trim_from(&moov, 44_100), None);
        // A 64-bit largesize box (size 1) that runs past the end.
        let mut big = vec![0, 0, 0, 1];
        big.extend_from_slice(b"moov");
        big.extend_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(trim_from(&big, 44_100), None);
        // A truncated header.
        assert_eq!(trim_from(&[0, 0, 0, 16, b'm'], 44_100), None);
    }

    #[test]
    fn largesize_moov_is_read() {
        // A moov with a 16-byte largesize header.
        let plain = trak(b"soun", Some(&[(500, 1024)]));
        let body = cat(&[header(b"mvhd", 1000), plain]);
        let mut moov = vec![0, 0, 0, 1];
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&(16 + body.len() as u64).to_be_bytes());
        moov.extend_from_slice(&body);
        assert_eq!(trim_from(&moov, 44_100), Some(FFMPEG));
    }

    #[test]
    fn video_trak_is_skipped() {
        let f = file(
            &[
                trak(b"vide", Some(&[(500, 3000)])),
                trak(b"soun", Some(&[(500, 1024)])),
            ],
            None,
        );
        assert_eq!(trim_from(&f, 44_100), Some(FFMPEG));
    }
}
