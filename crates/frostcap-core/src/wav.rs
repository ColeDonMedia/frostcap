//! RIFF/WAVE and RF64 header handling shared by the writer ([`crate::audio`]) and
//! crash recovery.
//!
//! Frostcap WAVs reserve a 28-byte `JUNK` chunk right after `WAVE`. While the data
//! fits 32-bit sizes the file is plain RIFF; once it would not, the header is
//! patched in place to RF64 (EBU TR 3306): `RF64`, `ds64` with 64-bit sizes, and
//! `0xFFFFFFFF` in the 32-bit size fields. Readers must therefore accept both.

/// Size of the reserved `JUNK`/`ds64` payload.
pub const DS64_PAYLOAD: usize = 28;
/// Offset of the reserved chunk header (right after `RIFF<size>WAVE`).
const JUNK_POS: u64 = 12;

/// A fresh WAV header (everything up to and including the `data` chunk header).
#[derive(Clone, Debug)]
pub struct Header {
    /// Header bytes; sizes are zero until patched.
    pub bytes: Vec<u8>,
    /// Offset of the `data` chunk's 32-bit size field.
    pub data_size_pos: u64,
}

/// Build the header for a `fmt ` chunk (`WAVEFORMATEX`, possibly extensible).
pub fn header(fmt_chunk: &[u8]) -> Header {
    let mut b = Vec::with_capacity(64 + fmt_chunk.len());
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"JUNK");
    b.extend_from_slice(&(DS64_PAYLOAD as u32).to_le_bytes());
    b.extend_from_slice(&[0; DS64_PAYLOAD]);
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&(fmt_chunk.len() as u32).to_le_bytes());
    b.extend_from_slice(fmt_chunk);
    if fmt_chunk.len() % 2 == 1 {
        b.push(0);
    }
    b.extend_from_slice(b"data");
    let data_size_pos = b.len() as u64;
    b.extend_from_slice(&0u32.to_le_bytes());
    Header {
        bytes: b,
        data_size_pos,
    }
}

/// What [`parse`] learned about a WAV file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavInfo {
    /// Sample rate.
    pub rate: u32,
    /// Channels.
    pub channels: u16,
    /// Bytes per frame.
    pub block: u16,
    /// Offset of the first sample byte.
    pub data_pos: u64,
    /// Offset of the `data` chunk's 32-bit size field.
    pub data_size_pos: u64,
    /// Offset of the reserved `JUNK`/`ds64` chunk, if the file has one.
    pub ds64_pos: Option<u64>,
    /// Declared data length (the `ds64` value for RF64).
    pub data_len: u64,
    /// Declared RIFF length (the `ds64` value for RF64).
    pub riff_len: u64,
    /// The file is RF64.
    pub rf64: bool,
}

impl WavInfo {
    /// Whole frames in the declared data.
    pub fn frames(&self) -> u64 {
        self.data_len / u64::from(self.block.max(1))
    }
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Parse the head of a RIFF or RF64 WAV (`bytes` = at least the header, `_file_len`
/// reserved for readers that validate against the real size).
pub fn parse(bytes: &[u8], _file_len: u64) -> Option<WavInfo> {
    let rf64 = match bytes.get(0..4)? {
        b"RIFF" => false,
        b"RF64" => true,
        _ => return None,
    };
    if bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut riff_len = u64::from(u32_at(bytes, 4)?);
    let mut ds64_data = None;
    let mut ds64_pos = None;
    let mut fmt = None;
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(bytes, pos + 4)?;
        let body = pos + 8;
        match id {
            b"ds64" | b"JUNK" if pos == JUNK_POS as usize && size as usize >= DS64_PAYLOAD => {
                ds64_pos = Some(pos as u64);
                if id == b"ds64" && rf64 {
                    riff_len = u64_at(bytes, body)?;
                    ds64_data = Some(u64_at(bytes, body + 8)?);
                }
            }
            b"fmt " => {
                let f = bytes.get(body..body + 16)?;
                fmt = Some((
                    u16::from_le_bytes([f[2], f[3]]),
                    u32::from_le_bytes(f[4..8].try_into().ok()?),
                    u16::from_le_bytes([f[12], f[13]]),
                ));
            }
            b"data" => {
                let (channels, rate, block) = fmt?;
                let data_len = match (size == u32::MAX, ds64_data) {
                    (true, Some(d)) if rf64 => d,
                    _ => u64::from(size),
                };
                return (rate > 0 && block > 0).then_some(WavInfo {
                    rate,
                    channels,
                    block,
                    data_pos: body as u64,
                    data_size_pos: pos as u64 + 4,
                    ds64_pos,
                    data_len,
                    riff_len,
                    rf64,
                });
            }
            _ => {}
        }
        pos = body + size as usize + (size as usize % 2);
    }
    None
}

/// In-place header edits `(offset, bytes)` that make a file of `file_len` bytes with
/// `data_len` sample bytes valid: RIFF while it fits 32 bits, RF64 beyond.
pub fn patch_ops(h: &Header, file_len: u64, data_len: u64, block: u16) -> Vec<(u64, Vec<u8>)> {
    patch_ops_at(h.data_size_pos, Some(JUNK_POS), file_len, data_len, block)
}

/// [`patch_ops`] for an existing file described by [`parse`]. Without a reserved
/// slot the 32-bit sizes are clamped (the best a plain RIFF can do).
pub fn patch_ops_for(
    info: &WavInfo,
    file_len: u64,
    data_len: u64,
    block: u16,
) -> Vec<(u64, Vec<u8>)> {
    patch_ops_at(info.data_size_pos, info.ds64_pos, file_len, data_len, block)
}

fn patch_ops_at(
    data_size_pos: u64,
    ds64_pos: Option<u64>,
    file_len: u64,
    data_len: u64,
    block: u16,
) -> Vec<(u64, Vec<u8>)> {
    let riff_len = file_len.saturating_sub(8);
    let fits = riff_len <= u64::from(u32::MAX - 1) && data_len <= u64::from(u32::MAX - 1);
    match ds64_pos {
        Some(p) if !fits => {
            let mut ds = Vec::with_capacity(DS64_PAYLOAD);
            ds.extend_from_slice(&riff_len.to_le_bytes());
            ds.extend_from_slice(&data_len.to_le_bytes());
            ds.extend_from_slice(&(data_len / u64::from(block.max(1))).to_le_bytes());
            ds.extend_from_slice(&0u32.to_le_bytes());
            vec![
                (0, b"RF64".to_vec()),
                (4, u32::MAX.to_le_bytes().to_vec()),
                (p, b"ds64".to_vec()),
                (p + 8, ds),
                (data_size_pos, u32::MAX.to_le_bytes().to_vec()),
            ]
        }
        _ => {
            let clamp = |v: u64| u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes().to_vec();
            vec![(4, clamp(riff_len)), (data_size_pos, clamp(data_len))]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt16(rate: u32, ch: u16, block: u16) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&3u16.to_le_bytes());
        f.extend_from_slice(&ch.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        f.extend_from_slice(&block.to_le_bytes());
        f.extend_from_slice(&32u16.to_le_bytes());
        f
    }

    #[test]
    fn header_has_junk_slot_and_parses() {
        let h = header(&fmt16(48_000, 2, 8));
        assert_eq!(&h.bytes[0..4], b"RIFF");
        assert_eq!(&h.bytes[12..16], b"JUNK");
        let info = parse(&h.bytes, 1 << 40).unwrap();
        assert_eq!((info.rate, info.channels, info.block), (48_000, 2, 8));
        assert_eq!(info.data_pos, h.bytes.len() as u64);
        assert_eq!(info.data_size_pos, h.data_size_pos);
        assert_eq!(info.ds64_pos, Some(12));
        assert!(!info.rf64);
    }

    #[test]
    fn small_files_stay_riff() {
        let h = header(&fmt16(48_000, 2, 8));
        let ops = patch_ops(&h, 1_000_000 + h.bytes.len() as u64, 1_000_000, 8);
        assert!(ops.iter().all(|(o, _)| *o != 0), "stays RIFF");
        let riff = ops.iter().find(|(o, _)| *o == 4).unwrap();
        assert_eq!(
            u32::from_le_bytes(riff.1[..4].try_into().unwrap()) as u64,
            1_000_000 + h.bytes.len() as u64 - 8
        );
        let data = ops.iter().find(|(o, _)| *o == h.data_size_pos).unwrap();
        assert_eq!(
            u32::from_le_bytes(data.1[..4].try_into().unwrap()),
            1_000_000
        );
    }

    #[test]
    fn big_files_become_rf64_with_ds64() {
        let h = header(&fmt16(48_000, 2, 8));
        let data = 5u64 << 30; // 5 GiB
        let len = data + h.bytes.len() as u64;
        let mut buf = h.bytes.clone();
        buf.resize(len.min(4096) as usize, 0);
        for (off, bytes) in patch_ops(&h, len, data, 8) {
            buf[off as usize..off as usize + bytes.len()].copy_from_slice(&bytes);
        }
        assert_eq!(&buf[0..4], b"RF64");
        assert_eq!(u32::from_le_bytes(buf[4..8].try_into().unwrap()), u32::MAX);
        assert_eq!(&buf[12..16], b"ds64");
        let info = parse(&buf, len).unwrap();
        assert!(info.rf64);
        assert_eq!(info.data_len, data);
        assert_eq!(info.frames(), data / 8);
        assert_eq!(info.riff_len, len - 8);
    }

    #[test]
    fn parser_accepts_plain_riff_without_junk_and_rejects_garbage() {
        let mut v = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&fmt16(8_000, 1, 4));
        v.extend_from_slice(b"data");
        v.extend_from_slice(&1234u32.to_le_bytes());
        let info = parse(&v, v.len() as u64 + 1234).unwrap();
        assert_eq!(
            (info.data_len, info.ds64_pos, info.rf64),
            (1234, None, false)
        );
        assert!(parse(b"RIFFxxxxAVI LIST", 100).is_none());
        assert!(parse(&[], 0).is_none());
    }

    #[test]
    fn without_a_junk_slot_sizes_clamp_instead_of_corrupting() {
        let mut v = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&fmt16(8_000, 1, 4));
        v.extend_from_slice(b"data\0\0\0\0");
        let info = parse(&v, 10).unwrap();
        let ops = patch_ops_for(&info, 6 << 30, 5 << 30, 4);
        assert!(ops.iter().all(|(o, _)| *o != 0 && *o != 12));
    }
}
