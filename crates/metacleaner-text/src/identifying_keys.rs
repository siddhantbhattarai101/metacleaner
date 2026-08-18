//! Shared denylist of metadata keys that identify an author, tool, or AI
//! system rather than describing content itself. Used by both the
//! Markdown-frontmatter cleaner and the HTML `<meta>` cleaner, since a
//! frontmatter `generator: ChatGPT` key and a
//! `<meta name="generator" content="ChatGPT">` tag are the same kind of
//! attribution metadata, just expressed in two different container formats.

pub(crate) const IDENTIFYING_KEYS: &[&str] = &[
    "author",
    "authors",
    "creator",
    "editor",
    "contributor",
    "lastmodifiedby",
    "generator",
    "poweredby",
    "tool",
    "model",
    "aimodel",
    "ai",
    "aigenerated",
    "assistant",
    "prompt",
    "sourcemodel",
    "og:generator",
];

/// Case-insensitive match against [`IDENTIFYING_KEYS`], with `-`/`_` treated
/// as equivalent (so `last-modified-by` and `last_modified_by` both match).
pub(crate) fn is_identifying_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|c| *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect();
    IDENTIFYING_KEYS.iter().any(|k| {
        let k_normalized: String = k.chars().filter(|c| *c != '-' && *c != '_').collect();
        k_normalized == normalized
    })
}
