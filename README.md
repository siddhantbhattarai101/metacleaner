# metacleaner

A local, offline metadata cleaner and AI-tag remover for images, written in
Rust. Strips EXIF, GPS, XMP, IPTC, C2PA content credentials, and AI-generator
signatures (Stable Diffusion `tEXt`/`iTXt`/`zTXt` chunks, DALL-E/Midjourney/
Adobe Firefly fingerprints) from JPEG, PNG, WebP, BMP, GIF, and TIFF images,
and can optionally reset the pixel-level fingerprint of the output so old
copies can't be hash-matched back to the source file. A separate `inspect`
subcommand reports what metadata is present in a file without modifying it.
A `serve` subcommand runs a local web UI (drag-and-drop, batch, download) at
`http://127.0.0.1`, bound to loopback only.

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
│   └── metacleaner-cli/    # `metaclean` binary (clean / inspect / serve)
│       └── assets/         # embedded web UI (index.html/style.css/app.js),
│                            # compiled into the binary via include_str! —
│                            # no files to ship alongside it
└── Cargo.toml              # workspace
```

## Build

```bash
cargo build --release --workspace
# binary at target/release/metaclean
```

## Usage

`metaclean` has two subcommands: `inspect` (read-only — report what
metadata is present) and `clean` (destructive — strip it and write output).
Run `metaclean inspect photo.jpg` first if you want to know what's in a file
before deciding to clean it.

### `inspect` — report what's there, without touching the file

```bash
# Human-readable report
metaclean inspect photo.jpg

# Multiple files, machine-readable JSON (for scripting/CI)
metaclean inspect --json *.jpg *.png
```

Exits `0` if every file was clean and readable, `1` if any file has
findings or failed to parse — so `metaclean inspect *.jpg || echo "found metadata"`
works as a CI gate.

Example output:

```
$ metaclean inspect vacation.jpg
vacation.jpg  [Jpeg 4032x3024, 3841022 bytes]
  [gps] EXIF metadata with GPS location (2481 bytes)
  [icc-profile] ICC color profile (560 bytes)
```

`inspect` walks each format's container structure directly (JPEG APPn
markers, PNG chunks, WebP RIFF chunks) — it never decodes pixels, so it's
cheap and safe to run on files you haven't decided to trust yet.

### `clean` — strip it

```bash
# Clean a single image (writes photo-clean.jpg alongside the original)
metaclean clean photo.jpg

# Batch process many images at once
metaclean clean *.jpg *.png

# Write outputs to a specific directory
metaclean clean -o cleaned/ *.jpg

# Overwrite files in place
metaclean clean --in-place photo.jpg

# Skip the invisible pixel-fingerprint reset (metadata is still stripped)
metaclean clean --no-fingerprint-reset photo.jpg

# Tune the fingerprint reset (max per-channel delta, and fraction of pixels touched)
metaclean clean --fingerprint-strength 1 --fingerprint-fraction 0.5 photo.jpg

# Force output to a different container format
metaclean clean --format webp photo.png

# Control JPEG re-encode quality (1-100, default 92)
metaclean clean --jpeg-quality 90 photo.jpg

# Machine-readable JSON output
metaclean clean --json *.jpg

# Tune the decompression-bomb guard (defaults: 256 MB input, 12000px per side, 512 MB decoded)
metaclean clean --max-input-mb 100 --max-dimension 8000 --max-decoded-mb 256 photo.jpg
```

Both subcommands share the same decompression-bomb guard flags
(`--max-input-mb`, `--max-dimension`; `clean` additionally has
`--max-decoded-mb`), so inspecting an untrusted file is exactly as safe as
cleaning one.

Run `metaclean --help`, `metaclean clean --help`, or `metaclean inspect --help`
for the full flag list.

### `serve` — local web UI

```bash
# Starts on http://127.0.0.1:7878 and opens it in your default browser
metaclean serve

# Different port, don't auto-open a browser tab
metaclean serve --port 9000 --no-open
```

Drag files onto the page (or pick them); each is inspected automatically on
drop, showing findings inline. Adjust the fingerprint-reset/quality/format
options, then "Clean & download all" cleans every file server-side (via the
exact same `metacleaner-core` functions the CLI uses) and downloads each
result through the browser.

Binds to `127.0.0.1` (loopback) by default — nothing outside this machine
can reach it. `--host` exists to override this, but only do so if you
understand that anyone who can reach that address can then upload and
process files through it. There's no authentication: this is designed to
be a personal local tool, the same trust model as running a local Jupyter
notebook or dev server, not a multi-user service.

The HTML/CSS/JS are compiled into the `metaclean` binary itself
(`include_str!`) — `serve` needs nothing on disk beyond the binary, which
matters for shipping this as a single apt-installable package.

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

Supported formats: JPEG, PNG, WebP, BMP, GIF, TIFF. HEIC/AVIF and video are
not yet supported.

**Animated GIFs and multi-page TIFFs are refused outright, not silently
truncated.** The `image` crate's public decoder API for both formats only
exposes "decode the first frame/page" — there's no way to ask "is there
more than one?" through it. Rather than quietly cleaning only page 1 of a
scanned document or frame 1 of an animation and discarding the rest,
`clean()` does its own bounds-checked structural scan first (GIF block
walk / TIFF IFD-chain walk) and returns a clear `MultiFrameNotSupported`
error if it finds more than one. `inspect` still works fine on these files
(it only reports on the first frame/page, never discards anything since it
doesn't write output).

## Testing

```bash
cargo test --workspace
```

`metacleaner-core`'s tests build a PNG with a real Automatic1111-style
`parameters` tEXt chunk and assert it's absent from the cleaned output, among
other checks (format conversion, unsupported-format rejection, fingerprint
reset actually changing bytes, the decompression-bomb guard rejecting a PNG
header that declares a 60,000x60,000 canvas, a genuinely animated GIF and a
multi-page TIFF both being refused rather than silently truncated, and
`inspect()` correctly identifying GPS-bearing EXIF, C2PA/JUMBF segments,
Stable Diffusion parameters, and GIF comment/application extensions across
PNG, JPEG, GIF, and TIFF fixtures).

## Roadmap

- Package as a `.deb` (via `cargo-deb`) so `apt install metacleaner` gives
  you the `metaclean` binary — `clean`/`inspect` on the command line,
  `metaclean serve` for the local web UI. This is the near-term next step.
- Text/document format support (DOCX/PDF/EPUB/HTML/Markdown metadata,
  invisible-Unicode stripping) — a second wave once the image toolset and
  its web UI are solid, matching the fuller feature set of tools like
  `watermarks-remover`.
- Animated GIF / multi-page TIFF support — clean each frame/page instead of
  refusing the whole file (needs the multi-frame encode path, not just the
  single-frame one `clean()` uses today).
- HEIC/AVIF support.
- Real C2PA manifest parsing via the `c2pa` crate. Today `inspect` flags
  *that* a JPEG APP11 segment or PNG `caBX` chunk is present (C2PA's known
  homes in each container) but doesn't parse the JUMBF/manifest contents —
  so it can't yet tell you *who* signed a Content Credentials claim.
- Segment-preserving strip mode (via `img-parts`) that keeps the ICC profile
  and exact pixels while only dropping EXIF/XMP/C2PA segments, as an
  alternative to the current full re-encode for users who want zero
  quality/pixel change instead of a hash-busting fingerprint reset.
- `metacleaner-wasm`: a `wasm-bindgen` wrapper around `metacleaner-core`
  for a fully client-side, publicly-hostable web version (the `serve`
  subcommand's local server covers the "apt-installable local tool" use
  case; this would cover "deploy this as a public website" instead).
