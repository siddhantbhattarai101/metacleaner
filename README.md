# metacleaner

A local, offline metadata cleaner and AI-tag remover for images, written in
Rust. Strips EXIF, GPS, XMP, IPTC, C2PA content credentials, and AI-generator
signatures (Stable Diffusion `tEXt`/`iTXt`/`zTXt` chunks, DALL-E/Midjourney/
Adobe Firefly fingerprints) from JPEG, PNG, and WebP images, and can
optionally reset the pixel-level fingerprint of the output so old copies
can't be hash-matched back to the source file.

No network calls, no server upload — everything runs on your machine.

## Why full decode/re-encode instead of parsing each metadata format?

JPEG APPn segments, PNG ancillary chunks, and WebP RIFF metadata chunks are
all *container-level* data that sits alongside — not inside — the pixel
data. Rather than hand-parsing every known tag scheme (EXIF, XMP, IPTC,
C2PA/JUMBF, plus whatever the next AI tool invents), `metacleaner-core`
fully decodes the image to raw pixels and re-encodes it from scratch. The
encoder only ever writes geometry + pixel data, so every non-pixel segment
is dropped by construction — including formats this tool has never heard of.

## Workspace layout

```
metacleaner/
├── crates/
│   ├── metacleaner-core/   # pure in-memory library: bytes in -> clean bytes out
│   │                       # no file or network I/O, so it's reusable from a
│   │                       # future wasm32-unknown-unknown build for a browser UI
│   └── metacleaner-cli/    # `metaclean` binary
└── Cargo.toml              # workspace
```

## Build

```bash
cargo build --release --workspace
# binary at target/release/metaclean
```

## Usage

```bash
# Clean a single image (writes photo-clean.jpg alongside the original)
metaclean photo.jpg

# Batch process many images at once
metaclean *.jpg *.png

# Write outputs to a specific directory
metaclean -o cleaned/ *.jpg

# Overwrite files in place
metaclean --in-place photo.jpg

# Skip the invisible pixel-fingerprint reset (metadata is still stripped)
metaclean --no-fingerprint-reset photo.jpg

# Tune the fingerprint reset (max per-channel delta, and fraction of pixels touched)
metaclean --fingerprint-strength 1 --fingerprint-fraction 0.5 photo.jpg

# Force output to a different container format
metaclean --format webp photo.png

# Control JPEG re-encode quality (1-100, default 92)
metaclean --jpeg-quality 90 photo.jpg

# Tune the decompression-bomb guard (defaults: 256 MB input, 12000px per side, 512 MB decoded)
metaclean --max-input-mb 100 --max-dimension 8000 --max-decoded-mb 256 photo.jpg
```

Run `metaclean --help` for the full flag list.

## Security: decompression-bomb guard

`clean()` rejects oversized or maliciously-crafted input before doing any
expensive work, in three layers:

1. **Raw byte size** — the CLI checks the file's size on disk (and the
   library checks `input.len()`) against `max_input_bytes` before reading or
   parsing anything.
2. **Declared dimensions** — a file's header can claim any width/height it
   wants regardless of how small the file actually is (e.g. a 65-byte PNG
   declaring a 60,000x60,000 canvas, which would need ~14 GB to decode).
   `metacleaner-core` reads each format's header via its own decoder, checks
   the declared dimensions against `max_image_dimension` via `image::Limits`,
   and rejects the file *before* allocating a pixel buffer.
3. **Decoded allocation size** — `max_decoded_bytes` bounds how much memory
   the decoder may allocate while reading pixel data, as a second, more
   general backstop.

All three are configurable via `CleanOptions` (library) or
`--max-input-mb` / `--max-dimension` / `--max-decoded-mb` (CLI), and can be
disabled individually by setting the corresponding `CleanOptions` field to
`None` if you trust your input source and want to process unusually large
legitimate images.

## What it removes

- EXIF (camera make/model, lens, ISO, aperture, shutter speed, software, timestamps)
- GPS / geotags
- XMP and IPTC (creator info, copyright, keywords, edit history)
- C2PA content credentials (Adobe Firefly, Photoshop, other Content Authenticity Initiative tools)
- Stable Diffusion generation parameters (Automatic1111, ComfyUI, Forge) stored in PNG `tEXt`/`iTXt`/`zTXt` chunks
- AI generator signatures embedded by DALL-E, Midjourney, Adobe Firefly, etc.
- The file's pixel-level fingerprint (optional, on by default)

## What it can't remove

Only data stored in file metadata / non-pixel container chunks is in scope.
Anything encoded into the pixels themselves — an invisible watermark such as
Google's SynthID, or signals a visual AI classifier looks for — is not
metadata and can't be stripped this way. Treat this as a privacy/provenance
tool, not a guarantee against AI-content detectors.

Supported formats: JPEG, PNG, WebP. HEIC/AVIF and video are not yet supported.

## Testing

```bash
cargo test --workspace
```

`metacleaner-core`'s tests build a PNG with a real Automatic1111-style
`parameters` tEXt chunk and assert it's absent from the cleaned output, among
other checks (format conversion, unsupported-format rejection, fingerprint
reset actually changing bytes, and the decompression-bomb guard rejecting a
PNG header that declares a 60,000x60,000 canvas).

## Roadmap

- `metacleaner-wasm`: thin `wasm-bindgen` wrapper around `metacleaner-core`
  for a browser-based, fully client-side UI (matches the original product
  brief: drag-and-drop, batch processing, nothing leaves the browser).
- HEIC/AVIF support.
- C2PA-aware reporting (surface *what* provenance data was found before
  stripping it, for audit trails).
