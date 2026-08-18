//! Real AI super-resolution (Real-ESRGAN, fixed 4x) via a bundled ONNX
//! model — as opposed to `metacleaner-core`'s classical Lanczos3 upscale,
//! which can only smooth/redistribute pixels that already exist. This
//! model was *trained* on paired low-res/high-res photos to hallucinate
//! plausible high-frequency detail during upscaling. That's a real quality
//! win for genuinely low-res input, but it means the output is no longer a
//! strictly faithful representation of the original pixels — the added
//! detail is invented, not recovered. Callers should treat this as a
//! distinct, clearly-labeled operation from anything else in this tool.
//!
//! Separate crate from `metacleaner-core` on purpose: this pulls in a
//! native ONNX Runtime and a bundled ~5MB model, dependencies
//! `metacleaner-core` deliberately avoids so it stays reusable from a
//! future WASM build.
//!
//! Model: `real_esrgan_general_x4v3`, BSD-3-Clause, Qualcomm AI Hub's ONNX
//! export of <https://github.com/xinntao/Real-ESRGAN> (see `models/`). It
//! takes a **fixed 128x128** RGB tile and produces a fixed 512x512 (4x)
//! tile — there is no arbitrary-resolution mode. Arbitrary-size images are
//! handled here by tiling: each 128x128 window is extracted with a border
//! of context around a smaller "core" region (edge-replicated at image
//! boundaries), run through the model, then only the core's upscaled
//! output is kept and stitched into the final image — the context border
//! exists purely so tile seams don't fall on a hard boundary with zero
//! surrounding information.

use image::{ImageBuffer, Rgb, RgbImage, RgbaImage};
use ort::session::Session;
use ort::value::Tensor;

const MODEL_ONNX: &[u8] = include_bytes!("../models/real_esrgan_general_x4v3.onnx");
const MODEL_DATA: &[u8] = include_bytes!("../models/real_esrgan_general_x4v3.data");
const MODEL_ONNX_FILENAME: &str = "real_esrgan_general_x4v3.onnx";
const MODEL_DATA_FILENAME: &str = "real_esrgan_general_x4v3.data";

const INPUT_NAME: &str = "image";
const OUTPUT_NAME: &str = "upscaled_image";

/// The model's fixed tile input size (see `models/metadata.json`).
const TILE_SIZE: u32 = 128;
/// The model's fixed scale factor.
pub const SCALE: u32 = 4;
/// Context border (in input-space pixels) kept around each tile's "core"
/// region so the model always sees real surrounding content, not a hard
/// crop edge. Keeping this modest relative to `TILE_SIZE` maximizes the
/// core region processed per inference call.
const OVERLAP: u32 = 8;
const CORE: u32 = TILE_SIZE - 2 * OVERLAP;

#[derive(Debug, thiserror::Error)]
pub enum AiUpscaleError {
    #[error("failed to prepare bundled model in the temp directory: {0}")]
    ModelSetup(#[from] std::io::Error),
    #[error("ONNX Runtime error: {0}")]
    Ort(#[from] ort::Error),
    #[error("model produced an unexpected output shape: {0:?}")]
    UnexpectedOutputShape(Vec<i64>),
}

/// A loaded model session, ready to upscale images. Loading involves
/// extracting the bundled model to a temp file (ONNX Runtime needs a real
/// file path to resolve the external-data weights file next to it) and
/// initializing ONNX Runtime, so construct one and reuse it rather than
/// creating a new one per image.
pub struct AiUpscaler {
    session: Session,
}

impl AiUpscaler {
    pub fn load() -> Result<Self, AiUpscaleError> {
        let dir = std::env::temp_dir().join("metacleaner-ai-model");
        std::fs::create_dir_all(&dir)?;
        let onnx_path = dir.join(MODEL_ONNX_FILENAME);
        let data_path = dir.join(MODEL_DATA_FILENAME);
        write_if_size_mismatched(&onnx_path, MODEL_ONNX)?;
        write_if_size_mismatched(&data_path, MODEL_DATA)?;

        let session = Session::builder()?.commit_from_file(&onnx_path)?;
        Ok(Self { session })
    }

    /// Upscale `img` by exactly [`SCALE`]x using tiled inference. Alpha is
    /// dropped (the model is RGB-only) and replaced with a fully-opaque
    /// channel at the new resolution — same tradeoff `clean()` already
    /// makes for formats without alpha support.
    pub fn upscale(&mut self, img: &RgbaImage) -> Result<RgbaImage, AiUpscaleError> {
        let rgb = image::DynamicImage::ImageRgba8(img.clone()).into_rgb8();
        let (width, height) = rgb.dimensions();

        let mut out: RgbImage = ImageBuffer::new(width * SCALE, height * SCALE);

        let mut y = 0u32;
        while y < height {
            let core_h = CORE.min(height - y);
            let mut x = 0u32;
            while x < width {
                let core_w = CORE.min(width - x);

                let tile = extract_tile(&rgb, x as i64 - OVERLAP as i64, y as i64 - OVERLAP as i64);
                let tile_out = self.infer_tile(&tile)?;

                let sub = image::imageops::crop_imm(
                    &tile_out,
                    OVERLAP * SCALE,
                    OVERLAP * SCALE,
                    core_w * SCALE,
                    core_h * SCALE,
                )
                .to_image();
                image::imageops::replace(&mut out, &sub, (x * SCALE) as i64, (y * SCALE) as i64);

                x += core_w;
            }
            y += core_h;
        }

        Ok(image::DynamicImage::ImageRgb8(out).into_rgba8())
    }

    fn infer_tile(&mut self, tile: &RgbImage) -> Result<RgbImage, AiUpscaleError> {
        debug_assert_eq!(tile.dimensions(), (TILE_SIZE, TILE_SIZE));

        let plane = (TILE_SIZE * TILE_SIZE) as usize;
        let mut data = vec![0f32; 3 * plane];
        for (i, px) in tile.pixels().enumerate() {
            data[i] = f32::from(px[0]) / 255.0;
            data[plane + i] = f32::from(px[1]) / 255.0;
            data[2 * plane + i] = f32::from(px[2]) / 255.0;
        }

        let input =
            Tensor::from_array(([1i64, 3, i64::from(TILE_SIZE), i64::from(TILE_SIZE)], data))?;
        let outputs = self.session.run(ort::inputs![INPUT_NAME => input])?;
        let (shape, out_data) = outputs[OUTPUT_NAME].try_extract_tensor::<f32>()?;

        let dims = shape.as_ref();
        let [_, _, out_h, out_w] = *dims else {
            return Err(AiUpscaleError::UnexpectedOutputShape(dims.to_vec()));
        };
        let (out_w, out_h) = (out_w as u32, out_h as u32);
        let out_plane = (out_w * out_h) as usize;

        Ok(ImageBuffer::from_fn(out_w, out_h, |x, y| {
            let idx = (y * out_w + x) as usize;
            let channel =
                |base: usize| (out_data[base + idx].clamp(0.0, 1.0) * 255.0).round() as u8;
            Rgb([channel(0), channel(out_plane), channel(2 * out_plane)])
        }))
    }
}

/// Extract a `TILE_SIZE x TILE_SIZE` window from `rgb`, anchored at
/// `(origin_x, origin_y)` in the source image's coordinate space. The
/// origin (and the window generally) may run outside the image bounds —
/// out-of-bounds samples are edge-replicated rather than causing an error,
/// so tiles near the image border still get a full-size, sensible input.
fn extract_tile(rgb: &RgbImage, origin_x: i64, origin_y: i64) -> RgbImage {
    let (w, h) = rgb.dimensions();
    ImageBuffer::from_fn(TILE_SIZE, TILE_SIZE, |tx, ty| {
        let sx = (origin_x + i64::from(tx)).clamp(0, i64::from(w) - 1) as u32;
        let sy = (origin_y + i64::from(ty)).clamp(0, i64::from(h) - 1) as u32;
        *rgb.get_pixel(sx, sy)
    })
}

/// Writes `bytes` to `path` unless a file of the exact same size is
/// already there (a cheap, good-enough check to avoid rewriting the model
/// on every single process start once it's been extracted once).
fn write_if_size_mismatched(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() as usize == bytes.len() {
            return Ok(());
        }
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn upscales_a_tiny_image_by_the_expected_factor() {
        let input: RgbaImage = ImageBuffer::from_fn(37, 29, |x, y| {
            Rgba([(x * 6) as u8, (y * 8) as u8, 100, 255])
        });
        let mut upscaler = AiUpscaler::load().expect("model should load");
        let output = upscaler.upscale(&input).expect("upscale should succeed");
        assert_eq!(output.width(), 37 * SCALE);
        assert_eq!(output.height(), 29 * SCALE);
    }

    #[test]
    fn upscales_an_image_larger_than_one_tile() {
        // Exercises the multi-tile stitching path (CORE=112, so 200px
        // needs two tiles per axis).
        let input: RgbaImage = ImageBuffer::from_fn(200, 150, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut upscaler = AiUpscaler::load().expect("model should load");
        let output = upscaler.upscale(&input).expect("upscale should succeed");
        assert_eq!(output.width(), 200 * SCALE);
        assert_eq!(output.height(), 150 * SCALE);
    }
}
