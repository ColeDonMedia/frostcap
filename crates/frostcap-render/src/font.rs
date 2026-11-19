//! Minimal TrueType (`glyf`) reader: cmap (formats 4/12), hmtx advances,
//! simple + composite glyph outlines. Enough for caption text with a system
//! font; no hinting, no shaping, no kerning.

/// One contour point in font units: (x, y, on_curve), y up.
pub type Pt = (f32, f32, bool);

/// A parsed TrueType font.
pub struct TtFont {
    data: Vec<u8>,
    upem: f32,
    ascent: f32,
    descent: f32,
    num_glyphs: u16,
    num_hmetrics: u16,
    loca_long: bool,
    hmtx: usize,
    loca: usize,
    glyf: usize,
    glyf_len: usize,
    cmap: (usize, u16),
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}
fn i16_at(d: &[u8], o: usize) -> Option<i16> {
    u16_at(d, o).map(|v| v as i16)
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(o)?,
        *d.get(o + 1)?,
        *d.get(o + 2)?,
        *d.get(o + 3)?,
    ]))
}

impl TtFont {
    /// Parse a TTF (or the first font of a TTC). `None` if unsupported.
    pub fn parse(data: Vec<u8>) -> Option<Self> {
        let d = &data;
        let mut base = 0usize;
        if d.get(0..4)? == b"ttcf" {
            base = u32_at(d, 12)? as usize;
        }
        let ver = u32_at(d, base)?;
        if ver != 0x0001_0000 && ver != u32::from_be_bytes(*b"true") {
            return None;
        }
        let n = u16_at(d, base + 4)? as usize;
        let mut tables = std::collections::HashMap::new();
        for i in 0..n {
            let r = base + 12 + i * 16;
            let tag = d.get(r..r + 4)?;
            let off = u32_at(d, r + 8)? as usize;
            let len = u32_at(d, r + 12)? as usize;
            if off.checked_add(len)? > d.len() {
                return None;
            }
            tables.insert(<[u8; 4]>::try_from(tag).ok()?, (off, len));
        }
        let t = |tag: &[u8; 4]| tables.get(tag).copied();
        let (head, _) = t(b"head")?;
        let (hhea, _) = t(b"hhea")?;
        let (maxp, _) = t(b"maxp")?;
        let (hmtx, _) = t(b"hmtx")?;
        let (loca, _) = t(b"loca")?;
        let (glyf, glyf_len) = t(b"glyf")?;
        let (cmap, _) = t(b"cmap")?;
        let upem = u16_at(d, head + 18)? as f32;
        if upem < 16.0 {
            return None;
        }
        // Best cmap subtable: (3,10)/(0,4+) format 12, else a format 4.
        let nsub = u16_at(d, cmap + 2)? as usize;
        let mut best: Option<(usize, u16, u8)> = None;
        for i in 0..nsub {
            let r = cmap + 4 + i * 8;
            let (pid, eid) = (u16_at(d, r)?, u16_at(d, r + 2)?);
            let off = cmap + u32_at(d, r + 4)? as usize;
            let fmt = u16_at(d, off)?;
            let rank = match (pid, eid, fmt) {
                (3, 10, 12) | (0, 4..=6, 12) => 3,
                (3, 1, 4) | (0, _, 4) => 2,
                _ => 0,
            };
            if rank > 0 && best.is_none_or(|b| rank > b.2) {
                best = Some((off, fmt, rank));
            }
        }
        let (cm_off, cm_fmt, _) = best?;
        Some(Self {
            upem,
            ascent: i16_at(d, hhea + 4)? as f32,
            descent: i16_at(d, hhea + 6)? as f32,
            num_hmetrics: u16_at(d, hhea + 34)?.max(1),
            num_glyphs: u16_at(d, maxp + 4)?,
            loca_long: i16_at(d, head + 50)? != 0,
            hmtx,
            loca,
            glyf,
            glyf_len,
            cmap: (cm_off, cm_fmt),
            data,
        })
    }

    /// Units per em.
    pub fn units_per_em(&self) -> f32 {
        self.upem
    }

    /// hhea ascender (font units, positive).
    pub fn ascent(&self) -> f32 {
        self.ascent
    }

    /// hhea descender (font units, negative).
    pub fn descent(&self) -> f32 {
        self.descent
    }

    /// Glyph id for `c` (0 = missing).
    pub fn glyph_index(&self, c: char) -> u16 {
        self.lookup(c as u32)
            .filter(|g| *g < self.num_glyphs)
            .unwrap_or(0)
    }

    fn lookup(&self, c: u32) -> Option<u16> {
        let d = &self.data;
        let (o, fmt) = self.cmap;
        if fmt == 12 {
            let n = u32_at(d, o + 12)? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let g = o + 16 + mid * 12;
                let (s, e) = (u32_at(d, g)?, u32_at(d, g + 4)?);
                if c < s {
                    hi = mid;
                } else if c > e {
                    lo = mid + 1;
                } else {
                    return u16::try_from(u32_at(d, g + 8)? + (c - s)).ok();
                }
            }
            return None;
        }
        if c > 0xFFFF {
            return None;
        }
        let seg = u16_at(d, o + 6)? as usize / 2;
        let ends = o + 14;
        let starts = ends + seg * 2 + 2;
        let deltas = starts + seg * 2;
        let ranges = deltas + seg * 2;
        for i in 0..seg {
            let end = u16_at(d, ends + i * 2)? as u32;
            if c > end {
                continue;
            }
            let start = u16_at(d, starts + i * 2)? as u32;
            if c < start {
                return None;
            }
            let delta = u16_at(d, deltas + i * 2)?;
            let ro = u16_at(d, ranges + i * 2)? as usize;
            if ro == 0 {
                return Some((c as u16).wrapping_add(delta));
            }
            let g = u16_at(d, ranges + i * 2 + ro + 2 * (c - start) as usize)?;
            return (g != 0).then(|| g.wrapping_add(delta));
        }
        None
    }

    /// Advance width (font units).
    pub fn advance(&self, gid: u16) -> f32 {
        let i = gid.min(self.num_hmetrics - 1) as usize;
        u16_at(&self.data, self.hmtx + i * 4).unwrap_or(0) as f32
    }

    fn glyph_range(&self, gid: u16) -> Option<(usize, usize)> {
        if gid >= self.num_glyphs {
            return None;
        }
        let d = &self.data;
        let i = gid as usize;
        let (a, b) = if self.loca_long {
            (
                u32_at(d, self.loca + i * 4)? as usize,
                u32_at(d, self.loca + i * 4 + 4)? as usize,
            )
        } else {
            (
                u16_at(d, self.loca + i * 2)? as usize * 2,
                u16_at(d, self.loca + i * 2 + 2)? as usize * 2,
            )
        };
        (b > a && b <= self.glyf_len).then_some((self.glyf + a, self.glyf + b))
    }

    /// Contours of a glyph (composites resolved).
    pub fn outline(&self, gid: u16) -> Vec<Vec<Pt>> {
        let mut out = Vec::new();
        self.outline_into(gid, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0, &mut out);
        out
    }

    fn outline_into(
        &self,
        gid: u16,
        m: [f32; 6],
        depth: u32,
        out: &mut Vec<Vec<Pt>>,
    ) -> Option<()> {
        if depth > 8 {
            return None;
        }
        let (start, end) = self.glyph_range(gid)?;
        let d = &self.data[..end];
        let nc = i16_at(d, start)?;
        let xf = |x: f32, y: f32| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        if nc >= 0 {
            let nc = nc as usize;
            let mut p = start + 10;
            let mut ends = Vec::with_capacity(nc);
            for _ in 0..nc {
                ends.push(u16_at(d, p)? as usize);
                p += 2;
            }
            let npts = ends.last().map_or(0, |e| e + 1);
            p += 2 + u16_at(d, p)? as usize;
            let mut flags = Vec::with_capacity(npts);
            while flags.len() < npts {
                let f = *d.get(p)?;
                p += 1;
                flags.push(f);
                if f & 8 != 0 {
                    let r = *d.get(p)?;
                    p += 1;
                    for _ in 0..r {
                        flags.push(f);
                    }
                }
            }
            flags.truncate(npts);
            let mut coords = |short: u8, same: u8| -> Option<Vec<f32>> {
                let mut v = 0i32;
                let mut out = Vec::with_capacity(npts);
                for &f in &flags {
                    if f & short != 0 {
                        let b = *d.get(p)? as i32;
                        p += 1;
                        v += if f & same != 0 { b } else { -b };
                    } else if f & same == 0 {
                        v += i16_at(d, p)? as i32;
                        p += 2;
                    }
                    out.push(v as f32);
                }
                Some(out)
            };
            let xs = coords(2, 16)?;
            let ys = coords(4, 32)?;
            let mut s = 0;
            for e in ends {
                if e < s || e >= npts {
                    return None;
                }
                out.push(
                    (s..=e)
                        .map(|i| {
                            let (x, y) = xf(xs[i], ys[i]);
                            (x, y, flags[i] & 1 != 0)
                        })
                        .collect(),
                );
                s = e + 1;
            }
            return Some(());
        }
        // Composite glyph.
        let mut p = start + 10;
        loop {
            let flags = u16_at(d, p)?;
            let child = u16_at(d, p + 2)?;
            p += 4;
            let (dx, dy) = if flags & 1 != 0 {
                let r = (i16_at(d, p)? as f32, i16_at(d, p + 2)? as f32);
                p += 4;
                r
            } else {
                let r = (*d.get(p)? as i8 as f32, *d.get(p + 1)? as i8 as f32);
                p += 2;
                r
            };
            let (dx, dy) = if flags & 2 != 0 { (dx, dy) } else { (0.0, 0.0) };
            let f2 = |o: usize| i16_at(d, o).map(|v| v as f32 / 16384.0);
            let (a, b, c, dd) = if flags & 8 != 0 {
                let s = f2(p)?;
                p += 2;
                (s, 0.0, 0.0, s)
            } else if flags & 0x40 != 0 {
                let r = (f2(p)?, 0.0, 0.0, f2(p + 2)?);
                p += 4;
                r
            } else if flags & 0x80 != 0 {
                let r = (f2(p)?, f2(p + 2)?, f2(p + 4)?, f2(p + 6)?);
                p += 8;
                r
            } else {
                (1.0, 0.0, 0.0, 1.0)
            };
            // Child transform, then the parent's.
            let cm = [
                m[0] * a + m[2] * b,
                m[1] * a + m[3] * b,
                m[0] * c + m[2] * dd,
                m[1] * c + m[3] * dd,
                m[0] * dx + m[2] * dy + m[4],
                m[1] * dx + m[3] * dy + m[5],
            ];
            self.outline_into(child, cm, depth + 1, out);
            if flags & 0x20 == 0 {
                break;
            }
        }
        Some(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn system_font() -> Option<TtFont> {
        let dir = std::path::PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
        ["seguisb.ttf", "segoeui.ttf", "arial.ttf"]
            .iter()
            .find_map(|f| std::fs::read(dir.join(f)).ok().and_then(TtFont::parse))
    }

    #[test]
    fn rejects_garbage() {
        assert!(TtFont::parse(vec![]).is_none());
        assert!(TtFont::parse(b"definitely not a font file at all".to_vec()).is_none());
    }

    #[test]
    fn reads_metrics_cmap_and_advances() {
        let f = system_font().expect("a system TrueType font (Segoe UI / Arial)");
        assert!(f.units_per_em() >= 16.0);
        assert!(f.ascent() > 0.0 && f.descent() < 0.0);
        let a = f.glyph_index('A');
        assert_ne!(a, 0);
        assert_eq!(f.glyph_index('\u{10FFFD}'), 0);
        let (i, w) = (f.glyph_index('i'), f.glyph_index('W'));
        assert!(f.advance(i) > 0.0 && f.advance(i) < f.advance(w));
    }

    #[test]
    fn outlines_simple_and_composite() {
        let f = system_font().expect("system font");
        let upem = f.units_per_em();
        let a = f.outline(f.glyph_index('A'));
        assert!(!a.is_empty() && a.iter().all(|c| c.len() >= 3));
        assert!(
            a.iter()
                .flatten()
                .all(|p| p.0 > -upem && p.0 < 2.0 * upem && p.1 > -upem && p.1 < 2.0 * upem)
        );
        assert!(f.outline(f.glyph_index(' ')).is_empty());
        // 'Ä' is a composite (A + dieresis) in common Windows fonts: more contours.
        let ad = f.outline(f.glyph_index('\u{00C4}'));
        assert!(ad.len() > a.len(), "{} vs {}", ad.len(), a.len());
    }
}
