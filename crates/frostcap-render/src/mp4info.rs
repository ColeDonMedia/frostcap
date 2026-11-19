//! Tiny ISO-BMFF (MP4) box walker used to validate exports: tracks, codecs,
//! durations, sample counts. Read-only, no allocation of media data.

use std::path::Path;

use crate::error::{RenderError, Result};

/// One track.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackInfo {
    /// Handler type (`vide`, `soun`).
    pub handler: String,
    /// Sample entry fourcc (`avc1`, `mp4a`, ...).
    pub codec: String,
    /// Media duration in seconds.
    pub duration_s: f64,
    /// Number of samples (`stsz`).
    pub samples: u32,
    /// Coded width/height for video sample entries.
    pub size: Option<(u16, u16)>,
    /// Audio channels / sample rate for audio sample entries.
    pub audio: Option<(u16, u32)>,
}

/// Summary of a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Mp4Info {
    /// `ftyp` major brand.
    pub brand: String,
    /// Movie duration (`mvhd`) in seconds.
    pub duration_s: f64,
    /// Tracks in file order.
    pub tracks: Vec<TrackInfo>,
}

fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn be64(b: &[u8], i: usize) -> u64 {
    ((be32(b, i) as u64) << 32) | be32(b, i + 4) as u64
}

/// Iterate child boxes: (fourcc, body).
fn boxes(b: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        if i + 8 > b.len() {
            return None;
        }
        let mut size = be32(b, i) as usize;
        let mut hdr = 8;
        if size == 1 && i + 16 <= b.len() {
            size = be64(b, i + 8) as usize;
            hdr = 16;
        } else if size == 0 {
            size = b.len() - i;
        }
        if size < hdr || i.checked_add(size).is_none_or(|end| end > b.len()) {
            return None;
        }
        let out = (&b[i + 4..i + 8], &b[i + hdr..i + size]);
        i += size;
        Some(out)
    })
}

fn child<'a>(b: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    boxes(b).find(|(n, _)| *n == name).map(|(_, body)| body)
}

/// (timescale, duration) from a full box with version 0/1 layout (mvhd/mdhd).
fn time_header(b: &[u8]) -> Option<(u32, u64)> {
    if b.is_empty() {
        return None;
    }
    if b[0] == 1 {
        (b.len() >= 32).then(|| (be32(b, 20), be64(b, 24)))
    } else {
        (b.len() >= 20).then(|| (be32(b, 12), be32(b, 16) as u64))
    }
}

/// Parse an MP4 file in memory.
pub fn parse(bytes: &[u8]) -> Option<Mp4Info> {
    let brand = child(bytes, b"ftyp")
        .filter(|f| f.len() >= 4)
        .map(|f| String::from_utf8_lossy(&f[0..4]).into_owned())?;
    let moov = child(bytes, b"moov")?;
    let (ts, dur) = time_header(child(moov, b"mvhd")?)?;
    let mut tracks = Vec::new();
    for (n, trak) in boxes(moov) {
        if n != b"trak" {
            continue;
        }
        let Some(mdia) = child(trak, b"mdia") else {
            continue;
        };
        let (mts, mdur) = child(mdia, b"mdhd").and_then(time_header).unwrap_or((1, 0));
        let handler = child(mdia, b"hdlr")
            .filter(|h| h.len() >= 12)
            .map(|h| String::from_utf8_lossy(&h[8..12]).into_owned())
            .unwrap_or_default();
        let stbl = child(mdia, b"minf").and_then(|m| child(m, b"stbl"));
        let (mut codec, mut size, mut audio, mut samples) = (String::new(), None, None, 0);
        if let Some(stbl) = stbl {
            if let Some(stsd) = child(stbl, b"stsd").filter(|s| s.len() >= 8)
                && let Some((fourcc, entry)) = boxes(&stsd[8..]).next()
            {
                codec = String::from_utf8_lossy(fourcc).into_owned();
                if handler == "vide" && entry.len() >= 28 {
                    size = Some((
                        u16::from_be_bytes([entry[24], entry[25]]),
                        u16::from_be_bytes([entry[26], entry[27]]),
                    ));
                } else if handler == "soun" && entry.len() >= 28 {
                    audio = Some((
                        u16::from_be_bytes([entry[16], entry[17]]),
                        be32(entry, 24) >> 16,
                    ));
                }
            }
            if let Some(stsz) = child(stbl, b"stsz").filter(|s| s.len() >= 12) {
                samples = be32(stsz, 8);
            }
        }
        tracks.push(TrackInfo {
            handler,
            codec,
            duration_s: mdur as f64 / mts.max(1) as f64,
            samples,
            size,
            audio,
        });
    }
    Some(Mp4Info {
        brand,
        duration_s: dur as f64 / ts.max(1) as f64,
        tracks,
    })
}

/// Largest `moov` / `moof` box body we buffer (the media data is never read).
const MAX_HEADER_BOX: u64 = 256 << 20;

/// Sum of the sample durations of one `traf` (video fragments), in track ticks.
fn traf_ticks(traf: &[u8], default: u32) -> u64 {
    let mut def = u64::from(default);
    if let Some(tfhd) = child(traf, b"tfhd").filter(|t| t.len() >= 8) {
        let flags = be32(tfhd, 0) & 0x00FF_FFFF;
        let mut at = 8usize;
        if flags & 0x1 != 0 {
            at += 8;
        }
        if flags & 0x2 != 0 {
            at += 4;
        }
        if flags & 0x8 != 0 && at + 4 <= tfhd.len() {
            def = u64::from(be32(tfhd, at));
        }
    }
    let mut total = 0u64;
    for (n, trun) in boxes(traf) {
        if n != b"trun" || trun.len() < 8 {
            continue;
        }
        let (flags, count) = (be32(trun, 0) & 0x00FF_FFFF, be32(trun, 4));
        let mut at = 8usize;
        if flags & 0x1 != 0 {
            at += 4;
        }
        if flags & 0x4 != 0 {
            at += 4;
        }
        let per = [0x100u32, 0x200, 0x400, 0x800]
            .iter()
            .filter(|f| flags & **f != 0)
            .count()
            * 4;
        for _ in 0..count.min(10_000_000) {
            let d = if flags & 0x100 != 0 && at + 4 <= trun.len() {
                u64::from(be32(trun, at))
            } else {
                def
            };
            total = total.saturating_add(d);
            at = at.saturating_add(per);
            if at > trun.len() {
                break;
            }
        }
    }
    total
}

/// Real duration (ms) of a (possibly fragmented, possibly torn) MP4, reading only
/// the `moov` and `moof` headers, never the media data. Recordings are fragmented
/// MP4s whose `moov` carries no duration, so the decoder cannot tell us either.
/// `None` when the file is not an MP4 or has no timescale / duration.
pub fn stream_duration_ms(path: &Path) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let (mut pos, mut moov): (u64, Option<Vec<u8>>) = (0, None);
    let (mut ticks, mut fragmented) = (0u64, false);
    let (mut timescale, mut trex_default) = (0u32, 0u32);
    let mut movie: Option<(u32, u64)> = None;
    let mut saw_ftyp = false;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; 16];
        let got = f.read(&mut h).ok()?;
        if got < 8 {
            break;
        }
        let s32 = u64::from(be32(&h, 0));
        let (size, hdr) = match s32 {
            1 if got >= 16 => (u64::from_be_bytes(h[8..16].try_into().ok()?), 16u64),
            0 => (len - pos, 8),
            s => (s, 8),
        };
        if size < hdr || pos.checked_add(size).is_none_or(|e| e > len) {
            break; // torn tail
        }
        let ty: [u8; 4] = h[4..8].try_into().ok()?;
        match &ty {
            b"ftyp" => saw_ftyp = true,
            b"moov" | b"moof" if size - hdr <= MAX_HEADER_BOX => {
                let mut body = vec![0u8; (size - hdr) as usize];
                f.seek(SeekFrom::Start(pos + hdr)).ok()?;
                f.read_exact(&mut body).ok()?;
                if &ty == b"moov" {
                    for (n, trak) in boxes(&body) {
                        if n != b"trak" {
                            continue;
                        }
                        let Some(mdia) = child(trak, b"mdia") else {
                            continue;
                        };
                        let vide =
                            child(mdia, b"hdlr").is_some_and(|h| h.get(8..12) == Some(b"vide"));
                        if let Some((ts, _)) = child(mdia, b"mdhd").and_then(time_header)
                            && (vide || timescale == 0)
                        {
                            timescale = ts;
                        }
                    }
                    trex_default = child(&body, b"mvex")
                        .and_then(|m| child(m, b"trex"))
                        .filter(|t| t.len() >= 16)
                        .map_or(0, |t| be32(t, 12));
                    movie = child(&body, b"mvhd").and_then(time_header);
                    moov = Some(body);
                } else {
                    fragmented = true;
                    for (n, traf) in boxes(&body) {
                        if n == b"traf" {
                            ticks = ticks.saturating_add(traf_ticks(traf, trex_default));
                        }
                    }
                }
            }
            _ => {}
        }
        pos += size;
    }
    if !saw_ftyp || moov.is_none() {
        return None;
    }
    let ms = if fragmented && timescale > 0 {
        ticks as f64 * 1000.0 / f64::from(timescale)
    } else {
        let (ts, dur) = movie?;
        dur as f64 * 1000.0 / f64::from(ts.max(1))
    };
    (ms > 0.0).then_some(ms)
}

/// Parse an MP4 file from disk.
pub fn parse_file(path: &Path) -> Result<Mp4Info> {
    let bytes = std::fs::read(path).map_err(|e| RenderError::io(path, e))?;
    parse(&bytes)
        .ok_or_else(|| RenderError::Media(format!("{}: not a parsable MP4", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    /// Fragmented MP4 (10 kHz video track, duration unknown in the moov) like a recording.
    fn fragmented(fragments: &[&[u32]]) -> Vec<u8> {
        let mut mdhd = vec![0u8; 4];
        mdhd.extend_from_slice(&[0; 8]);
        mdhd.extend_from_slice(&10_000u32.to_be_bytes());
        mdhd.extend_from_slice(&[0; 8]);
        let hdlr: Vec<u8> = [0u8; 8].iter().chain(b"vide").copied().collect();
        let mdia = bx(b"mdia", &[bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr)].concat());
        let mut f = bx(b"ftyp", b"iso5\0\0\0\0iso5");
        f.extend(bx(
            b"moov",
            &[bx(b"trak", &mdia), bx(b"mvex", &[])].concat(),
        ));
        for d in fragments {
            let mut trun = vec![0, 0, 0x01, 0x00];
            trun.extend_from_slice(&(d.len() as u32).to_be_bytes());
            d.iter()
                .for_each(|x| trun.extend_from_slice(&x.to_be_bytes()));
            let tfhd = bx(b"tfhd", &[0, 0, 0, 0, 0, 0, 0, 1]);
            f.extend(bx(
                b"moof",
                &bx(b"traf", &[tfhd, bx(b"trun", &trun)].concat()),
            ));
            f.extend(bx(b"mdat", &[0u8; 16]));
        }
        f
    }

    #[test]
    fn fragmented_recordings_report_their_real_duration() {
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .join("test-tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("frag-{}.mp4", std::process::id()));
        std::fs::write(&f, fragmented(&[&[1000, 1000, 1000], &[1000, 1000]])).unwrap();
        assert_eq!(stream_duration_ms(&f), Some(500.0)); // 5 * 1000 ticks at 10 kHz
        // A torn tail (crashed recording) is ignored.
        let mut torn = fragmented(&[&[1000, 1000]]);
        torn.extend_from_slice(&[0, 0, 0, 40, b'm', b'o', b'o', b'f', 1, 2, 3]);
        std::fs::write(&f, torn).unwrap();
        assert_eq!(stream_duration_ms(&f), Some(200.0));
        std::fs::write(&f, [0xFFu8; 64]).unwrap();
        assert_eq!(stream_duration_ms(&f), None);
        let _ = std::fs::remove_file(&f);
    }
    #[test]
    fn hostile_box_sizes_never_overflow() {
        // 64-bit `largesize` of u64::MAX in a box at offset 8 would overflow `i + size`.
        let mut b = Vec::new();
        b.extend(16u32.to_be_bytes());
        b.extend(b"ftypmp42\0\0\0\0\0\0\0\0".iter().take(8));
        b.extend(1u32.to_be_bytes());
        b.extend(b"moov");
        b.extend(u64::MAX.to_be_bytes());
        b.extend([0u8; 16]);
        assert!(parse(&b).is_none());
        assert_eq!(boxes(&b).count(), 1); // the ftyp, then the bogus box ends the walk
        // Plain garbage.
        assert!(parse(&[0xFF; 64]).is_none());
        assert!(parse(&[]).is_none());
    }
}
