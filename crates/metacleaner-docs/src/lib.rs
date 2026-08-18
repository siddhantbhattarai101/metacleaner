//! Strip identifying metadata from OOXML office documents (DOCX, XLSX,
//! PPTX) — all three are a ZIP archive of XML parts, and all three carry
//! the same `docProps/core.xml` (author, last-modified-by, title,
//! keywords...), `docProps/app.xml` (company, manager, template...), and
//! optional `docProps/custom.xml` (arbitrary custom properties — a common
//! place for tracking IDs or tool fingerprints) parts, so one
//! implementation covers all three formats.
//!
//! Approach mirrors `metacleaner-core`'s image philosophy: rather than
//! selectively targeting specific known-sensitive fields, every text
//! value inside these three property parts is blanked unconditionally.
//! That's deliberately blunt — it also empties fields like word/page
//! counts that aren't really privacy-sensitive — but it means nothing
//! sensitive is missed just because we didn't think to name it, matching
//! how `clean()` handles images by re-encoding rather than field-picking.
//!
//! Scope note: this cleans the three metadata parts, not the document
//! body. DOCX specifically can also carry author names in tracked-changes
//! revisions (`w:ins`/`w:del`) and comments even when "track changes"
//! isn't visibly showing — that's real, known, and out of scope for this
//! first pass; flagged here rather than silently ignored.
//!
//! Zip-bomb guarded throughout: input size, entry count, and both
//! per-entry and cumulative decompressed size are capped before/while
//! reading, the same category of defense `metacleaner-core` applies to
//! image decoding.

use std::io::{Cursor, Read};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// The three OOXML parts that carry document-level metadata, present
/// identically across DOCX/XLSX/PPTX.
const METADATA_PARTS: &[&str] = &[
    "docProps/core.xml",
    "docProps/app.xml",
    "docProps/custom.xml",
];

pub const DEFAULT_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;
pub const DEFAULT_MAX_ENTRY_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct OoxmlOptions {
    pub max_input_bytes: u64,
    pub max_entries: usize,
    pub max_entry_uncompressed_bytes: u64,
    pub max_total_uncompressed_bytes: u64,
}

impl Default for OoxmlOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_entry_uncompressed_bytes: DEFAULT_MAX_ENTRY_UNCOMPRESSED_BYTES,
            max_total_uncompressed_bytes: DEFAULT_MAX_TOTAL_UNCOMPRESSED_BYTES,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DocsError {
    #[error("input is {size} bytes, which exceeds the {max}-byte limit")]
    InputTooLarge { size: usize, max: u64 },
    #[error("not a recognized OOXML (DOCX/XLSX/PPTX) file: missing [Content_Types].xml")]
    NotOoxml,
    #[error("archive has {count} entries, which exceeds the {max}-entry limit")]
    TooManyEntries { count: usize, max: usize },
    #[error("entry \"{name}\" is {size} bytes uncompressed, which exceeds the {max}-byte per-entry limit")]
    EntryTooLarge { name: String, size: u64, max: u64 },
    #[error("archive decompresses to more than the {max}-byte cumulative limit")]
    TotalTooLarge { max: u64 },
    #[error("invalid zip archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("XML error in \"{part}\": {source}")]
    Xml {
        part: String,
        #[source]
        source: quick_xml::Error,
    },
}

#[derive(Debug, Clone)]
pub struct OoxmlCleanReport {
    pub bytes_in: usize,
    pub bytes_out: usize,
    /// Metadata parts that were present and had their text content blanked.
    pub stripped_parts: Vec<String>,
}

/// One non-empty metadata value found in a document-properties part.
#[derive(Debug, Clone)]
pub struct OoxmlFinding {
    pub part: String,
    pub field: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct InspectOoxmlReport {
    pub findings: Vec<OoxmlFinding>,
}

impl InspectOoxmlReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Is `input` a ZIP archive that looks like an OOXML document (DOCX/XLSX/
/// PPTX)? Just checks for the `[Content_Types].xml` part every OOXML
/// package has — doesn't distinguish which of the three, since metadata
/// cleaning is identical across all three.
pub fn is_ooxml(input: &[u8]) -> bool {
    let Ok(mut archive) = ZipArchive::new(Cursor::new(input)) else {
        return false;
    };
    let found = archive.by_name("[Content_Types].xml").is_ok();
    found
}

/// Report the non-empty values present in `docProps/core.xml`,
/// `docProps/app.xml`, and `docProps/custom.xml`, without modifying the
/// input.
pub fn inspect_ooxml(input: &[u8], opts: &OoxmlOptions) -> Result<InspectOoxmlReport, DocsError> {
    check_input_size(input, opts)?;
    let mut archive = open_guarded(input, opts)?;
    if archive.by_name("[Content_Types].xml").is_err() {
        return Err(DocsError::NotOoxml);
    }

    let mut findings = Vec::new();
    for &part in METADATA_PARTS {
        let Some(xml) = read_entry_if_present(&mut archive, part, opts)? else {
            continue;
        };
        for (field, value) in extract_leaf_values(&xml) {
            findings.push(OoxmlFinding {
                part: part.to_string(),
                field,
                value,
            });
        }
    }

    Ok(InspectOoxmlReport { findings })
}

/// Blank every text value in `docProps/core.xml`, `docProps/app.xml`, and
/// `docProps/custom.xml` (whichever are present), and return the
/// repackaged document. Every other part is copied through unchanged
/// (compressed bytes copied directly, not re-compressed).
pub fn clean_ooxml(
    input: &[u8],
    opts: &OoxmlOptions,
) -> Result<(Vec<u8>, OoxmlCleanReport), DocsError> {
    check_input_size(input, opts)?;
    let mut archive = open_guarded(input, opts)?;
    if archive.by_name("[Content_Types].xml").is_err() {
        return Err(DocsError::NotOoxml);
    }

    let mut out_buf = Vec::new();
    let mut writer = ZipWriter::new(Cursor::new(&mut out_buf));
    let mut stripped_parts = Vec::new();
    let mut total_uncompressed = 0u64;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();

        if METADATA_PARTS.contains(&name.as_str()) {
            let bytes = read_bounded(&mut entry, opts, &mut total_uncompressed)?;
            let cleaned = blank_text_nodes(&bytes).map_err(|source| DocsError::Xml {
                part: name.clone(),
                source,
            })?;
            writer.start_file(&name, SimpleFileOptions::default())?;
            std::io::Write::write_all(&mut writer, &cleaned)?;
            stripped_parts.push(name);
        } else {
            writer.raw_copy_file(entry)?;
        }
    }

    let cursor = writer.finish()?;
    let bytes_out = cursor.into_inner().len();

    Ok((
        out_buf,
        OoxmlCleanReport {
            bytes_in: input.len(),
            bytes_out,
            stripped_parts,
        },
    ))
}

fn check_input_size(input: &[u8], opts: &OoxmlOptions) -> Result<(), DocsError> {
    if input.len() as u64 > opts.max_input_bytes {
        return Err(DocsError::InputTooLarge {
            size: input.len(),
            max: opts.max_input_bytes,
        });
    }
    Ok(())
}

fn open_guarded<'a>(
    input: &'a [u8],
    opts: &OoxmlOptions,
) -> Result<ZipArchive<Cursor<&'a [u8]>>, DocsError> {
    let archive = ZipArchive::new(Cursor::new(input))?;
    if archive.len() > opts.max_entries {
        return Err(DocsError::TooManyEntries {
            count: archive.len(),
            max: opts.max_entries,
        });
    }
    Ok(archive)
}

fn read_entry_if_present<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    opts: &OoxmlOptions,
) -> Result<Option<Vec<u8>>, DocsError> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            let mut total = 0u64;
            Ok(Some(read_bounded(&mut entry, opts, &mut total)?))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Read a zip entry's decompressed content, enforcing per-entry and
/// cumulative caps against the *actual* bytes produced — not just the
/// (forgeable) declared size in the zip header, which is exactly what a
/// zip-bomb lies about.
fn read_bounded<R: Read>(
    entry: &mut R,
    opts: &OoxmlOptions,
    total_uncompressed: &mut u64,
) -> Result<Vec<u8>, DocsError> {
    let cap = opts.max_entry_uncompressed_bytes;
    let mut limited = entry.take(cap.saturating_add(1));
    let mut buf = Vec::new();
    limited.read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        return Err(DocsError::EntryTooLarge {
            name: String::new(),
            size: buf.len() as u64,
            max: cap,
        });
    }

    *total_uncompressed = total_uncompressed.saturating_add(buf.len() as u64);
    if *total_uncompressed > opts.max_total_uncompressed_bytes {
        return Err(DocsError::TotalTooLarge {
            max: opts.max_total_uncompressed_bytes,
        });
    }

    Ok(buf)
}

/// Rewrite `xml`, replacing every text node's content with an empty
/// string. Structure (elements, attributes, namespaces) is preserved so
/// the part remains schema-valid; only values are removed.
fn blank_text_nodes(xml: &[u8]) -> Result<Vec<u8>, quick_xml::Error> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut writer = quick_xml::Writer::new(Vec::new());
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Text(_) | Event::CData(_) => { /* drop all text content */ }
            event => {
                writer.write_event(event)?;
            }
        }
        buf.clear();
    }

    Ok(writer.into_inner())
}

/// Walk `xml`, collecting (field name, value) for every non-empty text
/// node. For `<property name="X"><vt:TYPE>value</vt:TYPE></property>`
/// (custom.xml's shape), the field name is the `name` attribute on the
/// enclosing `<property>`; otherwise it's the immediate parent element's
/// local name (core.xml/app.xml's shape: `<dc:creator>value</dc:creator>`
/// -> field "creator").
fn extract_leaf_values(xml: &[u8]) -> Vec<(String, String)> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut stack: Vec<(String, Option<String>)> = Vec::new();
    let mut out = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = local_name(&e);
                let name_attr = if local.eq_ignore_ascii_case("property") {
                    property_name_attr(&e, &reader)
                } else {
                    None
                };
                stack.push((local, name_attr));
            }
            Ok(Event::Empty(_)) => { /* self-closing: no text possible */ }
            Ok(Event::Text(t)) => {
                if let Ok(text) = t.unescape() {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let field = stack
                            .iter()
                            .rev()
                            .find_map(|(_, attr)| attr.clone())
                            .unwrap_or_else(|| {
                                stack.last().map(|(n, _)| n.clone()).unwrap_or_default()
                            });
                        out.push((field, trimmed.to_string()));
                    }
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    out
}

fn local_name(e: &BytesStart) -> String {
    let full = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    full.rsplit(':').next().unwrap_or(&full).to_string()
}

fn property_name_attr(e: &BytesStart, reader: &Reader<&[u8]>) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        if a.key.as_ref() == b"name" {
            a.decode_and_unescape_value(reader.decoder())
                .ok()
                .map(|c| c.into_owned())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal, real OOXML zip with actual identifying metadata in
    /// all three property parts, matching what Word/Excel/PowerPoint
    /// actually produce.
    fn make_ooxml_fixture() -> Vec<u8> {
        let core_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/">
  <dc:creator>Jane Doe</dc:creator>
  <cp:lastModifiedBy>John Smith</cp:lastModifiedBy>
  <dc:title>Quarterly Report</dc:title>
  <cp:revision>4</cp:revision>
  <dcterms:created xsi:type="dcterms:W3CDTF" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">2026-01-01T00:00:00Z</dcterms:created>
</cp:coreProperties>"#;

        let app_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
  <Application>Microsoft Office Word</Application>
  <Company>Acme Corp</Company>
  <Manager>Alice Manager</Manager>
</Properties>"#;

        let custom_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">
  <property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="TrackingID"><vt:lpwstr>internal-doc-4471</vt:lpwstr></property>
</Properties>"#;

        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>Hello, world.</w:t></w:r></w:p></w:body>
</w:document>"#;

        let mut buf = Vec::new();
        {
            let mut writer = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default();
            writer.start_file("[Content_Types].xml", opts).unwrap();
            std::io::Write::write_all(&mut writer, content_types).unwrap();
            writer.start_file("docProps/core.xml", opts).unwrap();
            std::io::Write::write_all(&mut writer, core_xml).unwrap();
            writer.start_file("docProps/app.xml", opts).unwrap();
            std::io::Write::write_all(&mut writer, app_xml).unwrap();
            writer.start_file("docProps/custom.xml", opts).unwrap();
            std::io::Write::write_all(&mut writer, custom_xml).unwrap();
            writer.start_file("word/document.xml", opts).unwrap();
            std::io::Write::write_all(&mut writer, document_xml).unwrap();
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn detects_ooxml_by_content_types_part() {
        let input = make_ooxml_fixture();
        assert!(is_ooxml(&input));
        assert!(!is_ooxml(b"not a zip at all"));
    }

    #[test]
    fn inspect_finds_identifying_metadata_in_all_three_parts() {
        let input = make_ooxml_fixture();
        let report = inspect_ooxml(&input, &OoxmlOptions::default()).unwrap();
        assert!(!report.is_clean());

        let has = |part: &str, field: &str, value: &str| {
            report
                .findings
                .iter()
                .any(|f| f.part == part && f.field == field && f.value == value)
        };
        assert!(has("docProps/core.xml", "creator", "Jane Doe"));
        assert!(has("docProps/core.xml", "lastModifiedBy", "John Smith"));
        assert!(has("docProps/app.xml", "Company", "Acme Corp"));
        assert!(has("docProps/app.xml", "Manager", "Alice Manager"));
        assert!(has(
            "docProps/custom.xml",
            "TrackingID",
            "internal-doc-4471"
        ));
    }

    #[test]
    fn clean_blanks_all_three_parts_and_preserves_document_body() {
        let input = make_ooxml_fixture();
        let (cleaned, report) = clean_ooxml(&input, &OoxmlOptions::default()).unwrap();

        assert_eq!(report.stripped_parts.len(), 3);

        let after = inspect_ooxml(&cleaned, &OoxmlOptions::default()).unwrap();
        assert!(
            after.is_clean(),
            "expected no findings after cleaning, got {:?}",
            after.findings
        );

        // The actual document content must survive untouched.
        let mut archive = ZipArchive::new(Cursor::new(cleaned.as_slice())).unwrap();
        let mut doc = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut doc)
            .unwrap();
        assert!(doc.contains("Hello, world."));
    }

    #[test]
    fn rejects_non_ooxml_zip() {
        let mut buf = Vec::new();
        {
            let mut writer = ZipWriter::new(Cursor::new(&mut buf));
            writer
                .start_file("readme.txt", SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, b"just a plain zip").unwrap();
            writer.finish().unwrap();
        }
        assert!(!is_ooxml(&buf));
        assert!(matches!(
            inspect_ooxml(&buf, &OoxmlOptions::default()),
            Err(DocsError::NotOoxml)
        ));
    }

    #[test]
    fn rejects_input_over_the_configured_byte_limit() {
        let input = make_ooxml_fixture();
        let opts = OoxmlOptions {
            max_input_bytes: 10,
            ..Default::default()
        };
        assert!(matches!(
            inspect_ooxml(&input, &opts),
            Err(DocsError::InputTooLarge { max: 10, .. })
        ));
    }
}
