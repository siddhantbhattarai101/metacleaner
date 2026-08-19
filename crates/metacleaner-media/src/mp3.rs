//! Strip identifying metadata from MP3 files: every ID3v2 frame (artist,
//! album, comment, encoder/software tag `TSSE`, embedded artwork, any
//! custom `TXXX`/`PRIV` frame) and the legacy 128/227-byte ID3v1 trailer,
//! if present. Audio frame data is untouched.
//!
//! Uses the `id3` crate (read/write both ID3v1 and ID3v2). Both formats
//! support in-place replacement on an in-memory `Cursor<Vec<u8>>` (via
//! their `StorageFile` trait) the same way `metacleaner-pdf` edits a
//! PDF's object graph without touching page content — `id3` locates the
//! existing ID3v2 header and replaces just that byte range, and does the
//! same for the ID3v1 trailer at end-of-file.
//!
//! License note: `id3` is MPL-2.0, unlike the rest of this workspace's
//! MIT/Apache-2.0 dependencies — see `Cargo.toml`.

use std::io::Cursor;

use id3::{Tag as Id3v2Tag, Version};

use crate::{check_size, preview, MediaError, MediaFinding, MediaOptions};

/// Report what would be stripped, without modifying `input`.
pub fn inspect_mp3(input: &[u8], opts: &MediaOptions) -> Result<Vec<MediaFinding>, MediaError> {
    check_size(input, opts)?;
    let mut cursor = Cursor::new(input.to_vec());

    let mut findings = v2_findings(&mut cursor)?;
    cursor.set_position(0);
    findings.extend(v1_findings(&mut cursor)?);

    Ok(findings)
}

/// Strip both the ID3v2 header and ID3v1 trailer from `input`. Audio
/// frame data in between is untouched.
pub fn clean_mp3(input: &[u8], opts: &MediaOptions) -> Result<(Vec<u8>, Vec<String>), MediaError> {
    check_size(input, opts)?;
    let mut cursor = Cursor::new(input.to_vec());

    let v2 = v2_findings(&mut cursor)?;
    cursor.set_position(0);
    let v1 = v1_findings(&mut cursor)?;
    cursor.set_position(0);

    let mut stripped = Vec::new();
    if !v2.is_empty() {
        Id3v2Tag::new().write_to_file(&mut cursor, Version::Id3v24)?;
        stripped.push(format!("ID3v2 ({} frame(s))", v2.len()));
    }
    if !v1.is_empty() {
        id3::v1::Tag::remove_from_file(&mut cursor)?;
        stripped.push("ID3v1".to_string());
    }

    Ok((cursor.into_inner(), stripped))
}

fn v2_findings(cursor: &mut Cursor<Vec<u8>>) -> Result<Vec<MediaFinding>, MediaError> {
    let tag = match id3::no_tag_ok(Id3v2Tag::read_from2(&mut *cursor))? {
        Some(tag) => tag,
        None => return Ok(Vec::new()),
    };
    Ok(tag
        .frames()
        .map(|frame| MediaFinding {
            location: "ID3v2".to_string(),
            field: frame.id().to_string(),
            value: preview(&frame.content().to_string()),
        })
        .collect())
}

fn v1_findings(cursor: &mut Cursor<Vec<u8>>) -> Result<Vec<MediaFinding>, MediaError> {
    // ID3v1 lives in the last 128 (or 355, extended) bytes of the file;
    // a shorter file can't contain one, and checking anyway would try to
    // seek before the start of the stream.
    if cursor.get_ref().len() < 128 {
        return Ok(Vec::new());
    }
    if !id3::v1::Tag::is_candidate(&mut *cursor)? {
        return Ok(Vec::new());
    }
    let tag = id3::v1::Tag::read_from(&mut *cursor)?;

    let mut findings = Vec::new();
    let mut push = |field: &str, value: &str| {
        if !value.is_empty() {
            findings.push(MediaFinding {
                location: "ID3v1".to_string(),
                field: field.to_string(),
                value: preview(value),
            });
        }
    };
    push("title", &tag.title);
    push("artist", &tag.artist);
    push("album", &tag.album);
    push("year", &tag.year);
    push("comment", &tag.comment);
    if let Some(genre) = &tag.genre_str {
        push("genre", genre);
    }

    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use id3::TagLike;

    /// Builds a minimal in-memory "file" containing only an ID3v2 tag
    /// (no audio frames) by writing a fresh tag into an empty cursor —
    /// `id3` treats a cursor with no recognized header as "no existing
    /// tag" and prepends one at position 0, which is exactly what we
    /// need for a round-trip test of the tag-stripping logic itself.
    fn build_test_mp3() -> Vec<u8> {
        let mut tag = Id3v2Tag::new();
        tag.set_artist("Jane Doe");
        tag.set_album("Test Album");
        tag.add_frame(id3::Frame::text("TSSE", "ChatGPT Audio Tool"));

        let mut cursor = Cursor::new(Vec::new());
        tag.write_to_file(&mut cursor, Version::Id3v24)
            .expect("build test mp3");
        cursor.into_inner()
    }

    #[test]
    fn inspects_id3v2_frames_without_modifying() {
        let bytes = build_test_mp3();
        let findings = inspect_mp3(&bytes, &MediaOptions::default()).expect("inspect");
        assert!(findings.iter().any(|f| f.field == "TPE1" && f.value == "Jane Doe"));
        assert!(findings.iter().any(|f| f.field == "TALB" && f.value == "Test Album"));
        assert!(findings
            .iter()
            .any(|f| f.field == "TSSE" && f.value == "ChatGPT Audio Tool"));
    }

    #[test]
    fn strips_id3v2_frames() {
        let bytes = build_test_mp3();
        let (cleaned, stripped) = clean_mp3(&bytes, &MediaOptions::default()).expect("clean");
        assert!(stripped.iter().any(|s| s.contains("ID3v2")));

        let findings = inspect_mp3(&cleaned, &MediaOptions::default()).expect("reinspect");
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn clean_is_a_no_op_on_untagged_audio() {
        // No ID3 tag at all: neither pass should error or report findings.
        let bytes = vec![0xFFu8, 0xFB, 0x90, 0x00]; // plausible-looking MPEG frame sync bytes
        let findings = inspect_mp3(&bytes, &MediaOptions::default()).expect("inspect");
        assert!(findings.is_empty());
        let (cleaned, stripped) = clean_mp3(&bytes, &MediaOptions::default()).expect("clean");
        assert!(stripped.is_empty());
        assert_eq!(cleaned, bytes);
    }

    #[test]
    fn rejects_oversized_input() {
        let bytes = build_test_mp3();
        let opts = MediaOptions {
            max_input_bytes: 1,
            ..Default::default()
        };
        let err = inspect_mp3(&bytes, &opts).unwrap_err();
        assert!(matches!(err, MediaError::InputTooLarge { .. }));
    }
}
