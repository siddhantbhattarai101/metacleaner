//! Non-destructive metadata inspection.
//!
//! `clean()` strips metadata by fully decoding and re-encoding pixels, and
//! never has to know what any given segment/chunk actually is. `inspect()`
//! is the opposite: it never touches pixel data, and instead walks each
//! format's container structure (JPEG APPn markers, PNG chunks, WebP RIFF
//! chunks) to report what metadata is present *before* anything is removed.
//!
//! Every walker here is defensive by construction: input is untrusted and
//! may be truncated, malformed, or deliberately adversarial, so all offset
//! arithmetic is checked and any inconsistency simply ends the walk early
//! (fewer findings) rather than panicking or reading out of bounds.

use std::io::Cursor;

use image::ImageDecoder;

use crate::{
    detect_format, CleanError, ImageFormat, DEFAULT_MAX_IMAGE_DIMENSION, DEFAULT_MAX_INPUT_BYTES,
};

/// What kind of metadata a [`Finding`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingCategory {
    Exif,
    Gps,
    Xmp,
    Iptc,
    IccProfile,
    C2pa,
    AiGenerator,
    Unknown,
}

impl FindingCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingCategory::Exif => "exif",
            FindingCategory::Gps => "gps",
            FindingCategory::Xmp => "xmp",
            FindingCategory::Iptc => "iptc",
            FindingCategory::IccProfile => "icc-profile",
            FindingCategory::C2pa => "c2pa",
            FindingCategory::AiGenerator => "ai-generator",
            FindingCategory::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for FindingCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One piece of metadata found inside a container segment/chunk.
#[derive(Debug, Clone)]
pub struct Finding {
    pub category: FindingCategory,
    pub label: String,
    pub size_bytes: usize,
}

/// Result of inspecting an image without modifying it.
#[derive(Debug, Clone)]
pub struct InspectReport {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub findings: Vec<Finding>,
}

impl InspectReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Options controlling `inspect()`. Mirrors [`crate::CleanOptions`]'s
/// decompression-bomb guard so inspecting untrusted input is exactly as
/// safe as cleaning it.
#[derive(Debug, Clone)]
pub struct InspectOptions {
    pub max_input_bytes: Option<u64>,
    pub max_image_dimension: Option<u32>,
}

impl Default for InspectOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: Some(DEFAULT_MAX_INPUT_BYTES),
            max_image_dimension: Some(DEFAULT_MAX_IMAGE_DIMENSION),
        }
    }
}

/// Report what metadata is present in `input` without modifying it.
pub fn inspect(input: &[u8], opts: &InspectOptions) -> Result<InspectReport, CleanError> {
    if let Some(max) = opts.max_input_bytes {
        if input.len() as u64 > max {
            return Err(CleanError::InputTooLarge {
                size: input.len(),
                max,
            });
        }
    }

    let format = detect_format(input).ok_or(CleanError::UnsupportedFormat)?;

    let mut limits = image::Limits::no_limits();
    limits.max_image_width = opts.max_image_dimension;
    limits.max_image_height = opts.max_image_dimension;
    let (width, height) = header_dimensions(input, format, limits)?;

    let findings = match format {
        ImageFormat::Jpeg => inspect_jpeg(input),
        ImageFormat::Png => inspect_png(input),
        ImageFormat::WebP => inspect_webp(input),
        ImageFormat::Bmp => Vec::new(), // classic BMP has no standard embedded-metadata mechanism
        ImageFormat::Gif => inspect_gif(input),
        ImageFormat::Tiff => inspect_tiff(input),
    };

    Ok(InspectReport {
        format,
        width,
        height,
        bytes: input.len(),
        findings,
    })
}

/// Read just the declared dimensions from a header, under the same
/// decompression-bomb `Limits` used by `clean()` — without ever allocating
/// a pixel buffer, since inspection doesn't need one.
fn header_dimensions(
    input: &[u8],
    format: ImageFormat,
    limits: image::Limits,
) -> Result<(u32, u32), CleanError> {
    let cursor = Cursor::new(input);
    let dims = match format {
        ImageFormat::Jpeg => {
            let mut decoder = image::codecs::jpeg::JpegDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            decoder.dimensions()
        }
        ImageFormat::Png => {
            let mut decoder = image::codecs::png::PngDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            decoder.dimensions()
        }
        ImageFormat::WebP => {
            let mut decoder = image::codecs::webp::WebPDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            decoder.dimensions()
        }
        ImageFormat::Bmp => {
            let mut decoder = image::codecs::bmp::BmpDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            decoder.dimensions()
        }
        ImageFormat::Gif => {
            // First frame's dimensions only, even for an animated GIF —
            // fine for a read-only report; `clean()` is the one that
            // refuses multi-frame input outright to avoid data loss.
            let mut decoder = image::codecs::gif::GifDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            decoder.dimensions()
        }
        ImageFormat::Tiff => {
            let mut decoder = image::codecs::tiff::TiffDecoder::new(cursor)?;
            decoder.set_limits(limits)?;
            decoder.dimensions()
        }
    };
    Ok(dims)
}

// ---------------------------------------------------------------------
// JPEG: walk APPn/COM marker segments after SOI, stop at SOS.
// ---------------------------------------------------------------------

fn inspect_jpeg(input: &[u8]) -> Vec<Finding> {
    let mut findings = Vec::new();
    if input.len() < 4 || input[0] != 0xFF || input[1] != 0xD8 {
        return findings;
    }

    let mut pos = 2usize;
    while pos < input.len() {
        // Skip any 0xFF fill bytes to land on the marker byte itself.
        if input[pos] != 0xFF {
            break;
        }
        let mut marker_pos = pos;
        while marker_pos < input.len() && input[marker_pos] == 0xFF {
            marker_pos += 1;
        }
        let Some(&marker) = input.get(marker_pos) else {
            break;
        };
        pos = marker_pos + 1;

        // Markers with no payload: TEM (0x01), RSTn/SOI/EOI (0xD0-0xD9).
        if marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
            if marker == 0xD9 {
                break; // EOI
            }
            continue;
        }
        if marker == 0xDA {
            break; // SOS: entropy-coded scan data follows, stop structural parse
        }

        let Some(len_bytes) = input.get(pos..pos.saturating_add(2)) else {
            break;
        };
        let seg_len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
        if seg_len < 2 {
            break;
        }
        let Some(seg_end) = pos.checked_add(seg_len) else {
            break;
        };
        let Some(payload) = input.get(pos.saturating_add(2)..seg_end) else {
            break;
        };
        classify_jpeg_segment(marker, payload, &mut findings);
        pos = seg_end;
    }
    findings
}

fn classify_jpeg_segment(marker: u8, payload: &[u8], findings: &mut Vec<Finding>) {
    match marker {
        0xE1 => {
            if let Some(exif) = payload.strip_prefix(b"Exif\0\0") {
                let category = if tiff_has_gps(exif) {
                    FindingCategory::Gps
                } else {
                    FindingCategory::Exif
                };
                findings.push(Finding {
                    category,
                    label: exif_label(category),
                    size_bytes: payload.len(),
                });
            } else if payload.starts_with(b"http://ns.adobe.com/xap/") {
                findings.push(Finding {
                    category: FindingCategory::Xmp,
                    label: "XMP metadata".into(),
                    size_bytes: payload.len(),
                });
                if xmp_has_c2pa_or_ai_hint(payload) {
                    findings.push(Finding {
                        category: FindingCategory::C2pa,
                        label: "Possible C2PA/AI-provenance marker inside XMP".into(),
                        size_bytes: payload.len(),
                    });
                }
            } else {
                findings.push(Finding {
                    category: FindingCategory::Unknown,
                    label: "Unrecognized APP1 segment".into(),
                    size_bytes: payload.len(),
                });
            }
        }
        0xE2 if payload.starts_with(b"ICC_PROFILE\0") => {
            findings.push(Finding {
                category: FindingCategory::IccProfile,
                label: "ICC color profile".into(),
                size_bytes: payload.len(),
            });
        }
        0xEB => {
            findings.push(Finding {
                category: FindingCategory::C2pa,
                label: "APP11 segment (JPEG's home for C2PA/JUMBF content credentials)".into(),
                size_bytes: payload.len(),
            });
        }
        0xED if payload.windows(4).any(|w| w == b"8BIM") => {
            findings.push(Finding {
                category: FindingCategory::Iptc,
                label: "IPTC/Photoshop metadata (APP13)".into(),
                size_bytes: payload.len(),
            });
        }
        0xFE => {
            let is_ai = looks_like_ai_generation_text(payload);
            findings.push(Finding {
                category: if is_ai {
                    FindingCategory::AiGenerator
                } else {
                    FindingCategory::Unknown
                },
                label: if is_ai {
                    "AI-generation parameters in JPEG comment".into()
                } else {
                    "JPEG comment segment".into()
                },
                size_bytes: payload.len(),
            });
        }
        0xE0 => { /* APP0 JFIF header: standard, not privacy metadata */ }
        0xE3..=0xEF => {
            findings.push(Finding {
                category: FindingCategory::Unknown,
                label: format!("APP{} metadata segment", marker - 0xE0),
                size_bytes: payload.len(),
            });
        }
        _ => {}
    }
}

fn exif_label(category: FindingCategory) -> String {
    match category {
        FindingCategory::Gps => "EXIF metadata with GPS location".into(),
        _ => "EXIF metadata".into(),
    }
}

// ---------------------------------------------------------------------
// PNG: walk chunks after the 8-byte signature.
// ---------------------------------------------------------------------

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

fn inspect_png(input: &[u8]) -> Vec<Finding> {
    let mut findings = Vec::new();
    if input.len() < 8 || input[0..8] != PNG_SIGNATURE {
        return findings;
    }

    let mut pos = 8usize;
    loop {
        let Some(header) = input.get(pos..pos.saturating_add(8)) else {
            break;
        };
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = [header[4], header[5], header[6], header[7]];

        let Some(data_start) = pos.checked_add(8) else {
            break;
        };
        let Some(data_end) = data_start.checked_add(len) else {
            break;
        };
        let Some(crc_end) = data_end.checked_add(4) else {
            break;
        };
        let Some(data) = input.get(data_start..data_end) else {
            break;
        };
        if crc_end > input.len() {
            break;
        }

        classify_png_chunk(&kind, data, &mut findings);
        pos = crc_end;
        if &kind == b"IEND" {
            break;
        }
    }
    findings
}

const PNG_BENIGN_CHUNKS: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"tIME", b"pHYs",
    b"bKGD", b"sBIT", b"hIST", b"sPLT", b"acTL", b"fcTL", b"fdAT",
];

fn classify_png_chunk(kind: &[u8; 4], data: &[u8], findings: &mut Vec<Finding>) {
    match kind {
        b"tEXt" | b"zTXt" | b"iTXt" => {
            let keyword = extract_png_text_keyword(data);
            if keyword.as_deref() == Some("XML:com.adobe.xmp") {
                findings.push(Finding {
                    category: FindingCategory::Xmp,
                    label: "XMP metadata (iTXt)".into(),
                    size_bytes: data.len(),
                });
            } else if matches!(
                keyword.as_deref(),
                Some("parameters" | "prompt" | "workflow")
            ) || looks_like_ai_generation_text(data)
            {
                let name = keyword.unwrap_or_else(|| "text".to_string());
                findings.push(Finding {
                    category: FindingCategory::AiGenerator,
                    label: format!("AI-generation parameters (\"{name}\" chunk)"),
                    size_bytes: data.len(),
                });
            } else {
                findings.push(Finding {
                    category: FindingCategory::Unknown,
                    label: format!(
                        "Text metadata (\"{}\")",
                        keyword.as_deref().unwrap_or("unknown")
                    ),
                    size_bytes: data.len(),
                });
            }
        }
        b"eXIf" => {
            let category = if tiff_has_gps(data) {
                FindingCategory::Gps
            } else {
                FindingCategory::Exif
            };
            findings.push(Finding {
                category,
                label: exif_label(category),
                size_bytes: data.len(),
            });
        }
        b"iCCP" => {
            findings.push(Finding {
                category: FindingCategory::IccProfile,
                label: "ICC color profile".into(),
                size_bytes: data.len(),
            });
        }
        b"caBX" => {
            findings.push(Finding {
                category: FindingCategory::C2pa,
                label: "C2PA content credentials (caBX chunk)".into(),
                size_bytes: data.len(),
            });
        }
        _ if PNG_BENIGN_CHUNKS.contains(&kind) => { /* structural/color, not privacy metadata */ }
        _ => {
            let name = std::str::from_utf8(kind).unwrap_or("????");
            findings.push(Finding {
                category: FindingCategory::Unknown,
                label: format!("Unrecognized chunk \"{name}\""),
                size_bytes: data.len(),
            });
        }
    }
}

/// PNG text chunks (tEXt/zTXt/iTXt) all start with a null-terminated
/// keyword (1-79 bytes per spec), regardless of what follows it.
fn extract_png_text_keyword(data: &[u8]) -> Option<String> {
    let nul = data.iter().position(|&b| b == 0)?;
    if nul == 0 || nul > 79 {
        return None;
    }
    std::str::from_utf8(&data[..nul]).ok().map(String::from)
}

// ---------------------------------------------------------------------
// WebP: walk RIFF chunks after the 12-byte "RIFF"+size+"WEBP" header.
// ---------------------------------------------------------------------

fn inspect_webp(input: &[u8]) -> Vec<Finding> {
    let mut findings = Vec::new();
    if input.len() < 12 || &input[0..4] != b"RIFF" || &input[8..12] != b"WEBP" {
        return findings;
    }

    let mut pos = 12usize;
    while let Some(header) = input.get(pos..pos.saturating_add(8)) {
        let fourcc = [header[0], header[1], header[2], header[3]];
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;

        let Some(data_start) = pos.checked_add(8) else {
            break;
        };
        let Some(data_end) = data_start.checked_add(len) else {
            break;
        };
        let Some(data) = input.get(data_start..data_end) else {
            break;
        };

        classify_webp_chunk(&fourcc, data, &mut findings);

        // RIFF chunks are padded to an even length.
        let padded_len = len + (len % 2);
        let Some(next) = data_start.checked_add(padded_len) else {
            break;
        };
        pos = next;
    }
    findings
}

const WEBP_BENIGN_CHUNKS: &[&[u8; 4]] = &[b"VP8 ", b"VP8L", b"VP8X", b"ANIM", b"ANMF", b"ALPH"];

fn classify_webp_chunk(fourcc: &[u8; 4], data: &[u8], findings: &mut Vec<Finding>) {
    match fourcc {
        b"EXIF" => {
            let category = if tiff_has_gps(data) {
                FindingCategory::Gps
            } else {
                FindingCategory::Exif
            };
            findings.push(Finding {
                category,
                label: exif_label(category),
                size_bytes: data.len(),
            });
        }
        b"XMP " => {
            findings.push(Finding {
                category: FindingCategory::Xmp,
                label: "XMP metadata".into(),
                size_bytes: data.len(),
            });
            if xmp_has_c2pa_or_ai_hint(data) {
                findings.push(Finding {
                    category: FindingCategory::C2pa,
                    label: "Possible C2PA/AI-provenance marker inside XMP".into(),
                    size_bytes: data.len(),
                });
            }
        }
        b"ICCP" => {
            findings.push(Finding {
                category: FindingCategory::IccProfile,
                label: "ICC color profile".into(),
                size_bytes: data.len(),
            });
        }
        _ if WEBP_BENIGN_CHUNKS.contains(&fourcc) => { /* pixel/animation data */ }
        _ => {
            let name = std::str::from_utf8(fourcc).unwrap_or("????");
            findings.push(Finding {
                category: FindingCategory::Unknown,
                label: format!("Unrecognized WebP chunk \"{name}\""),
                size_bytes: data.len(),
            });
        }
    }
}

// ---------------------------------------------------------------------
// Shared heuristics
// ---------------------------------------------------------------------

/// Bounded substring scan (first 64 KiB only, so a deliberately huge chunk
/// can't cost much CPU) for known Stable-Diffusion-family markers.
fn looks_like_ai_generation_text(data: &[u8]) -> bool {
    const MARKERS: &[&[u8]] = &[
        b"Steps:",
        b"Sampler:",
        b"CFG scale:",
        b"Seed:",
        b"Model hash:",
        b"Negative prompt:",
        b"\"prompt\"",
        b"\"workflow\"",
    ];
    contains_any(data, MARKERS)
}

/// Bounded substring scan for C2PA / AI-provenance hints inside an XMP packet.
fn xmp_has_c2pa_or_ai_hint(data: &[u8]) -> bool {
    const MARKERS: &[&[u8]] = &[
        b"c2pa",
        b"C2PA",
        b"digitalSourceType",
        b"compositeSynthetic",
        b"trainedAlgorithmicMedia",
        b"Content Credentials",
        b"caiSignature",
    ];
    contains_any(data, MARKERS)
}

fn contains_any(data: &[u8], markers: &[&[u8]]) -> bool {
    let sample = &data[..data.len().min(65536)];
    markers
        .iter()
        .any(|m| !m.is_empty() && sample.windows(m.len()).any(|w| w == *m))
}

/// Minimal, fully bounds-checked TIFF/EXIF IFD0 walk that only asks: is
/// there a GPSInfo IFD pointer (tag 0x8825)? Never allocates, never
/// panics on truncated/malformed input — every offset is checked, so
/// worst case this just returns `false`.
fn tiff_has_gps(data: &[u8]) -> bool {
    tiff_ifd0_tags(data).contains(&0x8825)
}

/// Every tag id present in a TIFF/EXIF blob's IFD0. Shared by GPS detection
/// above and the standalone-TIFF inspector below. Fully bounds-checked;
/// any inconsistency just yields an empty list.
fn tiff_ifd0_tags(data: &[u8]) -> Vec<u16> {
    tiff_scan_ifd0_tags(data).unwrap_or_default()
}

fn tiff_scan_ifd0_tags(data: &[u8]) -> Option<Vec<u16>> {
    let little_endian = match data.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read_u16 = |off: usize| -> Option<u16> {
        let end = off.checked_add(2)?;
        let b = data.get(off..end)?;
        Some(if little_endian {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let read_u32 = |off: usize| -> Option<u32> {
        let end = off.checked_add(4)?;
        let b = data.get(off..end)?;
        Some(if little_endian {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };

    if read_u16(2)? != 42 {
        return None;
    }
    let ifd0_offset = read_u32(4)? as usize;
    let entry_count = read_u16(ifd0_offset)? as usize;

    let mut tags = Vec::with_capacity(entry_count.min(1024));
    for i in 0..entry_count {
        let step = i.checked_mul(12)?;
        let entry_off = ifd0_offset.checked_add(2)?.checked_add(step)?;
        tags.push(read_u16(entry_off)?);
    }
    Some(tags)
}

// ---------------------------------------------------------------------
// GIF: walk blocks after the header + Logical Screen Descriptor.
// ---------------------------------------------------------------------

fn inspect_gif(input: &[u8]) -> Vec<Finding> {
    scan_gif(input).unwrap_or_default()
}

fn scan_gif(data: &[u8]) -> Option<Vec<Finding>> {
    let mut findings = Vec::new();
    if data.len() < 13 || &data[0..3] != b"GIF" {
        return Some(findings);
    }
    let packed = data[10];
    let mut pos = 13usize;
    if packed & 0x80 != 0 {
        let gct_size = 3usize * (1usize << (u32::from(packed & 0x07) + 1));
        pos = pos.checked_add(gct_size)?;
    }

    loop {
        let Some(&introducer) = data.get(pos) else {
            break;
        };
        pos = pos.checked_add(1)?;
        match introducer {
            0x21 => {
                let label = *data.get(pos)?;
                pos = pos.checked_add(1)?;
                match label {
                    0xFE => {
                        // Comment Extension: free-form text, worth scanning.
                        let (comment, new_pos) = read_gif_sub_blocks(data, pos)?;
                        let is_ai = looks_like_ai_generation_text(&comment);
                        findings.push(Finding {
                            category: if is_ai {
                                FindingCategory::AiGenerator
                            } else {
                                FindingCategory::Unknown
                            },
                            label: if is_ai {
                                "AI-generation parameters in GIF comment".into()
                            } else {
                                "GIF comment extension".into()
                            },
                            size_bytes: comment.len(),
                        });
                        pos = new_pos;
                    }
                    0xFF => {
                        // Application Extension: first 11 bytes identify the app.
                        // "NETSCAPE2.0" is standard loop-count signaling, not
                        // metadata; anything else gets surfaced.
                        let (app_data, new_pos) = read_gif_sub_blocks(data, pos)?;
                        let app_id = app_data
                            .get(..11)
                            .and_then(|b| std::str::from_utf8(b).ok())
                            .unwrap_or("");
                        if app_id != "NETSCAPE2.0" {
                            findings.push(Finding {
                                category: FindingCategory::Unknown,
                                label: format!("GIF application extension (\"{app_id}\")"),
                                size_bytes: app_data.len(),
                            });
                        }
                        pos = new_pos;
                    }
                    _ => {
                        // Graphic Control / Plain Text / reserved: structural, skip.
                        pos = skip_gif_sub_blocks(data, pos)?;
                    }
                }
            }
            0x2C => {
                // Image Descriptor: skip past this frame's pixel data.
                let desc = data.get(pos..pos.checked_add(9)?)?;
                let local_packed = desc[8];
                pos = pos.checked_add(9)?;
                if local_packed & 0x80 != 0 {
                    let lct_size = 3usize * (1usize << (u32::from(local_packed & 0x07) + 1));
                    pos = pos.checked_add(lct_size)?;
                }
                pos = pos.checked_add(1)?; // LZW minimum code size
                pos = skip_gif_sub_blocks(data, pos)?;
            }
            0x3B => break, // trailer
            _ => break,    // malformed; let the real decoder report it
        }
    }
    Some(findings)
}

/// Read a GIF sub-block chain (length-prefixed blocks terminated by a
/// zero-length block), concatenating their contents.
fn read_gif_sub_blocks(data: &[u8], mut pos: usize) -> Option<(Vec<u8>, usize)> {
    let mut collected = Vec::new();
    loop {
        let size = *data.get(pos)?;
        pos = pos.checked_add(1)?;
        if size == 0 {
            return Some((collected, pos));
        }
        let end = pos.checked_add(size as usize)?;
        collected.extend_from_slice(data.get(pos..end)?);
        pos = end;
    }
}

/// Same as [`read_gif_sub_blocks`] but discards the content — used when we
/// only need to skip past a block we don't inspect.
fn skip_gif_sub_blocks(data: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let size = *data.get(pos)?;
        pos = pos.checked_add(1)?;
        if size == 0 {
            return Some(pos);
        }
        pos = pos.checked_add(size as usize)?;
    }
}

// ---------------------------------------------------------------------
// TIFF: unlike the other formats, the whole file *is* a tag structure —
// check IFD0 for GPS and for tags that identify the device/author/tool.
// ---------------------------------------------------------------------

const TIFF_IDENTIFYING_TAGS: &[u16] = &[
    0x010F, // Make
    0x0110, // Model
    0x0131, // Software
    0x0132, // DateTime
    0x013B, // Artist
    0x8298, // Copyright
    0x8769, // Exif IFD pointer
];

fn inspect_tiff(input: &[u8]) -> Vec<Finding> {
    let tags = tiff_ifd0_tags(input);
    let mut findings = Vec::new();
    if tags.contains(&0x8825) {
        findings.push(Finding {
            category: FindingCategory::Gps,
            label: "TIFF tags include GPS location".into(),
            size_bytes: input.len(),
        });
    } else if tags.iter().any(|t| TIFF_IDENTIFYING_TAGS.contains(t)) {
        findings.push(Finding {
            category: FindingCategory::Exif,
            label: "TIFF tags include camera/software/authorship metadata".into(),
            size_bytes: input.len(),
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(data);
        chunk.extend_from_slice(&crc32(&chunk[4..]).to_be_bytes());
        chunk
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, entry) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *entry = c;
        }
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
        crc ^ 0xFFFF_FFFF
    }

    fn base_png(width: u32, height: u32) -> Vec<u8> {
        let img: image::RgbaImage = image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgba([(x * 20) as u8, (y * 20) as u8, 100, 255])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn splice_after_ihdr(base: &[u8], chunk: &[u8]) -> Vec<u8> {
        let insert_at = 8 + 25; // signature + IHDR chunk
        let mut out = base[..insert_at].to_vec();
        out.extend_from_slice(chunk);
        out.extend_from_slice(&base[insert_at..]);
        out
    }

    /// Minimal, valid, little-endian TIFF/EXIF blob with a single IFD0
    /// entry: the GPSInfo IFD pointer (tag 0x8825).
    fn tiff_with_gps_pointer() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        out.extend_from_slice(&1u16.to_le_bytes()); // entry count
        out.extend_from_slice(&0x8825u16.to_le_bytes()); // tag: GPSInfo
        out.extend_from_slice(&4u16.to_le_bytes()); // type: LONG
        out.extend_from_slice(&1u32.to_le_bytes()); // count
        out.extend_from_slice(&0u32.to_le_bytes()); // value/offset (unused)
        out.extend_from_slice(&0u32.to_le_bytes()); // next IFD offset
        out
    }

    /// Minimal, valid, little-endian TIFF/EXIF blob with no GPS tag.
    fn tiff_without_gps() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0x0132u16.to_le_bytes()); // tag: DateTime, not GPS
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    #[test]
    fn clean_image_has_no_findings() {
        let input = base_png(4, 4);
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report.is_clean());
        assert_eq!(report.width, 4);
        assert_eq!(report.height, 4);
    }

    #[test]
    fn detects_stable_diffusion_parameters_by_keyword() {
        let text = b"parameters\0a photo of a fox, Steps: 30, Sampler: Euler a";
        let input = splice_after_ihdr(&base_png(4, 4), &png_chunk(b"tEXt", text));
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::AiGenerator));
    }

    #[test]
    fn detects_gps_inside_png_exif_chunk() {
        let input = splice_after_ihdr(
            &base_png(4, 4),
            &png_chunk(b"eXIf", &tiff_with_gps_pointer()),
        );
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::Gps));
    }

    #[test]
    fn exif_without_gps_is_not_flagged_as_gps() {
        let input = splice_after_ihdr(&base_png(4, 4), &png_chunk(b"eXIf", &tiff_without_gps()));
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::Exif));
        assert!(!report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::Gps));
    }

    #[test]
    fn detects_c2pa_cabx_chunk() {
        let input = splice_after_ihdr(&base_png(4, 4), &png_chunk(b"caBX", b"fake jumbf box"));
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::C2pa));
    }

    fn base_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img: image::RgbImage = image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgb([(x * 10) as u8, (y * 10) as u8, 50])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .unwrap();
        out
    }

    fn splice_app11_after_soi(base: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut segment = vec![0xFF, 0xEB];
        segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(payload);

        let mut out = base[..2].to_vec(); // SOI
        out.extend_from_slice(&segment);
        out.extend_from_slice(&base[2..]);
        out
    }

    #[test]
    fn detects_c2pa_app11_segment_in_jpeg() {
        let input = splice_app11_after_soi(&base_jpeg(4, 4), b"jumb....c2pa manifest here");
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::C2pa));
        assert_eq!(report.format, ImageFormat::Jpeg);
    }

    #[test]
    fn clean_jpeg_has_no_findings() {
        let input = base_jpeg(4, 4);
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report.is_clean());
    }

    #[test]
    fn rejects_oversized_input() {
        let input = base_png(4, 4);
        let opts = InspectOptions {
            max_input_bytes: Some(10),
            ..Default::default()
        };
        let err = inspect(&input, &opts).unwrap_err();
        assert!(matches!(err, CleanError::InputTooLarge { max: 10, .. }));
    }

    fn base_gif(width: u32, height: u32) -> Vec<u8> {
        let img: image::RgbaImage = image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgba([(x * 40) as u8, (y * 40) as u8, 100, 255])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Gif)
            .unwrap();
        out
    }

    /// Position of the first block (Extension Introducer or Image
    /// Descriptor) after a real encoder's header + Logical Screen
    /// Descriptor + optional Global Color Table — the correct splice point
    /// for inserting a new, independent block.
    fn gif_first_block_pos(data: &[u8]) -> usize {
        let packed = data[10];
        let mut pos = 13usize;
        if packed & 0x80 != 0 {
            let gct_size = 3usize * (1usize << (u32::from(packed & 0x07) + 1));
            pos += gct_size;
        }
        pos
    }

    fn splice_gif_block(base: &[u8], block: &[u8]) -> Vec<u8> {
        let insert_at = gif_first_block_pos(base);
        let mut out = base[..insert_at].to_vec();
        out.extend_from_slice(block);
        out.extend_from_slice(&base[insert_at..]);
        out
    }

    fn build_gif_comment_extension(text: &[u8]) -> Vec<u8> {
        let mut out = vec![0x21, 0xFE];
        for chunk in text.chunks(255) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        out.push(0);
        out
    }

    #[test]
    fn clean_gif_has_no_findings() {
        let input = base_gif(4, 4);
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report.is_clean());
    }

    #[test]
    fn detects_ai_generation_text_in_gif_comment() {
        let comment = build_gif_comment_extension(b"Steps: 20, Sampler: Euler a, CFG scale: 7");
        let input = splice_gif_block(&base_gif(4, 4), &comment);
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == FindingCategory::AiGenerator));
    }

    #[test]
    fn detects_unusual_gif_application_extension() {
        let mut app_ext = vec![0x21, 0xFF, 11];
        app_ext.extend_from_slice(b"WEIRDAPP123"); // exactly 11 bytes
        app_ext.push(3);
        app_ext.extend_from_slice(b"abc");
        app_ext.push(0);
        let input = splice_gif_block(&base_gif(4, 4), &app_ext);
        let report = inspect(&input, &InspectOptions::default()).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.label.contains("WEIRDAPP123")));
    }

    #[test]
    fn inspect_tiff_detects_gps() {
        let findings = inspect_tiff(&tiff_with_gps_pointer());
        assert!(findings.iter().any(|f| f.category == FindingCategory::Gps));
    }

    #[test]
    fn inspect_tiff_detects_identifying_tags() {
        // tiff_without_gps() carries a DateTime tag (0x0132), which is on
        // the identifying-tags list but is not GPS.
        let findings = inspect_tiff(&tiff_without_gps());
        assert!(findings.iter().any(|f| f.category == FindingCategory::Exif));
        assert!(!findings.iter().any(|f| f.category == FindingCategory::Gps));
    }
}
