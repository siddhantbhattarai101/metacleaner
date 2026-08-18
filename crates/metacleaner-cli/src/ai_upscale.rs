//! Orchestrates `metacleaner-ai`'s AI super-resolution as a pre-processing
//! step ahead of `metacleaner_core::clean()`. Runs entirely independently
//! of the classical clean/inspect pipeline: decode the original bytes,
//! upscale the pixels, re-encode as a fresh PNG (metadata-free by
//! construction) and hand that to `clean()` as if it were the input —
//! `clean()` then still applies whatever fingerprint reset / classical
//! enhance / output-format conversion was also requested, on top of the
//! AI-upscaled pixels.

use std::io::Cursor;

use metacleaner_ai::AiUpscaler;

/// CPU-only tiled inference time scales with tile count, and this is meant
/// for genuinely low-res source images ("low quality to HD"), not full-size
/// photos — cap input dimensions so a request doesn't hang for minutes.
const MAX_AI_UPSCALE_INPUT_DIMENSION: u32 = 1600;

pub fn apply_ai_upscale(upscaler: &mut AiUpscaler, input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() as u64 > metacleaner_core::DEFAULT_MAX_INPUT_BYTES {
        return Err(format!(
            "input is {} bytes, exceeds the {}-byte limit",
            input.len(),
            metacleaner_core::DEFAULT_MAX_INPUT_BYTES
        ));
    }

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_AI_UPSCALE_INPUT_DIMENSION);
    limits.max_image_height = Some(MAX_AI_UPSCALE_INPUT_DIMENSION);

    let format = image::guess_format(input).map_err(|e| e.to_string())?;
    let mut decoder = image::ImageReader::with_format(Cursor::new(input), format)
        .into_decoder()
        .map_err(|e| e.to_string())?;
    apply_decoder_limits(&mut decoder, limits)?;
    let decoded = image::DynamicImage::from_decoder(decoder).map_err(|e| {
        format!(
            "failed to decode image for AI upscale (this includes the {MAX_AI_UPSCALE_INPUT_DIMENSION}px-per-side limit): {e}"
        )
    })?;
    let rgba = decoded.into_rgba8();

    let upscaled = upscaler
        .upscale(&rgba)
        .map_err(|e| format!("AI upscale failed: {e}"))?;

    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(upscaled)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn apply_decoder_limits(
    decoder: &mut impl image::ImageDecoder,
    limits: image::Limits,
) -> Result<(), String> {
    decoder.set_limits(limits).map_err(|e| e.to_string())
}
