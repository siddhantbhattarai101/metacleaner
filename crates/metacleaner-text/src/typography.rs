//! Optional typography normalization: curly quotes to straight quotes,
//! em/en-dashes to a plain hyphen, non-breaking/narrow-no-break spaces to
//! a regular space.
//!
//! Off by default (see `clean-text --normalize-typography`) since, unlike
//! every other pass in this crate, it changes ordinary rendered
//! characters rather than removing hidden/identifying content. It earns
//! its place here anyway because these specific characters are genuine,
//! common AI-tool typographic artifacts — ChatGPT/Claude/Gemini/Google
//! Docs default to smart quotes, em-dashes, and non-breaking spaces where
//! a human typing on a plain keyboard would produce straight quotes,
//! hyphens, and regular spaces — so normalizing them doubles as removing
//! a provenance signal, the same spirit as stripping a `generator:`
//! frontmatter key.
//!
//! `inspect_typography` always reports findings regardless of whether the
//! caller intends to normalize, same as every other inspect function in
//! this crate.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypographyFindingKind {
    /// Curly single/double quotes and low-9 quotation marks.
    CurlyQuote,
    /// Em dash (U+2014) or en dash (U+2013).
    Dash,
    /// Non-breaking space (U+00A0) or narrow no-break space (U+202F).
    NonBreakingSpace,
}

impl TypographyFindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TypographyFindingKind::CurlyQuote => "curly-quote",
            TypographyFindingKind::Dash => "dash",
            TypographyFindingKind::NonBreakingSpace => "non-breaking-space",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypographyFinding {
    pub kind: TypographyFindingKind,
    pub codepoint: u32,
    pub count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TypographyReport {
    pub findings: Vec<TypographyFinding>,
}

impl TypographyReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Report which typography-normalization-relevant characters are
/// present, without modifying `input`.
pub fn inspect_typography(input: &str) -> TypographyReport {
    normalize_typography(input).1
}

/// Replace curly quotes / em-en-dashes / non-breaking spaces in `input`
/// with their plain-ASCII equivalents, returning the normalized text and
/// a report of what was replaced.
pub fn normalize_typography(input: &str) -> (String, TypographyReport) {
    let mut out = String::with_capacity(input.len());
    let mut counts: Vec<(TypographyFindingKind, u32, usize)> = Vec::new();

    for c in input.chars() {
        match classify(c) {
            Some((kind, replacement)) => {
                out.push(replacement);
                let cp = c as u32;
                match counts
                    .iter_mut()
                    .find(|(k, code, _)| *k == kind && *code == cp)
                {
                    Some((_, _, n)) => *n += 1,
                    None => counts.push((kind, cp, 1)),
                }
            }
            None => out.push(c),
        }
    }

    (
        out,
        TypographyReport {
            findings: counts
                .into_iter()
                .map(|(kind, codepoint, count)| TypographyFinding {
                    kind,
                    codepoint,
                    count,
                })
                .collect(),
        },
    )
}

fn classify(c: char) -> Option<(TypographyFindingKind, char)> {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => {
            Some((TypographyFindingKind::CurlyQuote, '\''))
        }
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => {
            Some((TypographyFindingKind::CurlyQuote, '"'))
        }
        '\u{2013}' | '\u{2014}' => Some((TypographyFindingKind::Dash, '-')),
        '\u{00A0}' | '\u{202F}' => Some((TypographyFindingKind::NonBreakingSpace, ' ')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_curly_quotes() {
        let input = "\u{201C}Hello,\u{201D} she said, \u{2018}it\u{2019}s fine\u{2019}.";
        let (out, report) = normalize_typography(input);
        assert_eq!(out, "\"Hello,\" she said, 'it's fine'.");
        assert!(report
            .findings
            .iter()
            .all(|f| f.kind == TypographyFindingKind::CurlyQuote));
    }

    #[test]
    fn normalizes_dashes() {
        let input = "AI models\u{2014}like this one\u{2013}often overuse em dashes.";
        let (out, report) = normalize_typography(input);
        assert_eq!(out, "AI models-like this one-often overuse em dashes.");
        assert!(report
            .findings
            .iter()
            .all(|f| f.kind == TypographyFindingKind::Dash));
    }

    #[test]
    fn normalizes_non_breaking_spaces() {
        let input = "10\u{00A0}km and 3\u{202F}pm";
        let (out, report) = normalize_typography(input);
        assert_eq!(out, "10 km and 3 pm");
        assert_eq!(report.findings.len(), 2);
    }

    #[test]
    fn leaves_plain_ascii_untouched() {
        let input = "nothing to normalize here \"quoted\" - fine.";
        let (out, report) = normalize_typography(input);
        assert_eq!(out, input);
        assert!(report.is_clean());
    }

    #[test]
    fn inspect_does_not_modify_input() {
        let input = "curly \u{2019}quote\u{2019} test";
        let report = inspect_typography(input);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].count, 2);
    }
}
