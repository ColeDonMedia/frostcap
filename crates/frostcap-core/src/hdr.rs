//! HDR -> SDR conversion for captures of HDR displays.
//!
//! On a display with Advanced Color / HDR on, WGC delivers `B8G8R8A8` frames that the
//! compositor has already clipped and mis-scaled (washed-out or blown-out video).
//! Frostcap therefore asks WGC for `R16G16B16A16Float` frames (scRGB: linear, BT.709
//! primaries, 1.0 = 80 nits) on such monitors and converts them itself:
//!
//! 1. scale so the user's *SDR white level* (the "SDR content brightness" slider,
//!    80 nits and up) becomes 1.0, which keeps ordinary desktop content unchanged;
//! 2. negative values (wide-gamut colours outside BT.709) are clamped to 0;
//! 3. anything brighter than SDR white is brought back into range by dividing by
//!    the largest channel (hue-preserving, so highlights do not shift colour);
//! 4. the sRGB transfer function (what SDR displays and BT.709 video players use
//!    for desktop content) encodes the result to 8 bit.
//!
//! The same maths runs on the GPU ([`HLSL`], pixel shader) and on the CPU
//! ([`tonemap`], used for tests and as the reference).

/// scRGB reference white: 1.0 = 80 nits.
pub const SCRGB_WHITE_NITS: f32 = 80.0;

/// sRGB opto-electronic transfer function (linear -> encoded), input clamped to 0..=1.
pub fn srgb_oetf(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert one scRGB pixel to gamma-encoded SDR RGB in 0..=1.
/// `sdr_white_nits` is the display's SDR content white level (>= 80).
pub fn tonemap(rgb: [f32; 3], sdr_white_nits: f32) -> [f32; 3] {
    let scale = SCRGB_WHITE_NITS / sdr_white_nits.max(SCRGB_WHITE_NITS);
    let fix = |v: f32| {
        if v.is_finite() {
            v.max(0.0) * scale
        } else {
            0.0
        }
    };
    let c = [fix(rgb[0]), fix(rgb[1]), fix(rgb[2])];
    let m = c[0].max(c[1]).max(c[2]).max(1.0);
    [
        srgb_oetf(c[0] / m),
        srgb_oetf(c[1] / m),
        srgb_oetf(c[2] / m),
    ]
}

/// 8-bit value of a 0..=1 channel.
pub fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// IEEE half-float bits of `f` (round to nearest even, saturating to +-65504).
pub fn f32_to_f16_bits(f: f32) -> u16 {
    let b = f.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    if f.is_nan() {
        return sign | 0x7E00;
    }
    let f = f.abs().min(65_504.0);
    let b = f.to_bits();
    let exp = ((b >> 23) & 0xFF) as i32 - 127;
    let man = b & 0x7F_FFFF;
    if exp < -24 {
        return sign; // underflow to zero
    }
    if exp < -14 {
        // subnormal half
        let m = (man | 0x80_0000) >> (-14 - exp + 13);
        let round = ((man | 0x80_0000) >> (-14 - exp + 12)) & 1;
        return sign | (m + round) as u16;
    }
    let h = (((exp + 15) as u32) << 10) | (man >> 13);
    let round = (man >> 12) & 1;
    let sticky = man & 0xFFF != 0;
    let h = if round == 1 && (sticky || h & 1 == 1) {
        h + 1
    } else {
        h
    };
    sign | h as u16
}

/// Value of half-float `h`.
pub fn f16_bits_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1F);
    let man = f32::from(h & 0x3FF);
    sign * match exp {
        0 => man * 2f32.powi(-24),
        31 => {
            if man == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        e => (1.0 + man / 1024.0) * 2f32.powi(e - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(rgb: [f32; 3], white: f32) -> [u8; 3] {
        let t = tonemap(rgb, white);
        [to_u8(t[0]), to_u8(t[1]), to_u8(t[2])]
    }

    #[test]
    fn sdr_white_stays_white_at_any_brightness_slider() {
        assert_eq!(px([1.0; 3], 80.0), [255; 3]);
        // SDR content brightness 240 nits: that white arrives as scRGB 3.0.
        assert_eq!(px([3.0; 3], 240.0), [255; 3]);
        assert_eq!(px([0.0; 3], 240.0), [0; 3]);
    }

    #[test]
    fn mid_grey_round_trips_through_the_srgb_curve() {
        // sRGB 128/255 decodes to linear 0.2158; at 80 nits white it must come back as 128.
        let lin = 0.215_86;
        assert_eq!(px([lin; 3], 80.0), [128; 3]);
        // With the slider at 200 nits the same on-screen grey is scRGB lin * 2.5.
        assert_eq!(px([lin * 2.5; 3], 200.0), [128; 3]);
    }

    #[test]
    fn highlights_keep_their_hue_instead_of_clipping_per_channel() {
        let t = tonemap([4.0, 2.0, 1.0], 80.0);
        assert!((t[0] - 1.0).abs() < 1e-6, "brightest channel reaches white");
        // Ratios are kept in linear light: 4:2:1 -> 1:0.5:0.25.
        let lin = |e: f32| ((e + 0.055) / 1.055).powf(2.4);
        assert!((lin(t[1]) - 0.5).abs() < 1e-3 && (lin(t[2]) - 0.25).abs() < 1e-3);
        // A naive per-channel clip would give (1,1,1): white.
        assert!(t[2] < 0.6);
    }

    #[test]
    fn garbage_in_never_produces_garbage_out() {
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 1e30] {
            for c in tonemap([v, 0.5, v], 80.0) {
                assert!((0.0..=1.0).contains(&c), "{v} -> {c}");
            }
        }
        // sdr white below the scRGB reference is treated as 80 nits.
        assert_eq!(tonemap([1.0; 3], 10.0), tonemap([1.0; 3], 80.0));
    }

    #[test]
    fn tonemap_is_monotonic_in_brightness() {
        let mut last = 0.0;
        for i in 0..400 {
            let v = tonemap([i as f32 / 100.0; 3], 100.0)[1];
            assert!(v >= last - 1e-6);
            last = v;
        }
    }

    #[test]
    fn half_float_conversion_is_exact_for_representable_values() {
        for v in [
            0.0f32,
            1.0,
            -1.0,
            0.5,
            3.0,
            0.25,
            1024.0,
            65_504.0,
            2f32.powi(-14),
            2f32.powi(-24),
        ] {
            assert_eq!(f16_bits_to_f32(f32_to_f16_bits(v)), v, "{v}");
        }
        assert_eq!(f32_to_f16_bits(1.0), 0x3C00);
        assert_eq!(f32_to_f16_bits(-2.0), 0xC000);
        assert_eq!(f32_to_f16_bits(1e9), 0x7BFF, "saturates");
        let near = f16_bits_to_f32(f32_to_f16_bits(0.2158));
        assert!((near - 0.2158).abs() < 2e-4);
    }
}
