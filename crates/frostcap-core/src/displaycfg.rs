//! Display configuration queries (CCD API): the SDR white level of a monitor.

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::WIN32_ERROR;

/// `SDRWhiteLevel` is in 1/1000 of the 80-nit scRGB reference white.
pub fn nits_from_level(level: u32) -> f32 {
    (level as f32 / 1000.0 * crate::hdr::SCRGB_WHITE_NITS).max(crate::hdr::SCRGB_WHITE_NITS)
}

fn utf16_name(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// SDR white level (nits) of the monitor with GDI device name `gdi_name`
/// (e.g. `\\.\DISPLAY1`), if the display configuration can be read.
pub fn sdr_white_nits(gdi_name: &str) -> Option<f32> {
    let (mut np, mut nm) = (0u32, 0u32);
    // SAFETY: documented CCD call sequence with correctly sized buffers.
    unsafe {
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != WIN32_ERROR(0) {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            None,
        ) != WIN32_ERROR(0)
        {
            return None;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: p.sourceInfo.adapterId,
                    id: p.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0
                || !utf16_name(&src.viewGdiDeviceName).eq_ignore_ascii_case(gdi_name)
            {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size: size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut white.header) == 0 && white.SDRWhiteLevel > 0 {
                return Some(nits_from_level(white.SDRWhiteLevel));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_level_units_are_thousandths_of_80_nits() {
        assert_eq!(nits_from_level(1000), 80.0);
        assert_eq!(nits_from_level(3000), 240.0);
        assert_eq!(
            nits_from_level(500),
            80.0,
            "never below the scRGB reference"
        );
        assert_eq!(nits_from_level(0), 80.0);
    }

    #[test]
    fn gdi_names_are_cut_at_the_nul() {
        let mut b = [0u16; 32];
        for (i, c) in r"\\.\DISPLAY2".encode_utf16().enumerate() {
            b[i] = c;
        }
        assert_eq!(utf16_name(&b), r"\\.\DISPLAY2");
    }
}
