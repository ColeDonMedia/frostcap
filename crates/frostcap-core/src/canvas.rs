//! Canvas layout for recording several monitors as one picture ("All displays").
//!
//! The canvas is the bounding box of the monitors on the virtual desktop. Every
//! monitor is copied 1:1 (never rescaled) into its place; uncovered areas stay
//! black. Unless scaled (see below) nothing is resized, so cursor coordinates stay exact: they are
//! relative to the canvas origin ([`Layout::bounds`] `x`/`y`, possibly negative). A
//! union too big for the encoder is scaled down uniformly ([`Layout::scale`]).

use crate::error::{Error, Result};
use crate::project::Rect;

/// H.264 level 5.2 limit (4096x2304): macroblocks (16x16) per frame the OS encoders
/// accept. Larger unions (e.g. two 4K monitors side by side) cannot be encoded.
pub const MAX_MACROBLOCKS: u64 = 36_864;
/// Largest canvas side accepted by the project format.
pub const MAX_SIDE: i32 = 16_384;

/// Where one monitor lands on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    /// Index into the monitor list given to [`layout`].
    pub index: usize,
    /// Top-left on the canvas.
    pub dst: (u32, u32),
    /// Size (the monitor's size).
    pub size: (u32, u32),
}

/// Result of [`layout`].
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Union of the monitors on the virtual desktop (origin may be negative).
    pub bounds: Rect,
    /// Encoder width (union width rounded up to even).
    pub width: u32,
    /// Encoder height (union height rounded up to even).
    pub height: u32,
    /// Canvas pixels per desktop pixel (1.0 unless the union is too big to encode and
    /// was scaled down; cursor coordinates must be multiplied by it).
    pub scale: f64,
    /// One tile per monitor, in input order.
    pub tiles: Vec<Tile>,
}

/// Number of 16x16 macroblocks needed for a `w` x `h` frame.
pub fn macroblocks(w: u32, h: u32) -> u64 {
    u64::from(w.div_ceil(16)) * u64::from(h.div_ceil(16))
}

/// Lay out `monitors` (virtual-desktop rectangles, physical pixels) on one canvas.
pub fn layout(monitors: &[Rect]) -> Result<Layout> {
    if monitors.is_empty() {
        return Err(Error::SourceNotFound("no monitors to record".into()));
    }
    if monitors.iter().any(|m| m.width < 2 || m.height < 2) {
        return Err(Error::Invalid("a monitor has an empty size".into()));
    }
    let left = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let top = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let right = monitors.iter().map(|m| m.x + m.width).max().unwrap_or(0);
    let bottom = monitors.iter().map(|m| m.y + m.height).max().unwrap_or(0);
    let (uw, uh) = (right - left, bottom - top);
    if uw > MAX_SIDE || uh > MAX_SIDE {
        return Err(Error::Invalid(format!(
            "all displays together are {uw}x{uh} px, more than the {MAX_SIDE} px limit"
        )));
    }
    let (full_w, full_h) = ((uw as u32 + 1) & !1, (uh as u32 + 1) & !1);
    let mut scale = fit_scale(full_w, full_h);
    let dims = |s: f64| {
        if s < 1.0 {
            // Even sizes that never round up past the limits.
            (
                ((f64::from(full_w) * s) as u32) & !1,
                ((f64::from(full_h) * s) as u32) & !1,
            )
        } else {
            (full_w, full_h)
        }
    };
    // Macroblocks round up, so nudge the scale down until the area really fits.
    while macroblocks(dims(scale).0, dims(scale).1) > MAX_MACROBLOCKS {
        scale *= 0.995;
    }
    let (width, height) = dims(scale);
    // Tile edges are rounded, not sizes, so neighbours keep sharing an edge.
    let edge = |v: i32| ((f64::from(v) * scale).round() as u32).min(width.max(height));
    Ok(Layout {
        bounds: Rect {
            x: left,
            y: top,
            width: uw,
            height: uh,
        },
        width,
        height,
        scale,
        tiles: monitors
            .iter()
            .enumerate()
            .map(|(index, m)| {
                let (x0, x1) = (edge(m.x - left), edge(m.x - left + m.width));
                let (y0, y1) = (edge(m.y - top), edge(m.y - top + m.height));
                let (x1, y1) = (x1.min(width), y1.min(height));
                Tile {
                    index,
                    dst: (x0.min(x1), y0.min(y1)),
                    size: (x1.saturating_sub(x0), y1.saturating_sub(y0)),
                }
            })
            .collect(),
    })
}

/// Largest side the hardware H.264 encoders accept.
pub const MAX_ENCODE_SIDE: u32 = 4096;

/// Scale (<= 1) that makes a `w` x `h` canvas encodable: at most
/// [`MAX_ENCODE_SIDE`] per side and [`MAX_MACROBLOCKS`] in area.
pub fn fit_scale(w: u32, h: u32) -> f64 {
    let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let area = (MAX_MACROBLOCKS * 256) as f64 / (w * h);
    let side = f64::from(MAX_ENCODE_SIDE) / w.max(h);
    area.sqrt().min(side).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn two_monitors_side_by_side() {
        let l = layout(&[r(0, 0, 1920, 1080), r(1920, 0, 1920, 1080)]).unwrap();
        assert_eq!((l.width, l.height), (3840, 1080));
        assert_eq!(l.tiles[1].dst, (1920, 0));
        assert_eq!(l.bounds, r(0, 0, 3840, 1080));
    }

    #[test]
    fn negative_origin_keeps_input_coordinates_relative_to_the_canvas() {
        // Secondary monitor to the left of and above the primary.
        let l = layout(&[r(0, 0, 2560, 1440), r(-1280, -200, 1280, 720)]).unwrap();
        assert_eq!(l.bounds, r(-1280, -200, 3840, 1640));
        assert_eq!(l.tiles[0].dst, (1280, 200));
        assert_eq!(l.tiles[1].dst, (0, 0));
        // A cursor at virtual (100, 50) is canvas (100 - bounds.x, 50 - bounds.y).
        assert_eq!((100 - l.bounds.x, 50 - l.bounds.y), (1380, 250));
    }

    #[test]
    fn odd_unions_are_padded_to_even_encoder_sizes() {
        let l = layout(&[r(0, 0, 1921, 1081)]).unwrap();
        assert_eq!((l.width, l.height), (1922, 1082));
        assert_eq!(l.bounds.width, 1921, "bounds stay exact for input mapping");
    }

    #[test]
    fn gaps_are_allowed() {
        let l = layout(&[r(0, 0, 1000, 1000), r(2000, 500, 1000, 1000)]).unwrap();
        assert_eq!((l.width, l.height), (3000, 1500));
    }

    #[test]
    fn empty_or_absurd_sets_are_refused_with_a_clear_message() {
        assert!(layout(&[]).is_err());
        assert!(layout(&[r(0, 0, 0, 100)]).is_err());
        let e = layout(&[r(0, 0, 100, 100), r(20_000, 0, 100, 100)]).unwrap_err();
        assert!(e.to_string().contains("limit"));
    }

    #[test]
    fn fitting_sets_are_not_scaled() {
        for set in [
            vec![r(0, 0, 1920, 1080), r(1920, 0, 1920, 1080)],
            vec![r(0, 0, 1920, 1080), r(0, 1080, 1920, 1080)],
            vec![r(0, 0, 3840, 2160)],
        ] {
            let l = layout(&set).unwrap();
            assert_eq!(l.scale, 1.0, "{set:?}");
            assert_eq!(
                (l.width, l.height),
                (l.bounds.width as u32, l.bounds.height as u32)
            );
        }
    }

    #[test]
    fn oversized_sets_are_scaled_down_to_what_the_encoder_accepts() {
        // 2 x 1440p: 5120 px wide exceeds the 4096 px encoder width.
        let l = layout(&[r(0, 0, 2560, 1440), r(2560, 0, 2560, 1440)]).unwrap();
        assert_eq!((l.width, l.height), (4096, 1152));
        assert!((l.scale - 0.8).abs() < 1e-9);
        assert_eq!(l.tiles[0].dst, (0, 0));
        assert_eq!(l.tiles[1].dst, (2048, 0));
        assert_eq!(l.tiles[0].size, (2048, 1152));
        assert_eq!(l.bounds.width, 5120, "bounds stay in desktop pixels");
        // 2 x 4K and the 3 x 2560 setup.
        let l = layout(&[r(0, 0, 3840, 2160), r(3840, 0, 3840, 2160)]).unwrap();
        assert_eq!((l.width, l.height), (4096, 1152));
        let l = layout(&[
            r(0, 0, 2560, 1440),
            r(2560, 0, 2560, 1440),
            r(5120, 0, 2560, 1440),
        ])
        .unwrap();
        assert_eq!((l.width, l.height), (4096, 768));
        assert!(macroblocks(l.width, l.height) <= MAX_MACROBLOCKS);
        // Large and tall: area limit applies too.
        let l = layout(&[r(0, 0, 3840, 2160), r(0, 2160, 3840, 2160)]).unwrap();
        assert!(
            l.width <= 4096
                && l.height <= 4096
                && l.width.is_multiple_of(2)
                && l.height.is_multiple_of(2)
        );
        assert!(macroblocks(l.width, l.height) <= MAX_MACROBLOCKS);
    }

    #[test]
    fn scaled_tiles_share_edges_and_stay_inside_the_canvas() {
        let l = layout(&[
            r(0, 0, 2560, 1440),
            r(2560, 0, 2560, 1440),
            r(5120, 0, 1921, 1080),
        ])
        .unwrap();
        for w in l.tiles.windows(2) {
            assert_eq!(w[0].dst.0 + w[0].size.0, w[1].dst.0, "no gap or overlap");
        }
        for t in &l.tiles {
            assert!(t.dst.0 + t.size.0 <= l.width && t.dst.1 + t.size.1 <= l.height);
        }
    }
}
