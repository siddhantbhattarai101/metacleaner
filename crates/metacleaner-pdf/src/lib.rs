//! Strip identifying metadata from PDF files: the `/Info` dictionary
//! (Author, Producer, Creator, Title, Subject, Keywords, CreationDate,
//! ModDate, and any custom keys) and every XMP metadata stream (`/Type
//! /Metadata /Subtype /XML`). XMP can appear more than once — the
//! document Catalog's own packet, plus duplicates embedded inside images
//! or other objects — so every object in the file is scanned rather than
//! just the Catalog-referenced one.
//!
//! Approach mirrors `metacleaner-docs`: blunt, unconditional removal of
//! everything found in these known metadata containers, rather than
//! selectively targeting specific known-sensitive fields — the same
//! "don't miss something just because we didn't think to name it"
//! reasoning `metacleaner-core` applies to images.
//!
//! Scope note: this cleans `/Info` + XMP only, not the full PDF object
//! graph. A PDF can also carry identifying content in embedded file
//! attachments (`/EmbeddedFiles` name tree), form field values
//! (`/AcroForm`), and JavaScript actions (`/Names /JavaScript`,
//! `/OpenAction`) — those are real, known, and out of scope for this
//! first pass; flagged here rather than silently ignored, the same
//! documented-gap pattern `metacleaner-docs` uses for DOCX
//! tracked-change revision authors.
//!
//! Uses `lopdf` (pure Rust, no system libpoppler/mupdf dependency) to
//! read/rewrite the object graph directly, rather than fully
//! decode/re-encode the document the way `metacleaner-core` handles
//! images — PDF page content isn't practical to re-render losslessly
//! from scratch.

use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, Stream};

pub const DEFAULT_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_DECOMPRESSED_STREAM_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct PdfOptions {
    pub max_input_bytes: u64,
    /// Per-stream decompression cap, applied both while loading (object/
    /// xref streams) and when decoding an XMP packet for its finding
    /// preview. Guards against decompression-bomb-style attacks the same
    /// way `metacleaner-docs`'s zip-entry caps do.
    pub max_decompressed_stream_bytes: usize,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_decompressed_stream_bytes: DEFAULT_MAX_DECOMPRESSED_STREAM_BYTES,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("input is {size} bytes, which exceeds the {max}-byte limit")]
    InputTooLarge { size: usize, max: u64 },
    #[error("invalid or unsupported PDF: {0}")]
    Parse(#[from] lopdf::Error),
    #[error("I/O error while writing PDF: {0}")]
    Io(#[from] std::io::Error),
}

/// One identifying value found in `/Info` or an XMP metadata stream.
#[derive(Debug, Clone)]
pub struct PdfFinding {
    /// `"/Info"` for document-info entries, or `"XMP stream (object N
    /// G)"` for an XMP metadata packet.
    pub location: String,
    pub field: String,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct InspectPdfReport {
    pub findings: Vec<PdfFinding>,
}

impl InspectPdfReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct PdfCleanReport {
    pub bytes_in: usize,
    pub bytes_out: usize,
    /// Human-readable labels of what was stripped, e.g. "/Info
    /// dictionary (5 field(s))", "2 XMP metadata stream(s)".
    pub stripped: Vec<String>,
}

/// Report what would be stripped, without modifying `input`.
pub fn inspect_pdf(input: &[u8], opts: &PdfOptions) -> Result<InspectPdfReport, PdfError> {
    check_size(input, opts)?;
    let document = load(input, opts)?;

    let mut findings = info_findings(&document);
    findings.extend(xmp_findings(&document, opts));

    Ok(InspectPdfReport { findings })
}

/// Strip the `/Info` dictionary and all XMP metadata streams from
/// `input`. Page content, fonts, images, and every other object pass
/// through untouched (though `lopdf` re-serializes the object/xref
/// tables from scratch, so the output isn't byte-identical beyond that).
pub fn clean_pdf(input: &[u8], opts: &PdfOptions) -> Result<(Vec<u8>, PdfCleanReport), PdfError> {
    check_size(input, opts)?;
    let mut document = load(input, opts)?;

    let info = info_findings(&document);
    if !info.is_empty() {
        let info_id = trailer_info_id(&document);
        document.trailer.remove(b"Info");
        if let Some(id) = info_id {
            document.delete_object(id);
        }
    }

    let xmp_ids = xmp_stream_ids(&document);
    for id in &xmp_ids {
        if let Some(Object::Stream(stream)) = document.objects.get_mut(id) {
            stream.set_plain_content(Vec::new());
        }
    }

    let mut stripped = Vec::new();
    if !info.is_empty() {
        stripped.push(format!("/Info dictionary ({} field(s))", info.len()));
    }
    if !xmp_ids.is_empty() {
        stripped.push(format!("{} XMP metadata stream(s)", xmp_ids.len()));
    }

    let mut out = Vec::new();
    document.save_to(&mut out)?;
    let bytes_out = out.len();

    Ok((
        out,
        PdfCleanReport {
            bytes_in: input.len(),
            bytes_out,
            stripped,
        },
    ))
}

fn check_size(input: &[u8], opts: &PdfOptions) -> Result<(), PdfError> {
    if input.len() as u64 > opts.max_input_bytes {
        return Err(PdfError::InputTooLarge {
            size: input.len(),
            max: opts.max_input_bytes,
        });
    }
    Ok(())
}

fn load(input: &[u8], opts: &PdfOptions) -> Result<Document, PdfError> {
    let load_opts = LoadOptions {
        max_decompressed_size: Some(opts.max_decompressed_stream_bytes),
        ..Default::default()
    };
    Ok(Document::load_mem_with_options(input, load_opts)?)
}

fn trailer_info_id(document: &Document) -> Option<ObjectId> {
    match document.trailer.get(b"Info").ok()? {
        Object::Reference(id) => Some(*id),
        _ => None,
    }
}

fn resolve_dict<'a>(document: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => document.get_object(*id).ok().and_then(|o| o.as_dict().ok()),
        _ => None,
    }
}

fn info_findings(document: &Document) -> Vec<PdfFinding> {
    let Ok(info_obj) = document.trailer.get(b"Info") else {
        return Vec::new();
    };
    let Some(dict) = resolve_dict(document, info_obj) else {
        return Vec::new();
    };

    dict.iter()
        .filter_map(|(key, value)| {
            let bytes = value.as_str().ok()?;
            if bytes.is_empty() {
                return None;
            }
            Some(PdfFinding {
                location: "/Info".to_string(),
                field: String::from_utf8_lossy(key).to_string(),
                value: pdf_string_lossy(bytes),
            })
        })
        .collect()
}

fn xmp_stream_ids(document: &Document) -> Vec<ObjectId> {
    document
        .objects
        .iter()
        .filter_map(|(id, obj)| match obj {
            Object::Stream(s) if s.dict.has_type(b"Metadata") => Some(*id),
            _ => None,
        })
        .collect()
}

fn xmp_findings(document: &Document, opts: &PdfOptions) -> Vec<PdfFinding> {
    // Preview only — cap independently of the (potentially much larger)
    // load-time limit so a single huge XMP packet doesn't blow up an
    // inspect report.
    let preview_limit = opts.max_decompressed_stream_bytes.min(1 << 20);
    xmp_stream_ids(document)
        .into_iter()
        .filter_map(|id| {
            let Some(Object::Stream(stream)) = document.objects.get(&id) else {
                return None;
            };
            if stream.content.is_empty() {
                return None;
            }
            Some(PdfFinding {
                location: format!("XMP stream (object {} {})", id.0, id.1),
                field: "xmp".to_string(),
                value: stream_preview(stream, preview_limit),
            })
        })
        .collect()
}

const PREVIEW_MAX_CHARS: usize = 160;

fn stream_preview(stream: &Stream, max_bytes: usize) -> String {
    let bytes = stream
        .decompressed_content_with_limit(max_bytes)
        .unwrap_or_else(|_| stream.content.clone());
    let text = String::from_utf8_lossy(&bytes);
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > PREVIEW_MAX_CHARS {
        let truncated: String = collapsed.chars().take(PREVIEW_MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

/// Best-effort human-readable decode of a PDF string. PDF text strings
/// are either PDFDocEncoding (a superset of Latin-1 for the common
/// range) or, when prefixed with the UTF-16BE byte-order mark, UTF-16BE.
fn pdf_string_lossy(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary as Dict, Stream as PdfStream};

    fn build_test_pdf() -> Vec<u8> {
        let mut document = Document::with_version("1.7");

        let pages_id = document.new_object_id();
        let mut catalog = Dict::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = document.add_object(catalog);

        let mut pages = Dict::new();
        pages.set("Type", Object::Name(b"Pages".to_vec()));
        pages.set("Kids", Object::Array(vec![]));
        pages.set("Count", Object::Integer(0));
        document.set_object(pages_id, pages);

        document.trailer.set("Root", Object::Reference(catalog_id));

        let mut info = Dict::new();
        info.set("Author", Object::string_literal("Jane Doe"));
        info.set("Producer", Object::string_literal("ChatGPT"));
        info.set("Title", Object::string_literal("A Report"));
        let info_id = document.add_object(info);
        document.trailer.set("Info", Object::Reference(info_id));

        let xmp_content = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description pdf:Producer="ChatGPT"/></rdf:RDF></x:xmpmeta>"#.to_vec();
        let mut xmp_dict = Dict::new();
        xmp_dict.set("Type", Object::Name(b"Metadata".to_vec()));
        xmp_dict.set("Subtype", Object::Name(b"XML".to_vec()));
        document.add_object(Object::Stream(PdfStream::new(xmp_dict, xmp_content)));

        let mut out = Vec::new();
        document.save_to(&mut out).expect("build test pdf");
        out
    }

    #[test]
    fn inspects_info_dict_and_xmp_without_modifying() {
        let bytes = build_test_pdf();
        let report = inspect_pdf(&bytes, &PdfOptions::default()).expect("inspect");
        assert!(!report.is_clean());
        assert!(report
            .findings
            .iter()
            .any(|f| f.location == "/Info" && f.field == "Author" && f.value == "Jane Doe"));
        assert!(report
            .findings
            .iter()
            .any(|f| f.location == "/Info" && f.field == "Producer" && f.value == "ChatGPT"));
        assert!(report.findings.iter().any(|f| f.location.starts_with("XMP")));
    }

    #[test]
    fn strips_info_dict_and_xmp_content() {
        let bytes = build_test_pdf();
        let (cleaned, report) = clean_pdf(&bytes, &PdfOptions::default()).expect("clean");
        assert!(report.stripped.iter().any(|s| s.contains("/Info")));
        assert!(report.stripped.iter().any(|s| s.contains("XMP")));

        // Re-parse the cleaned output and confirm nothing identifying survives.
        let reinspected = inspect_pdf(&cleaned, &PdfOptions::default()).expect("reinspect");
        assert!(reinspected.is_clean(), "{:?}", reinspected.findings);

        let document = Document::load_mem(&cleaned).expect("reload cleaned pdf");
        assert!(document.trailer.get(b"Info").is_err());
    }

    #[test]
    fn rejects_oversized_input() {
        let bytes = build_test_pdf();
        let opts = PdfOptions {
            max_input_bytes: 1,
            ..Default::default()
        };
        let err = inspect_pdf(&bytes, &opts).unwrap_err();
        assert!(matches!(err, PdfError::InputTooLarge { .. }));
    }
}
