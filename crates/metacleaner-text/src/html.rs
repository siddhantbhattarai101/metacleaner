//! Strip AI/tool-identifying `<meta>` tags and HTML comments from `.html`
//! files.
//!
//! Same philosophy as [`crate::frontmatter`]: a denylist for `<meta>` tags
//! (reusing [`crate::identifying_keys`], since `<meta name="generator"
//! content="...">` and a frontmatter `generator:` key are the same kind of
//! attribution metadata), but a blanket strip for comments — unlike
//! frontmatter keys, HTML comments are never load-bearing content, they're
//! author/tool asides, so there's no legitimate case being protected by
//! keeping them. The one exception is IE conditional comments
//! (`<!--[if IE]>...<![endif]-->`), which are actual browser-conditional
//! markup, not annotations — those are left alone.
//!
//! Hand-rolled scanner, not a full HTML parser: tag/comment boundaries are
//! found by literal-marker matching with quote-awareness, not a DOM walk.
//! That's enough to correctly find `<meta ...>` tags and `<!-- ... -->`
//! comments without needing an HTML5-parsing dependency, and it can't
//! mis-handle content it doesn't recognize because everything not matched
//! as a comment or a `<meta>` tag is copied through byte-for-byte.

use crate::identifying_keys::is_identifying_key;

const COMMENT_PREVIEW_MAX: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlFindingKind {
    Meta,
    Comment,
}

impl HtmlFindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            HtmlFindingKind::Meta => "meta",
            HtmlFindingKind::Comment => "comment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HtmlFinding {
    pub kind: HtmlFindingKind,
    /// Meta tag name/property (e.g. "generator"), or "comment".
    pub label: String,
    /// Meta tag content, or a truncated preview of the comment text.
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct HtmlReport {
    pub findings: Vec<HtmlFinding>,
}

impl HtmlReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Report what would be stripped, without modifying `input`.
pub fn inspect_html(input: &str) -> HtmlReport {
    strip_html(input).1
}

/// Remove identifying `<meta>` tags and non-conditional HTML comments from
/// `input`. Everything else — including IE conditional comments and every
/// other tag — is returned byte-for-byte unchanged.
pub fn strip_html(input: &str) -> (String, HtmlReport) {
    let mut out = String::with_capacity(input.len());
    let mut findings = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if input[i..].starts_with("<!--") {
            match input[i + 4..].find("-->") {
                Some(rel_end) => {
                    let content_end = i + 4 + rel_end;
                    let content = &input[i + 4..content_end];
                    let tag_end = content_end + 3; // past "-->"
                    if is_conditional_comment(content) {
                        out.push_str(&input[i..tag_end]);
                    } else {
                        findings.push(HtmlFinding {
                            kind: HtmlFindingKind::Comment,
                            label: "comment".to_string(),
                            value: preview(content),
                        });
                    }
                    i = tag_end;
                    continue;
                }
                None => {
                    // Unterminated comment: keep the rest verbatim rather
                    // than guess, and stop scanning for more tags.
                    out.push_str(&input[i..]);
                    break;
                }
            }
        }

        if starts_with_tag_name(&input[i..], "meta") {
            match find_tag_end(&input[i..]) {
                Some(rel_end) => {
                    let tag_end = i + rel_end + 1; // include the '>'
                    let tag_text = &input[i..tag_end];
                    let attrs = parse_attrs(tag_text);
                    let name = attrs
                        .iter()
                        .find(|(k, _)| k == "name" || k == "property")
                        .map(|(_, v)| v.as_str());

                    match name.filter(|n| is_identifying_key(n)) {
                        Some(n) => {
                            let content = attrs
                                .iter()
                                .find(|(k, _)| k == "content")
                                .map(|(_, v)| v.clone())
                                .unwrap_or_default();
                            findings.push(HtmlFinding {
                                kind: HtmlFindingKind::Meta,
                                label: n.to_lowercase(),
                                value: content,
                            });
                        }
                        None => out.push_str(tag_text),
                    }
                    i = tag_end;
                    continue;
                }
                None => {
                    // Unterminated tag: keep the rest verbatim.
                    out.push_str(&input[i..]);
                    break;
                }
            }
        }

        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    (out, HtmlReport { findings })
}

fn is_conditional_comment(comment_content: &str) -> bool {
    comment_content
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("[if")
}

fn preview(comment_content: &str) -> String {
    let collapsed: String = comment_content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() > COMMENT_PREVIEW_MAX {
        let truncated: String = collapsed.chars().take(COMMENT_PREVIEW_MAX).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

/// True if `s` starts with `<` + `name` (case-insensitively) followed by a
/// tag-boundary character (whitespace, `/`, or `>`) — so `<meta` matches
/// but a hypothetical `<metadata>` tag would not.
fn starts_with_tag_name(s: &str, name: &str) -> bool {
    if !s.starts_with('<') {
        return false;
    }
    let rest = &s[1..];
    if rest.len() < name.len() || !rest[..name.len()].eq_ignore_ascii_case(name) {
        return false;
    }
    match rest[name.len()..].chars().next() {
        Some(c) => c.is_whitespace() || c == '/' || c == '>',
        None => false,
    }
}

/// Find the index (relative to `s`, which must start with `<`) of the `>`
/// that closes the tag `s` starts with, respecting quoted attribute values
/// so a `>` inside `content="a > b"` doesn't end the tag early.
fn find_tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (idx, ch) in s.char_indices().skip(1) {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '>' => return Some(idx),
            None => {}
        }
    }
    None
}

/// Parse `name=value` attribute pairs out of a full tag string (e.g.
/// `<meta name="generator" content="v0">`). Attribute names are
/// lowercased; values keep their original casing. Quoted (single or
/// double) and unquoted values are both handled.
fn parse_attrs(tag_text: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let chars: Vec<char> = tag_text.chars().collect();
    let mut i = 1; // skip '<'
                   // skip the tag name itself
    while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '/' && chars[i] != '>' {
        i += 1;
    }

    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == '/') {
            i += 1;
        }
        if i >= chars.len() || chars[i] == '>' {
            break;
        }
        let name_start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() && chars[i] != '>' {
            i += 1;
        }
        let name: String = chars[name_start..i]
            .iter()
            .collect::<String>()
            .to_lowercase();
        if name.is_empty() {
            i += 1;
            continue;
        }

        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }

        let value = if i < chars.len() && chars[i] == '=' {
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
                let quote = chars[i];
                i += 1;
                let value_start = i;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                let value: String = chars[value_start..i].iter().collect();
                if i < chars.len() {
                    i += 1; // skip closing quote
                }
                value
            } else {
                let value_start = i;
                while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '>' {
                    i += 1;
                }
                chars[value_start..i].iter().collect()
            }
        } else {
            String::new() // boolean attribute, no value
        };

        attrs.push((name, value));
    }

    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_generator_meta_keeps_functional_meta() {
        let input = r#"<head><meta charset="utf-8"><meta name="generator" content="v0.dev"><meta name="viewport" content="width=device-width"></head>"#;
        let (cleaned, report) = strip_html(input);
        assert!(cleaned.contains(r#"<meta charset="utf-8">"#));
        assert!(cleaned.contains(r#"<meta name="viewport" content="width=device-width">"#));
        assert!(!cleaned.contains("generator"));
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].label, "generator");
        assert_eq!(report.findings[0].value, "v0.dev");
    }

    #[test]
    fn strips_author_meta_and_plain_comments() {
        let input = "<head><meta name=\"author\" content=\"Jane Doe\"></head>\n<!-- built with ChatGPT -->\n<p>Hello</p>\n";
        let (cleaned, report) = strip_html(input);
        assert!(!cleaned.contains("Jane Doe"));
        assert!(!cleaned.contains("built with ChatGPT"));
        assert!(cleaned.contains("<p>Hello</p>"));
        assert_eq!(report.findings.len(), 2);
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == HtmlFindingKind::Meta));
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == HtmlFindingKind::Comment));
    }

    #[test]
    fn preserves_ie_conditional_comments() {
        let input = "<!--[if IE]>\n<p>You are using Internet Explorer.</p>\n<![endif]-->\n";
        let (cleaned, report) = strip_html(input);
        assert_eq!(cleaned, input);
        assert!(report.is_clean());
    }

    #[test]
    fn leaves_clean_html_untouched() {
        let input = "<!doctype html>\n<html><head><title>Hi</title></head><body><p>Hello, world.</p></body></html>\n";
        let (cleaned, report) = strip_html(input);
        assert_eq!(cleaned, input);
        assert!(report.is_clean());
    }

    #[test]
    fn handles_unterminated_comment_gracefully() {
        let input = "<p>before</p><!-- unterminated";
        let (cleaned, report) = strip_html(input);
        assert_eq!(cleaned, input);
        assert!(report.is_clean());
    }

    #[test]
    fn does_not_confuse_metadata_tag_with_meta() {
        let input = "<metadata name=\"author\">x</metadata>";
        let (cleaned, report) = strip_html(input);
        assert_eq!(cleaned, input);
        assert!(report.is_clean());
    }

    #[test]
    fn respects_literal_gt_inside_quoted_attribute_value() {
        let input = r#"<meta name="generator" content="a > b"><p>ok</p>"#;
        let (cleaned, report) = strip_html(input);
        assert!(cleaned.contains("<p>ok</p>"));
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].value, "a > b");
    }

    #[test]
    fn inspect_does_not_modify_input() {
        let input = "<meta name=\"author\" content=\"Jane\">";
        let report = inspect_html(input);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].value, "Jane");
    }
}
