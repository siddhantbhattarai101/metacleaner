//! metacleaner-core
//!
//! Strips EXIF, GPS, XMP, IPTC, C2PA content credentials, and AI-generator
//! signatures (Stable Diffusion `tEXt`/`iTXt`/`zTXt` chunks, DALL-E/Midjourney/
//! Firefly fingerprints, etc.) from raster images, and optionally resets the
//! image's pixel-level fingerprint.
//!
//! Approach: rather than parsing every known metadata segment/chunk format
//! (a losing game against new AI tools that invent new tag schemes), the
//! image is fully decoded to raw pixels and re-encoded from scratch. Every
//! container-level segment (JPEG APPn markers, PNG ancillary chunks, WebP
//! RIFF metadata chunks) that isn't part of pixel data is dropped by
//! construction, because the encoder writes only what image data + geometry
//! demands.
//!
//! This crate does no file or network I/O — it operates purely on in-memory
//! byte slices, so it can be reused unchanged from a native CLI or compiled
//! to `wasm32-unknown-unknown` for a browser build later.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageEncoder};
use rand::Rng;

mod inspect;
pub use inspect::{inspect, Finding, FindingCategory, InspectOptions, InspectReport};

/// Default cap on raw input size, in bytes, before any parsing is attempted.
/// 256 MiB is far beyond any legitimate photo; rejecting oversized input
/// outright avoids doing any decode work on it at all.
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Default cap on decoded pixel dimensions. 12000x12000 comfortably covers
/// any real camera or generator output while still bounding a maliciously
/// crafted file (e.g. a few-KB PNG whose IHDR claims an enormous canvas)
/// from forcing a huge allocation.
pub const DEFAULT_MAX_IMAGE_DIMENSION: u32 = 12_000;

/// Default cap on the memory a decoder is allowed to allocate while reading
/// pixel data — the actual decompression-bomb guard. Matches the `image`
/// crate's own built-in default.
pub const DEFAULT_MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

/// Supported image container formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    WebP,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Png => "png",
            ImageFormat::WebP => "webp",
        }
    }

    fn from_image_crate(fmt: image::ImageFormat) -> Option<Self> {
        match fmt {
            image::ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
            image::ImageFormat::Png => Some(ImageFormat::Png),
            image::ImageFormat::WebP => Some(ImageFormat::WebP),
            _ => None,
        }
    }
}

/// Detect the container format of an in-memory image, if supported.
pub fn detect_format(bytes: &[u8]) -> Option<ImageFormat> {
    image::guess_format(bytes)
        .ok()
        .and_then(ImageFormat::from_image_crate)
}

#[derive(Debug, thiserror::Error)]
pub enum CleanError {
    #[error(
        "could not determine image format, or format is unsupported (supported: JPEG, PNG, WebP)"
    )]
    UnsupportedFormat,
    #[error("input is {size} bytes, which exceeds the {max}-byte limit")]
    InputTooLarge { size: usize, max: u64 },
    #[error("failed to decode image (this includes exceeding configured size/memory limits): {0}")]
    Decode(#[from] image::ImageError),
    #[error("failed to encode image: {0}")]
    Encode(String),
}

/// Options controlling how an image is cleaned.
#[derive(Debug, Clone)]
pub struct CleanOptions {
    /// Apply a microscopic, visually-invisible perturbation (±1-2 RGB
    /// levels) to a subset of pixels so the output's cryptographic/
    /// perceptual hash no longer matches the source file. Does not touch
    /// alpha.
    pub reset_fingerprint: bool,
    /// Max per-channel delta applied when `reset_fingerprint` is set.
    /// 1-2 is invisible to the eye; kept configurable for testing.
    pub fingerprint_strength: u8,
    /// Fraction (0.0-1.0) of pixels perturbed when `reset_fingerprint` is set.
    pub fingerprint_fraction: f32,
    /// JPEG re-encode quality, 1-100. Ignored for PNG/WebP (lossless).
    pub jpeg_quality: u8,
    /// Force a specific output container instead of round-tripping the
    /// input's own format (e.g. force PNG -> JPEG). `None` keeps the
    /// input format.
    pub output_format: Option<ImageFormat>,
    /// Reject input larger than this many bytes before any parsing is
    /// attempted. `None` disables the check. Defaults to
    /// [`DEFAULT_MAX_INPUT_BYTES`].
    pub max_input_bytes: Option<u64>,
    /// Reject input whose decoded width or height exceeds this many pixels.
    /// `None` disables the check. Defaults to
    /// [`DEFAULT_MAX_IMAGE_DIMENSION`]. This is the primary
    /// decompression-bomb guard: a tiny file can declare an enormous
    /// canvas in its header, and without this check decoding it would
    /// force a huge allocation before we ever see the real pixel count.
    pub max_image_dimension: Option<u32>,
    /// Reject input that would require the decoder to allocate more than
    /// this many bytes while reading pixel data. `None` disables the
    /// check. Defaults to [`DEFAULT_MAX_DECODED_BYTES`].
    pub max_decoded_bytes: Option<u64>,
}

impl Default for CleanOptions {
    fn default() -> Self {
        Self {
            reset_fingerprint: true,
            fingerprint_strength: 2,
            fingerprint_fraction: 0.25,
            jpeg_quality: 92,
            output_format: None,
            max_input_bytes: Some(DEFAULT_MAX_INPUT_BYTES),
            max_image_dimension: Some(DEFAULT_MAX_IMAGE_DIMENSION),
            max_decoded_bytes: Some(DEFAULT_MAX_DECODED_BYTES),
        }
    }
}

/// Summary of what happened during a clean, useful for CLI reporting and UI.
#[derive(Debug, Clone)]
pub struct CleanReport {
    pub input_format: ImageFormat,
    pub output_format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub bytes_in: usize,
    pub bytes_out: usize,
    pub fingerprint_reset: bool,
}

#[derive(Debug)]
pub struct CleanedImage {
    pub bytes: Vec<u8>,
    pub report: CleanReport,
}

/// Strip all metadata from `input` and return the cleaned image bytes.
pub fn clean(input: &[u8], opts: &CleanOptions) -> Result<CleanedImage, CleanError> {
    if let Some(max) = opts.max_input_bytes {
        if input.len() as u64 > max {
            return Err(CleanError::InputTooLarge {
                size: input.len(),
                max,
            });
        }
    }

    let input_format = detect_format(input).ok_or(CleanError::UnsupportedFormat)?;
    let output_format = opts.output_format.unwrap_or(input_format);

    let mut limits = image::Limits::no_limits();
    limits.max_image_width = opts.max_image_dimension;
    limits.max_image_height = opts.max_image_dimension;
    limits.max_alloc = opts.max_decoded_bytes;

    let decoded = decode_with_limits(input, input_format, limits)?;
    let width = decoded.width();
    let height = decoded.height();

    let mut rgba = decoded.into_rgba8();
    if opts.reset_fingerprint {
        reset_fingerprint(
            &mut rgba,
            opts.fingerprint_strength,
            opts.fingerprint_fraction,
        );
    }

    let mut out = Vec::new();
    {
        let mut cursor = Cursor::new(&mut out);
        match output_format {
            ImageFormat::Jpeg => {
                // JPEG has no alpha channel.
                let rgb = DynamicImage::ImageRgba8(rgba).into_rgb8();
                let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                    &mut cursor,
                    opts.jpeg_quality.clamp(1, 100),
                );
                encoder
                    .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
                    .map_err(|e| CleanError::Encode(e.to_string()))?;
            }
            ImageFormat::Png => {
                let encoder = image::codecs::png::PngEncoder::new_with_quality(
                    &mut cursor,
                    image::codecs::png::CompressionType::Best,
                    image::codecs::png::FilterType::Adaptive,
                );
                encoder
                    .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
                    .map_err(|e| CleanError::Encode(e.to_string()))?;
            }
            ImageFormat::WebP => {
                let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut cursor);
                encoder
                    .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
                    .map_err(|e| CleanError::Encode(e.to_string()))?;
            }
        }
    }

    Ok(CleanedImage {
        report: CleanReport {
            input_format,
            output_format,
            width,
            height,
            bytes_in: input.len(),
            bytes_out: out.len(),
            fingerprint_reset: opts.reset_fingerprint,
        },
        bytes: out,
    })
}

/// Decode `input` with the given format, enforcing `limits` on decoded
/// dimensions and allocation size before any large buffer is allocated.
fn decode_with_limits(
    input: &[u8],
    format: ImageFormat,
    limits: image::Limits,
) -> Result<DynamicImage, CleanError> {
    let cursor = Cursor::new(input);
    match format {
        ImageFormat::Jpeg => {
            let mut decoder = image::codecs::jpeg::JpegDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            Ok(DynamicImage::from_decoder(decoder)?)
        }
        ImageFormat::Png => {
            let mut decoder = image::codecs::png::PngDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            Ok(DynamicImage::from_decoder(decoder)?)
        }
        ImageFormat::WebP => {
            let mut decoder = image::codecs::webp::WebPDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            Ok(DynamicImage::from_decoder(decoder)?)
        }
    }
}

/// Apply an invisible, randomized per-pixel perturbation to defeat
/// hash/fingerprint matching against the original file. RGB only —
/// alpha is left untouched so transparency is preserved exactly.
fn reset_fingerprint(img: &mut image::RgbaImage, strength: u8, fraction: f32) {
    if strength == 0 || fraction <= 0.0 {
        return;
    }
    let strength = strength as i16;
    let fraction = fraction.clamp(0.0, 1.0);
    let mut rng = rand::thread_rng();

    for pixel in img.pixels_mut() {
        if !rng.gen_bool(fraction as f64) {
            continue;
        }
        for channel in 0..3 {
            let delta: i16 = rng.gen_range(-strength..=strength);
            let v = pixel[channel] as i16 + delta;
            pixel[channel] = v.clamp(0, 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn make_png_with_text_chunk() -> Vec<u8> {
        // Build a tiny RGBA image, encode as PNG, then splice in a fake
        // Stable Diffusion tEXt chunk ("parameters") after IHDR, exactly
        // like Automatic1111/ComfyUI do.
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(4, 4, |x, y| {
            Rgba([(x * 40) as u8, (y * 40) as u8, 128, 255])
        });
        let mut base = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut base), image::ImageFormat::Png)
            .unwrap();

        let keyword = b"parameters\0";
        let text =
            b"a photo of a cat, seed: 12345, sampler: Euler a, cfg scale: 7, model hash: abcd1234";
        let mut chunk_data = Vec::new();
        chunk_data.extend_from_slice(keyword);
        chunk_data.extend_from_slice(text);

        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(chunk_data.len() as u32).to_be_bytes());
        chunk.extend_from_slice(b"tEXt");
        chunk.extend_from_slice(&chunk_data);
        let crc = crc32(&chunk[4..]);
        chunk.extend_from_slice(&crc.to_be_bytes());

        // Insert right after the 8-byte signature + IHDR chunk (25 bytes: 4 len + 4 type + 13 data + 4 crc).
        let insert_at = 8 + 25;
        let mut out = base[..insert_at].to_vec();
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&base[insert_at..]);
        out
    }

    fn crc32(bytes: &[u8]) -> u32 {
        // Minimal CRC-32 (IEEE) for test fixture generation only.
        let mut table = [0u32; 256];
        for (i, entry) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB88320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *entry = c;
        }
        let mut crc = 0xFFFFFFFFu32;
        for &b in bytes {
            crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
        crc ^ 0xFFFFFFFF
    }

    #[test]
    fn strips_stable_diffusion_text_chunk() {
        let input = make_png_with_text_chunk();
        assert!(
            input.windows(10).any(|w| w == b"parameters"),
            "fixture sanity check: input should contain the SD tEXt chunk"
        );

        let opts = CleanOptions {
            reset_fingerprint: false,
            ..Default::default()
        };
        let cleaned = clean(&input, &opts).expect("clean should succeed");

        assert!(
            !cleaned.bytes.windows(10).any(|w| w == b"parameters"),
            "output must not contain the Stable Diffusion parameters chunk"
        );
        assert_eq!(cleaned.report.input_format, ImageFormat::Png);
        assert_eq!(cleaned.report.width, 4);
        assert_eq!(cleaned.report.height, 4);
    }

    #[test]
    fn fingerprint_reset_changes_bytes_but_not_dimensions() {
        let input = make_png_with_text_chunk();
        let opts = CleanOptions {
            reset_fingerprint: true,
            fingerprint_strength: 2,
            fingerprint_fraction: 1.0,
            ..Default::default()
        };
        let cleaned = clean(&input, &opts).expect("clean should succeed");
        let baseline = clean(
            &input,
            &CleanOptions {
                reset_fingerprint: false,
                ..Default::default()
            },
        )
        .expect("baseline clean should succeed");

        assert_ne!(cleaned.bytes, baseline.bytes);
        assert_eq!(cleaned.report.width, baseline.report.width);
        assert_eq!(cleaned.report.height, baseline.report.height);
    }

    #[test]
    fn rejects_unsupported_format() {
        let err = clean(b"not an image", &CleanOptions::default()).unwrap_err();
        assert!(matches!(err, CleanError::UnsupportedFormat));
    }

    #[test]
    fn can_convert_png_to_jpeg() {
        let input = make_png_with_text_chunk();
        let opts = CleanOptions {
            output_format: Some(ImageFormat::Jpeg),
            reset_fingerprint: false,
            ..Default::default()
        };
        let cleaned = clean(&input, &opts).expect("clean should succeed");
        assert_eq!(cleaned.report.output_format, ImageFormat::Jpeg);
        assert_eq!(detect_format(&cleaned.bytes), Some(ImageFormat::Jpeg));
    }

    #[test]
    fn rejects_input_over_the_configured_byte_limit() {
        let input = make_png_with_text_chunk();
        let opts = CleanOptions {
            max_input_bytes: Some(10),
            ..Default::default()
        };
        let err = clean(&input, &opts).unwrap_err();
        assert!(matches!(err, CleanError::InputTooLarge { max: 10, .. }));
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(data);
        let crc = crc32(&chunk[4..]);
        chunk.extend_from_slice(&crc.to_be_bytes());
        chunk
    }

    /// A well-formed PNG header declaring an enormous canvas, paired with a
    /// tiny (and never-decoded) IDAT — exactly the shape of a
    /// decompression-bomb attempt: a few hundred bytes on disk claiming a
    /// canvas that would need gigabytes of RAM to decode.
    fn make_png_with_huge_dimensions(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr_data = Vec::new();
        ihdr_data.extend_from_slice(&width.to_be_bytes());
        ihdr_data.extend_from_slice(&height.to_be_bytes());
        ihdr_data.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace

        let mut out = Vec::new();
        out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // PNG signature
        out.extend_from_slice(&png_chunk(b"IHDR", &ihdr_data));
        out.extend_from_slice(&png_chunk(b"IDAT", &[0u8; 8])); // never reached
        out.extend_from_slice(&png_chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn rejects_decompression_bomb_dimensions() {
        let input = make_png_with_huge_dimensions(60_000, 60_000);
        let opts = CleanOptions {
            max_image_dimension: Some(DEFAULT_MAX_IMAGE_DIMENSION),
            ..Default::default()
        };
        let err = clean(&input, &opts).unwrap_err();
        assert!(
            matches!(err, CleanError::Decode(_)),
            "expected a Decode(Limits) error, got {err:?}"
        );
    }

    #[test]
    fn accepts_dimensions_within_the_configured_limit() {
        // A genuine, fully-decodable 4x4 PNG — well within any sane limit.
        let input = make_png_with_text_chunk();
        let opts = CleanOptions {
            max_image_dimension: Some(1000),
            reset_fingerprint: false,
            ..Default::default()
        };
        let cleaned = clean(&input, &opts).expect("4x4 is within the 1000px limit");
        assert_eq!(cleaned.report.width, 4);
        assert_eq!(cleaned.report.height, 4);
    }
}
