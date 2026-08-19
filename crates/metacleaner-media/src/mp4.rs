//! Strip identifying metadata from MP4/M4A/MOV files: the iTunes-style
//! `moov/udta/meta/ilst` metadata item list (title, artist, album,
//! comment, encoder/software tag, embedded artwork, and any custom
//! freeform items) and the chapter list/track. Audio/video sample data
//! (`mdat`) and every structural atom besides `udta` are untouched.
//!
//! Uses `mp4ameta` (pure Rust, MIT). Its writer locates the existing
//! `udta` atom (recorded when the file was read) and replaces/removes it
//! in place — the same "in-place, don't touch payload" approach
//! `metacleaner-pdf` uses for `/Info` + XMP, rather than a full
//! decode/re-encode of the media itself (impractical for audio/video).
//!
//! Caveat, deliberately scoped: `mp4ameta` targets the iTunes-style
//! `ilst` dictionary. Some camera- or editor-specific atoms outside that
//! dictionary may not be covered by this first pass — the same
//! documented-gap pattern `metacleaner-pdf`/`metacleaner-docs` use for
//! their own known-incomplete scopes.

use std::io::Cursor;

use mp4ameta::Tag;

use crate::{check_size, preview, MediaError, MediaFinding, MediaOptions};

/// Report what would be stripped, without modifying `input`.
pub fn inspect_mp4(input: &[u8], opts: &MediaOptions) -> Result<Vec<MediaFinding>, MediaError> {
    check_size(input, opts)?;
    let mut cursor = Cursor::new(input.to_vec());
    let tag = Tag::read_from(&mut cursor)?;
    Ok(tag_findings(&tag))
}

/// Strip the `udta/meta/ilst` metadata item list and chapter data from
/// `input`. Every other atom, including `mdat` sample data, is left as
/// `mp4ameta` re-serializes it (in-place replacement, not a full remux).
pub fn clean_mp4(input: &[u8], opts: &MediaOptions) -> Result<(Vec<u8>, Vec<String>), MediaError> {
    check_size(input, opts)?;
    let mut cursor = Cursor::new(input.to_vec());
    let mut tag = Tag::read_from(&mut cursor)?;

    let findings = tag_findings(&tag);
    let mut stripped = Vec::new();
    if !findings.is_empty() {
        tag.clear();
        tag.userdata.write_to(&mut cursor)?;
        stripped.push(format!("{} metadata item(s)", findings.len()));
    }

    Ok((cursor.into_inner(), stripped))
}

fn tag_findings(tag: &Tag) -> Vec<MediaFinding> {
    tag.data()
        .map(|(ident, data)| MediaFinding {
            location: "MP4 metadata".to_string(),
            field: ident.to_string(),
            value: data_preview(data),
        })
        .collect()
}

fn data_preview(data: &mp4ameta::Data) -> String {
    match data {
        mp4ameta::Data::Utf8(s) | mp4ameta::Data::Utf16(s) => preview(s),
        mp4ameta::Data::Jpeg(b) => format!("JPEG image data, {} bytes", b.len()),
        mp4ameta::Data::Png(b) => format!("PNG image data, {} bytes", b.len()),
        mp4ameta::Data::Bmp(b) => format!("BMP image data, {} bytes", b.len()),
        mp4ameta::Data::Reserved(b) => format!("binary data, {} bytes", b.len()),
        mp4ameta::Data::BeSigned(b) => format!("binary data, {} bytes", b.len()),
        mp4ameta::Data::Unknown { data, .. } => format!("binary data, {} bytes", data.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(fourcc: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + content.len());
        v.extend_from_slice(&((8 + content.len()) as u32).to_be_bytes());
        v.extend_from_slice(fourcc);
        v.extend_from_slice(content);
        v
    }

    fn data_atom(payload: &[u8]) -> Vec<u8> {
        let mut content = Vec::new();
        content.push(0u8); // version
        content.extend_from_slice(&[0, 0, 1]); // type code 1 = UTF8, big-endian 24-bit
        content.extend_from_slice(&[0, 0, 0, 0]); // locale
        content.extend_from_slice(payload);
        atom(b"data", &content)
    }

    fn item_atom(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        atom(fourcc, &data_atom(payload))
    }

    /// Builds the smallest MP4 `mp4ameta` will parse: `ftyp` + `moov`
    /// containing just `mvhd` (required) and `udta/meta/ilst` with two
    /// text items. No `trak`/`mdat` — the crate's default `ReadConfig`
    /// doesn't require a track to read/write metadata items.
    fn build_test_mp4() -> Vec<u8> {
        let mut ftyp_content = Vec::new();
        ftyp_content.extend_from_slice(b"isom");
        ftyp_content.extend_from_slice(&[0, 0, 0, 0]);
        ftyp_content.extend_from_slice(b"isom");
        ftyp_content.extend_from_slice(b"mp42");
        let ftyp = atom(b"ftyp", &ftyp_content);

        // mvhd version 0: 4-byte fullbox header + 96-byte body. Only
        // `timescale` (bytes 8..12 of the body: after creation_time and
        // modification_time) needs a real value — mp4ameta divides by it
        // when computing duration, so zero would panic.
        let mut mvhd_content = vec![0u8; 4];
        mvhd_content.extend_from_slice(&[0u8; 96]);
        mvhd_content[4 + 8..4 + 12].copy_from_slice(&1000u32.to_be_bytes());
        let mvhd = atom(b"mvhd", &mvhd_content);

        let title_item = item_atom(b"\xa9nam", b"Test Title");
        let artist_item = item_atom(b"\xa9ART", b"Test Artist");
        let mut ilst_content = Vec::new();
        ilst_content.extend(title_item);
        ilst_content.extend(artist_item);
        let ilst = atom(b"ilst", &ilst_content);

        let mut meta_content = vec![0u8; 4]; // fullbox header
        meta_content.extend(ilst);
        let meta = atom(b"meta", &meta_content);

        let udta = atom(b"udta", &meta);

        let mut moov_content = Vec::new();
        moov_content.extend(mvhd);
        moov_content.extend(udta);
        let moov = atom(b"moov", &moov_content);

        // The writer (unlike the reader) needs an `mdat` atom to anchor
        // its in-place rewrite bookkeeping against, even an empty one.
        let mdat = atom(b"mdat", b"");

        let mut out = Vec::new();
        out.extend(ftyp);
        out.extend(moov);
        out.extend(mdat);
        out
    }

    #[test]
    fn inspects_metadata_items_without_modifying() {
        let bytes = build_test_mp4();
        let findings = inspect_mp4(&bytes, &MediaOptions::default()).expect("inspect");
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().any(|f| f.field == "\u{a9}nam" && f.value == "Test Title"));
        assert!(findings.iter().any(|f| f.field == "\u{a9}ART" && f.value == "Test Artist"));
    }

    #[test]
    fn strips_metadata_items() {
        let bytes = build_test_mp4();
        let (cleaned, stripped) = clean_mp4(&bytes, &MediaOptions::default()).expect("clean");
        assert_eq!(stripped.len(), 1);

        let findings = inspect_mp4(&cleaned, &MediaOptions::default()).expect("reinspect");
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn rejects_oversized_input() {
        let bytes = build_test_mp4();
        let opts = MediaOptions {
            max_input_bytes: 1,
            ..Default::default()
        };
        let err = inspect_mp4(&bytes, &opts).unwrap_err();
        assert!(matches!(err, MediaError::InputTooLarge { .. }));
    }
}
