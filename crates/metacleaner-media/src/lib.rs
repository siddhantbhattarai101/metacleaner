//! Strip identifying metadata from MP3 (ID3v1/ID3v2) and MP4/M4A/MOV
//! (iTunes-style `ilst` atom) audio/video files. See the `mp3`/`mp4`
//! module docs for format-specific detail and scope notes.

mod mp3;
pub use mp3::{clean_mp3, inspect_mp3};

mod mp4;
pub use mp4::{clean_mp4, inspect_mp4};

pub const DEFAULT_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct MediaOptions {
    pub max_input_bytes: u64,
}

impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("input is {size} bytes, which exceeds the {max}-byte limit")]
    InputTooLarge { size: usize, max: u64 },
    #[error("invalid or unsupported MP3/ID3 data: {0}")]
    Id3(#[from] id3::Error),
    #[error("invalid or unsupported MP4 data: {0}")]
    Mp4(#[from] mp4ameta::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// One identifying value found in an MP3/MP4 tag.
#[derive(Debug, Clone)]
pub struct MediaFinding {
    /// `"ID3v2"`, `"ID3v1"`, or `"MP4 metadata"`.
    pub location: String,
    pub field: String,
    pub value: String,
}

fn check_size(input: &[u8], opts: &MediaOptions) -> Result<(), MediaError> {
    if input.len() as u64 > opts.max_input_bytes {
        return Err(MediaError::InputTooLarge {
            size: input.len(),
            max: opts.max_input_bytes,
        });
    }
    Ok(())
}

const PREVIEW_MAX_CHARS: usize = 160;

/// Collapse whitespace and truncate a finding value for display, the
/// same preview convention `metacleaner-pdf`/`metacleaner-text` use.
fn preview(s: &str) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > PREVIEW_MAX_CHARS {
        let truncated: String = collapsed.chars().take(PREVIEW_MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}
