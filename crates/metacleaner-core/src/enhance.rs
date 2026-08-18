//! Classical (non-AI) image quality enhancement: a per-channel auto-levels
//! contrast stretch followed by unsharp-mask sharpening.
//!
//! This is deliberately **not** AI super-resolution/upscaling. Real
//! super-resolution (Real-ESRGAN, waifu2x, and similar) runs a trained
//! neural network and needs an inference runtime (e.g. ONNX Runtime) plus
//! a pretrained model weighing anywhere from tens to hundreds of MB — a
//! very different kind of dependency than a small, fast, offline metadata
//! cleaner should carry, and it only really helps genuinely low-resolution
//! or heavily degraded input. What most "enhance" buttons in mainstream
//! photo tools actually do to a normal photo is exactly what this module
//! does: auto-levels (stretch the tonal range so the image isn't flat/dull)
//! and unsharp masking (locally boost contrast at edges so detail reads as
//! "sharper"). Both are deterministic, well-established techniques with no
//! model weights involved.

use image::RgbaImage;

/// Fraction of pixels clipped at each end of the histogram before finding
/// the stretch range — the standard "auto levels" clip percentage. Without
/// this, a single stray very-dark or very-bright pixel would peg the whole
/// stretch and do nothing.
const CONTRAST_CLIP_FRACTION: f32 = 0.005;

/// Gaussian sigma for the unsharp mask's blur pass.
const SHARPEN_SIGMA: f32 = 1.0;

/// Unsharp-mask threshold: per-channel differences from the blurred version
/// smaller than this are treated as noise and left alone, so flat areas
/// don't pick up sharpening artifacts.
const SHARPEN_THRESHOLD: i32 = 2;

/// Apply auto-contrast then unsharp-mask sharpening, in place. Alpha is
/// untouched.
pub fn enhance(img: &mut RgbaImage) {
    auto_contrast(img);
    let sharpened = image::imageops::unsharpen(img, SHARPEN_SIGMA, SHARPEN_THRESHOLD);
    *img = sharpened;
}

/// Per-channel histogram stretch: find the low/high values that clip
/// [`CONTRAST_CLIP_FRACTION`] of pixels at each end, then linearly remap
/// that range to the full 0-255 range. A channel that's already
/// (near-)full-range, or degenerate (a single flat color), is left alone.
fn auto_contrast(img: &mut RgbaImage) {
    let total_pixels = img.width() as usize * img.height() as usize;
    if total_pixels == 0 {
        return;
    }
    let clip = (total_pixels as f32 * CONTRAST_CLIP_FRACTION) as usize;

    let mut histograms = [[0u32; 256]; 3];
    for pixel in img.pixels() {
        for (c, hist) in histograms.iter_mut().enumerate() {
            hist[pixel[c] as usize] += 1;
        }
    }

    let mut low = [0u8; 3];
    let mut high = [255u8; 3];
    for c in 0..3 {
        let mut cum = 0usize;
        for (v, &count) in histograms[c].iter().enumerate() {
            cum += count as usize;
            if cum > clip {
                low[c] = v as u8;
                break;
            }
        }
        cum = 0;
        for (v, &count) in histograms[c].iter().enumerate().rev() {
            cum += count as usize;
            if cum > clip {
                high[c] = v as u8;
                break;
            }
        }
    }

    if (0..3).all(|c| high[c] <= low[c]) {
        return; // fully flat image; nothing to stretch
    }

    for pixel in img.pixels_mut() {
        for c in 0..3 {
            let (lo, hi) = (low[c] as f32, high[c] as f32);
            if hi <= lo {
                continue;
            }
            let stretched = (pixel[c] as f32 - lo) / (hi - lo) * 255.0;
            pixel[c] = stretched.clamp(0.0, 255.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn stretches_a_low_contrast_image_toward_full_range() {
        // A gradient confined to the narrow [100, 140] band.
        let mut img: RgbaImage = image::ImageBuffer::from_fn(64, 1, |x, _y| {
            let v = 100 + (x * 40 / 63) as u8;
            Rgba([v, v, v, 255])
        });
        enhance(&mut img);

        let min = img.pixels().map(|p| p[0]).min().unwrap();
        let max = img.pixels().map(|p| p[0]).max().unwrap();
        assert!(
            min < 30,
            "expected the dark end to stretch toward 0, got {min}"
        );
        assert!(
            max > 220,
            "expected the bright end to stretch toward 255, got {max}"
        );
    }

    #[test]
    fn leaves_a_flat_image_unpanicked_and_stable() {
        let mut img: RgbaImage = image::ImageBuffer::from_pixel(8, 8, Rgba([128, 128, 128, 255]));
        enhance(&mut img); // must not panic or divide by zero on a flat channel
        for p in img.pixels() {
            assert_eq!(p[0], 128);
        }
    }

    #[test]
    fn preserves_alpha() {
        let mut img: RgbaImage = image::ImageBuffer::from_fn(16, 16, |x, y| {
            Rgba([(x * 15) as u8, (y * 15) as u8, 100, 200])
        });
        enhance(&mut img);
        assert!(img.pixels().all(|p| p[3] == 200));
    }
}
