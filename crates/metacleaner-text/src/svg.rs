//! Strip editor-identifying metadata from `.svg` files.
//!
//! SVG is XML, so — like [`crate::html`] — this is a hand-rolled,
//! quote-aware tag/comment scanner rather than a full XML parser: enough
//! to correctly find `<metadata>...</metadata>` blocks, XML comments, and
//! `inkscape:*`/`sodipodi:*` namespaced attributes on any element, without
//! pulling in an XML-parsing dependency. Everything not matched is copied
//! through byte-for-byte.
//!
//! `<title>`/`<desc>` are explicitly NOT touched, even when an editor puts
//! them right next to `<metadata>` — those are accessibility content
//! (screen readers announce them), not editor cruft.
//!
//! Unlike HTML comments, SVG comments have no "conditional comment"
//! exception to preserve — every comment is stripped.

const COMMENT_PREVIEW_MAX: usize = 60;
const NAMESPACE_PREFIXES: &[&str] = &["inkscape:", "sodipodi:"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgFindingKind {
    MetadataElement,
    NamespacedAttr,
    Comment,
}

impl SvgFindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SvgFindingKind::MetadataElement => "metadata-element",
            SvgFindingKind::NamespacedAttr => "namespaced-attr",
            SvgFindingKind::Comment => "comment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SvgFinding {
    pub kind: SvgFindingKind,
    /// Attribute name (e.g. "inkscape:version"), or "metadata"/"comment".
    pub label: String,
    /// Attribute value, or a truncated preview of the element/comment text.
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct SvgReport {
    pub findings: Vec<SvgFinding>,
}

impl SvgReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Report what would be stripped, without modifying `input`.
pub fn inspect_svg(input: &str) -> SvgReport {
    strip_svg(input).1
}

/// Remove `<metadata>` elements, `inkscape:*`/`sodipodi:*` attributes, and
/// comments from `input`. Everything else — including `<title>`/`<desc>`
/// and every other element/attribute — is returned byte-for-byte unchanged.
pub fn strip_svg(input: &str) -> (String, SvgReport) {
    let mut out = String::with_capacity(input.len());
    let mut findings = Vec::new();
    let mut i = 0usize;

    while i < input.len() {
        if input[i..].starts_with("<!--") {
            match input[i + 4..].find("-->") {
                Some(rel_end) => {
                    let content_end = i + 4 + rel_end;
                    let content = &input[i + 4..content_end];
                    let tag_end = content_end + 3; // past "-->"
                    findings.push(SvgFinding {
                        kind: SvgFindingKind::Comment,
                        label: "comment".to_string(),
                        value: preview(content),
                    });
                    i = tag_end;
                    continue;
                }
                None => {
                    // Unterminated comment: keep the rest verbatim.
                    out.push_str(&input[i..]);
                    break;
                }
            }
        }

        if starts_with_tag_name(&input[i..], "metadata") {
            if let Some(open_rel_end) = find_tag_end(&input[i..]) {
                let open_tag_end = i + open_rel_end + 1;
                let open_tag = &input[i..open_tag_end];
                if open_tag.trim_end().ends_with("/>") {
                    findings.push(SvgFinding {
                        kind: SvgFindingKind::MetadataElement,
                        label: "metadata".to_string(),
                        value: String::new(),
                    });
                    i = open_tag_end;
                    continue;
                }
                if let Some(close_rel) = input[open_tag_end..].find("</metadata>") {
                    let close_start = open_tag_end + close_rel;
                    let close_end = close_start + "</metadata>".len();
                    let inner = &input[open_tag_end..close_start];
                    findings.push(SvgFinding {
                        kind: SvgFindingKind::MetadataElement,
                        label: "metadata".to_string(),
                        value: preview(inner),
                    });
                    i = close_end;
                    continue;
                }
            }
            // Malformed/unterminated <metadata>: fall through to generic
            // tag handling below rather than guessing.
        }

        if input[i..].starts_with('<') && !input[i..].starts_with("</") {
            if let Some(rel_end) = find_tag_end(&input[i..]) {
                let tag_end = i + rel_end + 1;
                let tag_text = &input[i..tag_end];
                let (rewritten, removed) = strip_namespaced_attrs(tag_text);
                for (name, value) in removed {
                    findings.push(SvgFinding {
                        kind: SvgFindingKind::NamespacedAttr,
                        label: name,
                        value,
                    });
                }
                out.push_str(&rewritten);
                i = tag_end;
                continue;
            }
        }

        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    (out, SvgReport { findings })
}

fn preview(content: &str) -> String {
    let collapsed: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > COMMENT_PREVIEW_MAX {
        let truncated: String = collapsed.chars().take(COMMENT_PREVIEW_MAX).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

/// True if `s` starts with `<` + `name` (case-insensitively) followed by a
/// tag-boundary character (whitespace, `/`, or `>`).
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
/// that closes the tag `s` starts with, respecting quoted attribute values.
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

/// Rewrite a full tag string (e.g. `<path inkscape:label="x" d="M0 0">`),
/// dropping any attribute whose name starts with a namespace prefix in
/// [`NAMESPACE_PREFIXES`] (and its leading whitespace), and returning what
/// was removed as (name, value) pairs. Every other byte — tag name, kept
/// attributes, their original quoting/spacing, the closing `>`/`/>` — is
/// preserved exactly.
fn strip_namespaced_attrs(tag_text: &str) -> (String, Vec<(String, String)>) {
    let chars: Vec<char> = tag_text.chars().collect();
    let mut i = 1usize; // skip '<'
    let mut out = String::from("<");

    let name_start = i;
    while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '/' && chars[i] != '>' {
        i += 1;
    }
    out.extend(&chars[name_start..i]);

    let mut removed = Vec::new();

    while i < chars.len() {
        let ws_start = i;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        if chars[i] == '/' || chars[i] == '>' {
            out.extend(&chars[ws_start..]);
            break;
        }

        let name_start2 = i;
        while i < chars.len()
            && chars[i] != '='
            && !chars[i].is_whitespace()
            && chars[i] != '>'
            && chars[i] != '/'
        {
            i += 1;
        }
        let name: String = chars[name_start2..i].iter().collect();
        if name.is_empty() {
            // Stray character (shouldn't normally happen); copy through
            // one char at a time to guarantee forward progress.
            out.extend(&chars[ws_start..i + 1]);
            i += 1;
            continue;
        }

        let mut end = i;
        let mut value = String::new();
        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j < chars.len() && chars[j] == '=' {
            j += 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '"' || chars[j] == '\'') {
                let quote = chars[j];
                j += 1;
                let value_start = j;
                while j < chars.len() && chars[j] != quote {
                    j += 1;
                }
                value = chars[value_start..j].iter().collect();
                if j < chars.len() {
                    j += 1; // skip closing quote
                }
            } else {
                let value_start = j;
                while j < chars.len() && !chars[j].is_whitespace() && chars[j] != '>' {
                    j += 1;
                }
                value = chars[value_start..j].iter().collect();
            }
            end = j;
        }

        let is_namespaced = NAMESPACE_PREFIXES
            .iter()
            .any(|p| name.to_lowercase().starts_with(p));

        if is_namespaced {
            removed.push((name, value));
        } else {
            out.extend(&chars[ws_start..end]);
        }
        i = end;
    }

    (out, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_metadata_element_and_comments() {
        let input = r#"<svg xmlns="http://www.w3.org/2000/svg">
  <!-- Created with Inkscape (http://www.inkscape.org/) -->
  <metadata id="metadata1"><rdf:RDF><cc:Work><dc:format>image/svg+xml</dc:format></cc:Work></rdf:RDF></metadata>
  <path d="M0 0 L10 10" />
</svg>"#;
        let (cleaned, report) = strip_svg(input);
        assert!(!cleaned.contains("<metadata"));
        assert!(!cleaned.contains("Inkscape"));
        assert!(cleaned.contains(r#"<path d="M0 0 L10 10" />"#));
        assert_eq!(report.findings.len(), 2);
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == SvgFindingKind::MetadataElement));
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == SvgFindingKind::Comment));
    }

    #[test]
    fn strips_self_closing_metadata() {
        let input = r#"<svg><metadata id="x"/></svg>"#;
        let (cleaned, report) = strip_svg(input);
        assert_eq!(cleaned, "<svg></svg>");
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn strips_inkscape_and_sodipodi_attrs_but_keeps_other_attrs() {
        let input = r#"<svg
   sodipodi:docname="drawing.svg"
   inkscape:version="1.1.2"
   width="100"
   height="100">
  <path
     inkscape:label="Layer 1"
     d="M0 0"
     style="fill:#000" />
</svg>"#;
        let (cleaned, report) = strip_svg(input);
        assert!(!cleaned.contains("sodipodi:"));
        assert!(!cleaned.contains("inkscape:"));
        assert!(cleaned.contains(r#"width="100""#));
        assert!(cleaned.contains(r#"height="100""#));
        assert!(cleaned.contains(r#"d="M0 0""#));
        assert!(cleaned.contains(r#"style="fill:#000""#));
        assert_eq!(report.findings.len(), 3);
        assert!(report
            .findings
            .iter()
            .all(|f| f.kind == SvgFindingKind::NamespacedAttr));
    }

    #[test]
    fn preserves_title_and_desc() {
        let input = r#"<svg><title>A circle</title><desc>A red circle icon</desc><circle r="5"/></svg>"#;
        let (cleaned, report) = strip_svg(input);
        assert_eq!(cleaned, input);
        assert!(report.is_clean());
    }

    #[test]
    fn leaves_clean_svg_untouched() {
        let input = r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10"/></svg>"#;
        let (cleaned, report) = strip_svg(input);
        assert_eq!(cleaned, input);
        assert!(report.is_clean());
    }

    #[test]
    fn inspect_does_not_modify_input() {
        let input = r#"<svg inkscape:version="1.0"><rect/></svg>"#;
        let report = inspect_svg(input);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].label, "inkscape:version");
    }
}
