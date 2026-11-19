//! Minimal MP4 / fragmented-MP4 inspection (duration, finalized state). Used by
//! recovery and the smoke tests; tolerant of a torn tail (crashed recording).

use std::path::Path;

/// What we learned about a file.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Mp4Info {
    /// Has a `moov` box (init segment / index).
    pub has_moov: bool,
    /// Uses `moof` fragments.
    pub fragmented: bool,
    /// Complete fragments found.
    pub fragments: u32,
    /// Duration (ms) from fragments, or from `mvhd` for a classic MP4.
    pub duration_ms: u64,
    /// Video width from `tkhd` (0 if unknown).
    pub width: u32,
    /// Video height from `tkhd` (0 if unknown).
    pub height: u32,
}

impl Mp4Info {
    /// A player can open it and it has content.
    pub fn playable(&self) -> bool {
        self.has_moov && self.duration_ms > 0
    }
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// Complete child boxes of `b` as (type, payload). Stops at the first torn box.
fn boxes(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= b.len() {
        let Some(size32) = be32(b, i) else { break };
        let mut size = size32 as usize;
        let mut hdr = 8;
        if size32 == 1 {
            let Some(s) = be64(b, i + 8) else { break };
            size = usize::try_from(s).unwrap_or(usize::MAX);
            hdr = 16;
        } else if size32 == 0 {
            size = b.len() - i;
        }
        if size < hdr || i.checked_add(size).is_none_or(|e| e > b.len()) {
            break;
        }
        let mut ty = [0u8; 4];
        ty.copy_from_slice(&b[i + 4..i + 8]);
        out.push((ty, &b[i + hdr..i + size]));
        i += size;
    }
    out
}

fn child<'a>(b: &'a [u8], ty: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).into_iter().find(|(t, _)| t == ty).map(|(_, p)| p)
}

/// (timescale, default sample duration from trex, width, height) of the video track.
fn track_params(moov: &[u8]) -> (u32, u32, u32, u32) {
    let mut timescale = 0;
    let (mut w, mut h) = (0, 0);
    for (t, trak) in boxes(moov) {
        if &t != b"trak" {
            continue;
        }
        let Some(mdia) = child(trak, b"mdia") else {
            continue;
        };
        let ts = child(mdia, b"mdhd").and_then(|m| {
            if m.first() == Some(&1) {
                be32(m, 20)
            } else {
                be32(m, 12)
            }
        });
        let is_video = child(mdia, b"hdlr").is_some_and(|h| h.get(8..12) == Some(b"vide"));
        if let Some(ts) = ts
            && (is_video || timescale == 0)
        {
            timescale = ts;
            if is_video {
                if let Some(tkhd) = child(trak, b"tkhd") {
                    let at = if tkhd.first() == Some(&1) { 88 } else { 76 };
                    w = be32(tkhd, at).map_or(0, |v| v >> 16);
                    h = be32(tkhd, at + 4).map_or(0, |v| v >> 16);
                }
                break;
            }
        }
    }
    let trex_default = child(moov, b"mvex")
        .and_then(|m| child(m, b"trex"))
        .and_then(|t| be32(t, 12))
        .unwrap_or(0);
    (timescale, trex_default, w, h)
}

fn traf_duration(traf: &[u8], default: u32) -> u64 {
    let mut def = u64::from(default);
    if let Some(tfhd) = child(traf, b"tfhd")
        && let Some(flags) = be32(tfhd, 0).map(|v| v & 0x00FF_FFFF)
    {
        let mut at = 8;
        if flags & 0x1 != 0 {
            at += 8;
        }
        if flags & 0x2 != 0 {
            at += 4;
        }
        if flags & 0x8 != 0
            && let Some(d) = be32(tfhd, at)
        {
            def = u64::from(d);
        }
    }
    let mut total = 0u64;
    for (t, trun) in boxes(traf) {
        if &t != b"trun" {
            continue;
        }
        let (Some(flags), Some(count)) = (be32(trun, 0).map(|v| v & 0x00FF_FFFF), be32(trun, 4))
        else {
            continue;
        };
        let mut at = 8usize;
        if flags & 0x1 != 0 {
            at += 4;
        }
        if flags & 0x4 != 0 {
            at += 4;
        }
        let per = [0x100, 0x200, 0x400, 0x800]
            .iter()
            .filter(|f| flags & **f != 0)
            .count()
            * 4;
        for _ in 0..count.min(10_000_000) {
            let d = if flags & 0x100 != 0 {
                be32(trun, at).map_or(0, u64::from)
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

/// Inspect MP4 bytes.
pub fn inspect_bytes(data: &[u8]) -> Option<Mp4Info> {
    let top = boxes(data);
    if top.is_empty() {
        return None;
    }
    let mut info = Mp4Info::default();
    let moov = top.iter().find(|(t, _)| t == b"moov").map(|(_, p)| *p);
    info.has_moov = moov.is_some();
    let (timescale, trex_default, width, height) = moov.map_or((0, 0, 0, 0), track_params);
    info.width = width;
    info.height = height;
    let mut ticks = 0u64;
    for (t, p) in &top {
        if t == b"moof" {
            info.fragmented = true;
            info.fragments += 1;
            for (ct, traf) in boxes(p) {
                if &ct == b"traf" {
                    ticks = ticks.saturating_add(traf_duration(traf, trex_default));
                }
            }
        }
    }
    if info.fragmented && timescale > 0 {
        info.duration_ms = ticks.saturating_mul(1000) / u64::from(timescale);
    } else if let Some(mvhd) = moov.and_then(|m| child(m, b"mvhd")) {
        let (ts, dur) = if mvhd.first() == Some(&1) {
            (be32(mvhd, 20)?, be64(mvhd, 24)?)
        } else {
            (be32(mvhd, 12)?, u64::from(be32(mvhd, 16)?))
        };
        if ts > 0 {
            info.duration_ms = dur.saturating_mul(1000) / u64::from(ts);
        }
    }
    Some(info)
}

/// Inspect an MP4 file (reads it fully; recordings are local and bounded by disk).
pub fn inspect(path: &Path) -> Option<Mp4Info> {
    inspect_bytes(&std::fs::read(path).ok()?)
}
/// Synthetic MP4 builders for tests.
#[cfg(test)]
pub(crate) mod test_util {
    pub(crate) fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    pub(crate) fn mdhd(timescale: u32) -> Vec<u8> {
        let mut p = vec![0u8; 4]; // version 0, flags
        p.extend_from_slice(&[0; 8]); // creation, modification
        p.extend_from_slice(&timescale.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // duration (unknown in fMP4)
        p.extend_from_slice(&[0; 4]); // language + pre_defined
        bx(b"mdhd", &p)
    }

    pub(crate) fn trun(durations: &[u32]) -> Vec<u8> {
        let mut p = vec![0, 0, 0x01, 0x00]; // flags: sample-duration-present
        p.extend_from_slice(&(durations.len() as u32).to_be_bytes());
        for d in durations {
            p.extend_from_slice(&d.to_be_bytes());
        }
        bx(b"trun", &p)
    }

    /// Fragmented MP4 with a 10 kHz video track and the given sample durations.
    pub(crate) fn fragmented(fragments: &[&[u32]]) -> Vec<u8> {
        let mut f = bx(b"ftyp", b"iso5\0\0\0\0iso5");
        let mdia = bx(
            b"mdia",
            &[
                mdhd(10_000),
                bx(
                    b"hdlr",
                    &[0u8; 8].iter().chain(b"vide").copied().collect::<Vec<_>>(),
                ),
            ]
            .concat(),
        );
        let moov = bx(b"moov", &[bx(b"trak", &mdia), bx(b"mvex", &[])].concat());
        f.extend(moov);
        for d in fragments {
            let tfhd = bx(b"tfhd", &[0, 0, 0, 0, 0, 0, 0, 1]);
            f.extend(bx(b"moof", &bx(b"traf", &[tfhd, trun(d)].concat())));
            f.extend(bx(b"mdat", &[0u8; 16]));
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;

    #[test]
    fn fragmented_duration_is_the_sum_of_fragments() {
        let data = fragmented(&[&[1000, 1000, 1000], &[1000, 1000]]);
        let i = inspect_bytes(&data).unwrap();
        assert!(i.fragmented && i.has_moov);
        assert_eq!(i.duration_ms, 500); // 5 * 1000 / 10_000 s
        assert_eq!(i.fragments, 2);
    }

    #[test]
    fn torn_last_fragment_is_ignored() {
        let mut data = fragmented(&[&[1000, 1000]]);
        let good = data.len();
        data.extend(fragmented(&[&[1000]])[good.min(40)..].iter().take(20)); // garbage tail
        let i = inspect_bytes(&data).unwrap();
        assert_eq!(i.duration_ms, 200);
    }

    #[test]
    fn missing_moov_is_not_playable() {
        let data = [bx(b"ftyp", b"isom\0\0\0\0"), bx(b"mdat", &[0; 32])].concat();
        let i = inspect_bytes(&data).unwrap();
        assert!(!i.has_moov && !i.playable());
    }
}
