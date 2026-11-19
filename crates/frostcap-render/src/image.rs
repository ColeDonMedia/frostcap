//! Background image decoding through the Windows Imaging Component (no image
//! crate). The file is read with the project's size cap, sniffed (PNG/JPEG
//! magic, sane dimensions) and only then handed to WIC from memory.

use std::io::Read as _;
use std::path::Path;

use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};

use crate::background::RgbaImage;
use crate::error::{RenderError, Result};

fn wic_err(e: windows::core::Error) -> RenderError {
    RenderError::Media(format!("image decode: {e}"))
}

/// Decode a PNG/JPEG file to straight-alpha RGBA8. Files over
/// `limits::MAX_BACKGROUND_BYTES`, non-images and absurd dimensions are
/// rejected before WIC sees them.
pub fn decode_image(path: &Path) -> Result<RgbaImage> {
    use frostcap_project::limits::MAX_BACKGROUND_BYTES;
    let meta = std::fs::metadata(path).map_err(|e| RenderError::io(path, e))?;
    if !meta.is_file() {
        return Err(RenderError::Media(format!(
            "{} is not a file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BACKGROUND_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| RenderError::io(path, e))?;
    if bytes.len() as u64 > MAX_BACKGROUND_BYTES {
        return Err(RenderError::Media("background image is too large".into()));
    }
    let (kind, w, h) =
        frostcap_project::image_info(&bytes).map_err(|e| RenderError::Media(e.to_string()))?;
    // WIC decodes damaged files leniently; a PNG without its end chunk is cut off.
    if kind == frostcap_project::ImageKind::Png
        && !bytes[bytes.len().saturating_sub(16)..]
            .windows(4)
            .any(|w| w == b"IEND")
    {
        return Err(RenderError::Media("PNG is truncated".into()));
    }
    // SAFETY: plain WIC calls on a factory created here; `bytes` outlives the stream use.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(wic_err)?;
        let stream = wic.CreateStream().map_err(wic_err)?;
        stream.InitializeFromMemory(&bytes).map_err(wic_err)?;
        let decoder = wic
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .map_err(wic_err)?;
        let frame = decoder.GetFrame(0).map_err(wic_err)?;
        let conv = wic.CreateFormatConverter().map_err(wic_err)?;
        conv.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .map_err(wic_err)?;
        let (mut gw, mut gh) = (0u32, 0u32);
        conv.GetSize(&mut gw, &mut gh).map_err(wic_err)?;
        if (gw, gh) != (w, h) {
            return Err(RenderError::Media("image header and data disagree".into()));
        }
        let mut data = vec![0u8; w as usize * h as usize * 4];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut data)
            .map_err(wic_err)?;
        Ok(RgbaImage { w, h, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0xEDB8_8320
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    /// Minimal RGBA PNG writer (stored deflate blocks) for the tests.
    fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        for y in 0..h as usize {
            raw.push(0);
            raw.extend(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
        }
        let mut z = vec![0x78, 0x01];
        let mut chunks = raw.chunks(65_535).peekable();
        while let Some(c) = chunks.next() {
            z.push(u8::from(chunks.peek().is_none()));
            z.extend((c.len() as u16).to_le_bytes());
            z.extend((!(c.len() as u16)).to_le_bytes());
            z.extend(c);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + u32::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        z.extend(((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], body: &[u8]| {
            out.extend((body.len() as u32).to_be_bytes());
            let mut c = kind.to_vec();
            c.extend(body);
            out.extend(&c);
            out.extend(crc32(&c).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend(w.to_be_bytes());
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        out
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
            })
            .join("test-tmp")
            .join(format!("render-image-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn decodes_a_png_to_straight_rgba() {
        let src: Vec<u8> = vec![
            255, 0, 0, 255, /**/ 0, 255, 0, 255, /**/ 0, 0, 255, 128, //
            10, 20, 30, 0, /**/ 255, 255, 255, 255, /**/ 1, 2, 3, 4,
        ];
        let d = scratch("png");
        let f = d.join("t.png");
        std::fs::write(&f, png(3, 2, &src)).unwrap();
        let img = decode_image(&f).unwrap();
        assert_eq!((img.w, img.h), (3, 2));
        assert_eq!(img.data, src);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_non_images_oversized_dimensions_and_truncated_data() {
        let d = scratch("bad");
        let put = |n: &str, b: &[u8]| {
            let f = d.join(n);
            std::fs::write(&f, b).unwrap();
            f
        };
        assert!(decode_image(&put("a.png", b"not an image at all")).is_err());
        assert!(decode_image(&d.join("missing.png")).is_err());
        assert!(decode_image(&d).is_err());
        // Header claims a huge picture: refused before WIC allocates anything.
        let mut huge = png(2, 2, &[0; 16]);
        huge[16..20].copy_from_slice(&40_000u32.to_be_bytes());
        assert!(decode_image(&put("huge.png", &huge)).is_err());
        // Valid header, damaged pixel data.
        let good = png(8, 8, &[200; 8 * 8 * 4]);
        assert!(decode_image(&put("cut.png", &good[..good.len() / 2])).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
