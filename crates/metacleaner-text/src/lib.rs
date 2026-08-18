//! Strip invisible-Unicode steganography from plain text: zero-width
//! characters, bidirectional-control overrides, the (deprecated, no
//! legitimate use) Unicode Tag block, and supplementary-plane variation
//! selectors. These are the same techniques used to invisibly watermark
//! LLM output (bit-encode a signature via presence/absence of zero-width
//! characters between words) and — more seriously — to smuggle hidden
//! instructions past a reader/moderator entirely: the 2025 "EchoLeak"
//! prompt-injection attack against Microsoft 365 Copilot (CVE-2025-32711)
//! used Unicode Tag block characters to hide an invisible payload inside
//! ordinary-looking text. So this isn't just AI-provenance hygiene, it
//! doubles as a prompt-injection defense.
//!
//! Scope note: this targets a specific, well-documented list of
//! known-invisible / known-abused codepoints rather than attempting a
//! general Unicode "format character" (`Cf` general-category) classifier —
//! building and maintaining a full Unicode category table is a much bigger
//! undertaking than this crate's "no dependencies, small, auditable" goal
//! allows. The covered set matches what the research behind this feature
//! identified as the actual attack surface in practice.
//!
//! Context-aware by design: zero-width joiner (U+200D) is left alone by
//! default because it's legitimate and extremely common (it's what joins
//! base emoji into family/profession sequences, e.g. 👨‍👩‍👧‍👦) —
//! stripping it indiscriminately would visibly break those, not just clean
//! metadata. Every other targeted codepoint has no legitimate role in
//! ordinary prose, so they're stripped by default.

use std::fmt;

/// What kind of invisible/steganography-relevant character a
/// [`TextFinding`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFindingCategory {
    /// Zero-width space, zero-width non-joiner, word joiner, a
    /// not-at-the-start byte-order mark, soft hyphen — invisible under
    /// normal rendering, no typographic purpose in plain prose.
    ZeroWidth,
    /// Zero-width joiner (U+200D) specifically — reported separately from
    /// [`TextFindingCategory::ZeroWidth`] because it's legitimate (emoji
    /// sequences) and preserved by default; only reported/stripped when
    /// the caller explicitly opts in.
    ZeroWidthJoiner,
    /// Bidirectional embedding/override/isolate controls and direction
    /// marks — can silently change how surrounding text renders (used in
    /// real filename/extension-spoofing attacks), or just carry no
    /// purpose outside genuine complex-script RTL/LTR embedding.
    BidiControl,
    /// The Unicode Tag block (U+E0000-U+E007F) — originally proposed for
    /// language tagging, deprecated for that use, and today has no
    /// legitimate role in ordinary text. Purely a steganography vector.
    UnicodeTag,
    /// Supplementary-plane variation selectors (U+E0100-U+E01EF) — a rarer
    /// legitimate use exists (CJK ideographic variation selection via the
    /// IVD), but this range is the one identified as actively used for
    /// steganographic payload smuggling.
    VariationSelectorSupplement,
}

impl TextFindingCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            TextFindingCategory::ZeroWidth => "zero-width",
            TextFindingCategory::ZeroWidthJoiner => "zero-width-joiner",
            TextFindingCategory::BidiControl => "bidi-control",
            TextFindingCategory::UnicodeTag => "unicode-tag",
            TextFindingCategory::VariationSelectorSupplement => "variation-selector",
        }
    }
}

impl fmt::Display for TextFindingCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One distinct invisible codepoint found in the text, and how many times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextFinding {
    pub category: TextFindingCategory,
    pub codepoint: u32,
    pub count: usize,
}

/// Options controlling which categories get stripped by [`clean_text`].
/// All default to "strip" except the zero-width joiner, which defaults to
/// preserved (see the module docs for why).
#[derive(Debug, Clone, Copy)]
pub struct CleanTextOptions {
    pub strip_zero_width: bool,
    pub strip_zero_width_joiner: bool,
    pub strip_bidi_controls: bool,
    pub strip_unicode_tags: bool,
    pub strip_variation_selector_supplement: bool,
}

impl Default for CleanTextOptions {
    fn default() -> Self {
        Self {
            strip_zero_width: true,
            strip_zero_width_joiner: false,
            strip_bidi_controls: true,
            strip_unicode_tags: true,
            strip_variation_selector_supplement: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InspectTextReport {
    pub char_count: usize,
    pub findings: Vec<TextFinding>,
}

impl InspectTextReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct CleanTextReport {
    pub chars_in: usize,
    pub chars_out: usize,
    pub removed: Vec<TextFinding>,
}

impl CleanTextReport {
    pub fn is_clean(&self) -> bool {
        self.removed.is_empty()
    }
}

/// Report which targeted invisible/steganography-relevant codepoints are
/// present, without modifying `input`. Every codepoint in scope is
/// reported here regardless of [`CleanTextOptions`] — inspection is
/// read-only, so there's no reason to hide anything from the report.
pub fn inspect_text(input: &str) -> InspectTextReport {
    let mut counts: Vec<(TextFindingCategory, u32, usize)> = Vec::new();
    let mut char_count = 0usize;

    for (i, c) in input.chars().enumerate() {
        char_count += 1;
        let Some(category) = classify(c, i == 0) else {
            continue;
        };
        let cp = c as u32;
        match counts
            .iter_mut()
            .find(|(cat, code, _)| *cat == category && *code == cp)
        {
            Some((_, _, n)) => *n += 1,
            None => counts.push((category, cp, 1)),
        }
    }

    InspectTextReport {
        char_count,
        findings: counts
            .into_iter()
            .map(|(category, codepoint, count)| TextFinding {
                category,
                codepoint,
                count,
            })
            .collect(),
    }
}

/// Strip the codepoint categories enabled in `opts` from `input`, returning
/// the cleaned text and a report of what was actually removed.
pub fn clean_text(input: &str, opts: &CleanTextOptions) -> (String, CleanTextReport) {
    let mut out = String::with_capacity(input.len());
    let mut removed: Vec<(TextFindingCategory, u32, usize)> = Vec::new();
    let mut chars_in = 0usize;
    let mut chars_out = 0usize;

    for (i, c) in input.chars().enumerate() {
        chars_in += 1;
        let category = classify(c, i == 0);
        let strip = match category {
            Some(TextFindingCategory::ZeroWidth) => opts.strip_zero_width,
            Some(TextFindingCategory::ZeroWidthJoiner) => opts.strip_zero_width_joiner,
            Some(TextFindingCategory::BidiControl) => opts.strip_bidi_controls,
            Some(TextFindingCategory::UnicodeTag) => opts.strip_unicode_tags,
            Some(TextFindingCategory::VariationSelectorSupplement) => {
                opts.strip_variation_selector_supplement
            }
            None => false,
        };

        if strip {
            let category = category.expect("strip is only true when category is Some");
            let cp = c as u32;
            match removed
                .iter_mut()
                .find(|(cat, code, _)| *cat == category && *code == cp)
            {
                Some((_, _, n)) => *n += 1,
                None => removed.push((category, cp, 1)),
            }
        } else {
            out.push(c);
            chars_out += 1;
        }
    }

    (
        out,
        CleanTextReport {
            chars_in,
            chars_out,
            removed: removed
                .into_iter()
                .map(|(category, codepoint, count)| TextFinding {
                    category,
                    codepoint,
                    count,
                })
                .collect(),
        },
    )
}

/// Classify a single character. `is_first` matters only for U+FEFF: at the
/// very start of a text it's a legitimate byte-order mark, anywhere else
/// it's an invisible no-break space with no reason to be there.
fn classify(c: char, is_first: bool) -> Option<TextFindingCategory> {
    let cp = c as u32;
    match cp {
        0x200B | 0x200C | 0x2060 | 0x00AD | 0x061C | 0x200E | 0x200F => {
            Some(TextFindingCategory::ZeroWidth)
        }
        0xFEFF if !is_first => Some(TextFindingCategory::ZeroWidth),
        0x200D => Some(TextFindingCategory::ZeroWidthJoiner),
        0x202A..=0x202E | 0x2066..=0x2069 => Some(TextFindingCategory::BidiControl),
        0xE0000..=0xE007F => Some(TextFindingCategory::UnicodeTag),
        0xE0100..=0xE01EF => Some(TextFindingCategory::VariationSelectorSupplement),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_zero_width_space_watermark_pattern() {
        // A classic zero-width-character watermark: bits encoded as
        // ZWSP/word-joiner inserted between words.
        let input = "The\u{200B}quick\u{2060}brown\u{200B}fox";
        let (cleaned, report) = clean_text(input, &CleanTextOptions::default());
        assert_eq!(cleaned, "Thequickbrownfox");
        assert_eq!(report.chars_in - report.chars_out, 3);
        assert!(!report.is_clean());
    }

    #[test]
    fn preserves_zwj_emoji_sequence_by_default() {
        // Family emoji: person + ZWJ + person + ZWJ + child.
        let input = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let (cleaned, report) = clean_text(input, &CleanTextOptions::default());
        assert_eq!(cleaned, input, "ZWJ must survive by default");
        assert!(report.is_clean());
    }

    #[test]
    fn strips_zwj_when_explicitly_requested() {
        let input = "a\u{200D}b";
        let opts = CleanTextOptions {
            strip_zero_width_joiner: true,
            ..Default::default()
        };
        let (cleaned, _) = clean_text(input, &opts);
        assert_eq!(cleaned, "ab");
    }

    #[test]
    fn strips_unicode_tag_block_smuggling() {
        // Tag-block payload smuggled after visible text, as in the
        // EchoLeak-class attacks.
        let mut input = String::from("Looks completely normal");
        input.push('\u{E0001}');
        for ch in "secret".chars() {
            input.push(char::from_u32(0xE0000 + ch as u32).unwrap());
        }
        input.push('\u{E007F}');

        let (cleaned, report) = clean_text(&input, &CleanTextOptions::default());
        assert_eq!(cleaned, "Looks completely normal");
        assert!(report
            .removed
            .iter()
            .all(|f| f.category == TextFindingCategory::UnicodeTag));
    }

    #[test]
    fn strips_supplementary_variation_selectors() {
        let input = "hidden\u{E0100}\u{E0101}payload";
        let (cleaned, report) = clean_text(input, &CleanTextOptions::default());
        assert_eq!(cleaned, "hiddenpayload");
        assert_eq!(report.removed.len(), 2);
    }

    #[test]
    fn preserves_leading_bom_but_strips_mid_text_one() {
        let input = "\u{FEFF}Hello\u{FEFF}World";
        let (cleaned, report) = clean_text(input, &CleanTextOptions::default());
        assert_eq!(cleaned, "\u{FEFF}HelloWorld");
        assert_eq!(report.removed.len(), 1);
    }

    #[test]
    fn strips_bidi_override_spoofing_pattern() {
        // RLO-based extension spoofing: "invoice" + RLO + "exe.txt" renders
        // as "invoicetxt.exe" visually.
        let input = "invoice\u{202E}exe.txt";
        let (cleaned, report) = clean_text(input, &CleanTextOptions::default());
        assert_eq!(cleaned, "invoiceexe.txt");
        assert!(report
            .removed
            .iter()
            .all(|f| f.category == TextFindingCategory::BidiControl));
    }

    #[test]
    fn clean_text_is_a_no_op_on_plain_ascii() {
        let input = "nothing unusual here.";
        let (cleaned, report) = clean_text(input, &CleanTextOptions::default());
        assert_eq!(cleaned, input);
        assert!(report.is_clean());
    }

    #[test]
    fn inspect_reports_regardless_of_options_and_does_not_modify() {
        let input = "a\u{200B}b\u{200D}c";
        let report = inspect_text(input);
        assert_eq!(report.char_count, 5);
        assert_eq!(report.findings.len(), 2);
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == TextFindingCategory::ZeroWidth));
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == TextFindingCategory::ZeroWidthJoiner));
    }
}
