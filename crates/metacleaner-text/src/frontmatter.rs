//! Strip AI/tool-identifying keys from Markdown YAML frontmatter (the
//! `---`-delimited block at the top of a `.md` file).
//!
//! Unlike the invisible-Unicode stripping in the rest of this crate — which
//! removes unconditionally, because there's no legitimate reason for those
//! codepoints to be in prose — frontmatter is user-authored content
//! metadata a blog/site generator often depends on (`title`, `date`,
//! `tags`, `slug`, `draft`...). Wholesale-stripping the block the way
//! `metacleaner-core` re-encodes an image would silently break real,
//! wanted functionality. So this targets a curated, denylist of keys that
//! are specifically identity/tool/AI-provenance related, leaving
//! everything else in the block untouched.
//!
//! Deliberately a small hand-rolled scanner, not a full YAML parser: only
//! simple top-level `key: value` lines are recognized (matching how
//! frontmatter keys are used in practice); nested maps/lists are left
//! alone rather than risk misparsing them.

use crate::identifying_keys::is_identifying_key;

#[derive(Debug, Clone)]
pub struct FrontmatterFinding {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct FrontmatterReport {
    pub had_frontmatter: bool,
    pub removed: Vec<FrontmatterFinding>,
}

impl FrontmatterReport {
    pub fn is_clean(&self) -> bool {
        self.removed.is_empty()
    }
}

/// Report which identifying frontmatter keys are present, without
/// modifying `input`.
pub fn inspect_frontmatter(input: &str) -> FrontmatterReport {
    let Some((_, block_lines, _, _)) = split_frontmatter(input) else {
        return FrontmatterReport {
            had_frontmatter: false,
            removed: Vec::new(),
        };
    };

    let removed = block_lines
        .iter()
        .filter_map(|line| parse_key_value(line))
        .filter(|(key, _)| is_identifying_key(key))
        .map(|(key, value)| FrontmatterFinding {
            key: key.to_string(),
            value: value.to_string(),
        })
        .collect();

    FrontmatterReport {
        had_frontmatter: true,
        removed,
    }
}

/// Remove lines for identifying keys from `input`'s frontmatter block, if
/// present. Every other line — other frontmatter keys, and the entire
/// document body — is returned byte-for-byte unchanged.
pub fn strip_frontmatter(input: &str) -> (String, FrontmatterReport) {
    let Some((open_line, block_lines, close_line, rest)) = split_frontmatter(input) else {
        return (
            input.to_string(),
            FrontmatterReport {
                had_frontmatter: false,
                removed: Vec::new(),
            },
        );
    };

    let mut removed = Vec::new();
    let mut kept_lines = Vec::with_capacity(block_lines.len());
    for line in &block_lines {
        match parse_key_value(line) {
            Some((key, value)) if is_identifying_key(key) => {
                removed.push(FrontmatterFinding {
                    key: key.to_string(),
                    value: value.to_string(),
                });
            }
            _ => kept_lines.push(*line),
        }
    }

    let mut out = String::with_capacity(input.len());
    out.push_str(open_line);
    out.push('\n');
    for line in &kept_lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(close_line);
    out.push_str(rest);

    (
        out,
        FrontmatterReport {
            had_frontmatter: true,
            removed,
        },
    )
}

/// A simple top-level `key: value` line: no leading whitespace (top-level,
/// not nested), a `:` separator, no leading `-` (not a list item).
fn parse_key_value(line: &str) -> Option<(&str, &str)> {
    if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('-') {
        return None;
    }
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None; // not a bare identifier — likely not a real key
    }
    Some((key, value.trim()))
}

/// Split `input` into (opening `---` line, frontmatter body lines, closing
/// `---` line, everything after including its own leading newline), or
/// `None` if `input` doesn't start with a frontmatter block.
fn split_frontmatter(input: &str) -> Option<(&str, Vec<&str>, &str, &str)> {
    let first_line_end = input.find('\n').unwrap_or(input.len());
    let first_line = &input[..first_line_end];
    if first_line.trim_end_matches('\r') != "---" {
        return None;
    }
    let after_open = &input[first_line_end.min(input.len())..];
    let after_open = after_open.strip_prefix('\n')?;

    // Find the closing "---" line.
    let mut offset = 0usize;
    let mut body_lines = Vec::new();
    for line in after_open.split('\n') {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "---" || trimmed == "..." {
            let close_line = trimmed;
            let close_end = offset + line.len();
            let rest = &after_open[close_end.min(after_open.len())..];
            return Some((first_line, body_lines, close_line, rest));
        }
        body_lines.push(trimmed);
        offset += line.len() + 1; // +1 for the '\n' the split consumed
    }

    None // no closing delimiter found — not well-formed frontmatter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_identifying_keys_keeps_content_keys() {
        let input = "---\ntitle: My Post\nauthor: Jane Doe\ndate: 2026-01-01\ngenerator: ChatGPT\ntags: [rust, cli]\n---\n\nHello, **world**.\n";
        let (cleaned, report) = strip_frontmatter(input);
        assert!(cleaned.contains("title: My Post"));
        assert!(cleaned.contains("date: 2026-01-01"));
        assert!(cleaned.contains("tags: [rust, cli]"));
        assert!(!cleaned.contains("author:"));
        assert!(!cleaned.contains("generator:"));
        assert!(cleaned.ends_with("Hello, **world**.\n"));
        assert_eq!(report.removed.len(), 2);
    }

    #[test]
    fn matches_keys_regardless_of_dash_underscore_case() {
        let input = "---\nLast-Modified-By: Bob\n---\nbody\n";
        let (cleaned, report) = strip_frontmatter(input);
        assert!(!cleaned.contains("Last-Modified-By"));
        assert_eq!(report.removed.len(), 1);
    }

    #[test]
    fn leaves_files_without_frontmatter_untouched() {
        let input = "# Just a heading\n\nauthor: not frontmatter, just text\n";
        let (cleaned, report) = strip_frontmatter(input);
        assert_eq!(cleaned, input);
        assert!(!report.had_frontmatter);
        assert!(report.is_clean());
    }

    #[test]
    fn leaves_nested_and_list_frontmatter_values_alone() {
        let input = "---\nauthors:\n  - Jane Doe\n  - John Smith\ntitle: Report\n---\nbody\n";
        let (cleaned, report) = strip_frontmatter(input);
        // "authors:" itself (a bare key with no inline value) is still
        // recognized and removed, but its nested list items are untouched
        // structurally — they just have no parent key left, which is a
        // known, documented limitation of the simple-scanner approach.
        assert!(cleaned.contains("title: Report"));
        assert!(!report.is_clean());
    }

    #[test]
    fn inspect_does_not_modify_input() {
        let input = "---\nauthor: Jane\ntitle: T\n---\nbody\n";
        let report = inspect_frontmatter(input);
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].key, "author");
    }

    #[test]
    fn handles_missing_closing_delimiter_gracefully() {
        let input = "---\ntitle: Unterminated\nno closing delimiter here\n";
        let (cleaned, report) = strip_frontmatter(input);
        assert_eq!(cleaned, input);
        assert!(!report.had_frontmatter);
    }
}
