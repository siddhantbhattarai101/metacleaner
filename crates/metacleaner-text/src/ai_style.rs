//! Advisory-only detector for common AI-writing stylistic tells:
//! vocabulary/phrases and structural patterns documented as frequently
//! associated with LLM-generated prose. The lexicon is drawn from
//! Wikipedia's "Signs of AI writing" essay and published AI-detection
//! commentary; the statistical checks (sentence-length burstiness,
//! em-dash rate) are the subset of that research computable offline,
//! without a reference language model.
//!
//! This is intentionally **not** a score, percentage, or verdict, and it
//! **never** modifies text or affects any exit code. AI-detection
//! heuristics like these have well-documented false-positive problems —
//! they disproportionately flag non-native-English and formal/academic
//! writing, because the same low lexical-variability signal that trips
//! them occurs naturally in both. [`FALSE_POSITIVE_CAVEAT`] is meant to
//! travel with every report this produces. Treat every finding here as
//! "maybe worth a second look", never as proof of anything.
//!
//! Scope, deliberately: lexicon/substring matching plus simple sentence-
//! and word-level statistics only. Perplexity/burstiness detectors like
//! DetectGPT or Binoculars need a reference model to compare against —
//! out of scope for an offline, dependency-light CLI, and explicitly not
//! reimplemented here even approximately.

pub const FALSE_POSITIVE_CAVEAT: &str = "Advisory only: these patterns also occur in genuine \
     human writing, especially formal/academic prose and non-native-English writing. \
     Not a score, and not proof of AI authorship.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiStyleCategory {
    Vocabulary,
    Phrase,
    Statistic,
}

impl AiStyleCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            AiStyleCategory::Vocabulary => "vocabulary",
            AiStyleCategory::Phrase => "phrase",
            AiStyleCategory::Statistic => "statistic",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiStyleFinding {
    pub category: AiStyleCategory,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct AiStyleReport {
    pub findings: Vec<AiStyleFinding>,
}

impl AiStyleReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Single words/word-stems widely reported as disproportionately common
/// in LLM output relative to typical human prose baselines.
const VOCAB_TELLS: &[&str] = &[
    "delve",
    "boasts",
    "bolstered",
    "crucial",
    "underscore",
    "underscores",
    "garner",
    "garnered",
    "intricate",
    "intricacies",
    "meticulous",
    "meticulously",
    "tapestry",
    "testament",
    "vibrant",
    "showcase",
    "showcasing",
    "foster",
    "fostering",
    "pivotal",
    "leverage",
    "leveraging",
    "utilize",
    "utilizing",
    "harness",
    "harnessing",
    "streamline",
    "streamlining",
    "encompass",
    "encompassing",
    "exemplify",
    "exemplifies",
    "groundbreaking",
    "unwavering",
    "multifaceted",
    "landscape",
    "realm",
    "synergy",
    "underpinnings",
    "cutting-edge",
    "seamless",
    "indelible",
];

/// Multi-word stock phrases widely reported as AI-writing tells.
const PHRASE_TELLS: &[&str] = &[
    "it's important to note",
    "it is important to note",
    "in today's fast-paced world",
    "in the ever-evolving",
    "when it comes to",
    "stands as a testament",
    "serves as a testament",
    "plays a pivotal role",
    "not only does",
    "not just a",
];

/// Report advisory AI-writing-pattern findings in `input`. Purely
/// read-only, purely informational — see the module docs and
/// [`FALSE_POSITIVE_CAVEAT`].
pub fn inspect_ai_style(input: &str) -> AiStyleReport {
    let mut findings = Vec::new();
    let lower = input.to_lowercase();

    for &word in VOCAB_TELLS {
        let count = count_word_occurrences(&lower, word);
        if count > 0 {
            findings.push(AiStyleFinding {
                category: AiStyleCategory::Vocabulary,
                label: word.to_string(),
                detail: format!("×{count}"),
            });
        }
    }

    for &phrase in PHRASE_TELLS {
        let count = lower.matches(phrase).count();
        if count > 0 {
            findings.push(AiStyleFinding {
                category: AiStyleCategory::Phrase,
                label: phrase.to_string(),
                detail: format!("×{count}"),
            });
        }
    }

    if let Some(f) = sentence_burstiness_finding(input) {
        findings.push(f);
    }
    if let Some(f) = em_dash_rate_finding(input) {
        findings.push(f);
    }

    AiStyleReport { findings }
}

/// Count whole-word occurrences of `needle` in `lower_haystack` (both
/// already lowercase). Multi-word phrases fall back to plain substring
/// counting since word-boundary checks only make sense at the ends.
fn count_word_occurrences(lower_haystack: &str, needle: &str) -> usize {
    if needle.contains(' ') || needle.contains('-') {
        return lower_haystack.matches(needle).count();
    }

    let bytes = lower_haystack.as_bytes();
    let needle_len = needle.len();
    let mut count = 0;
    let mut search_start = 0;

    while let Some(rel) = lower_haystack[search_start..].find(needle) {
        let idx = search_start + rel;
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let end = idx + needle_len;
        let after_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            count += 1;
        }
        search_start = idx + needle_len.max(1);
    }

    count
}

/// Flag unusually uniform sentence length (low "burstiness") — human
/// writing tends to mix short and long sentences; LLM output more often
/// settles into a narrow band. Needs enough sentences to be meaningful,
/// so short snippets never trigger this.
fn sentence_burstiness_finding(input: &str) -> Option<AiStyleFinding> {
    let lengths: Vec<usize> = split_sentences(input)
        .into_iter()
        .map(|s| s.split_whitespace().count())
        .filter(|&n| n > 0)
        .collect();

    const MIN_SENTENCES: usize = 6;
    const MIN_MEAN_WORDS: f64 = 3.0;
    const LOW_BURSTINESS_THRESHOLD: f64 = 0.35;

    if lengths.len() < MIN_SENTENCES {
        return None;
    }

    let mean = lengths.iter().sum::<usize>() as f64 / lengths.len() as f64;
    if mean < MIN_MEAN_WORDS {
        return None;
    }

    let variance = lengths
        .iter()
        .map(|&n| {
            let d = n as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / lengths.len() as f64;
    let coefficient_of_variation = variance.sqrt() / mean;

    if coefficient_of_variation < LOW_BURSTINESS_THRESHOLD {
        Some(AiStyleFinding {
            category: AiStyleCategory::Statistic,
            label: "uniform sentence length".to_string(),
            detail: format!(
                "coefficient of variation {coefficient_of_variation:.2} across {} sentences (low burstiness)",
                lengths.len()
            ),
        })
    } else {
        None
    }
}

fn split_sentences(input: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let bytes = input.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'.' || b == b'!' || b == b'?' {
            sentences.push(&input[start..=i]);
            start = i + 1;
        }
    }
    if start < input.len() {
        sentences.push(&input[start..]);
    }
    sentences
}

/// Flag an elevated em-dash rate. Needs enough words that a handful of
/// legitimate em-dashes doesn't trip it on short text.
fn em_dash_rate_finding(input: &str) -> Option<AiStyleFinding> {
    const MIN_WORDS: usize = 100;
    const RATE_PER_1000_THRESHOLD: f64 = 4.0;

    let word_count = input.split_whitespace().count();
    if word_count < MIN_WORDS {
        return None;
    }

    let em_dash_count = input.chars().filter(|&c| c == '\u{2014}').count();
    let rate_per_1000 = em_dash_count as f64 / word_count as f64 * 1000.0;

    if rate_per_1000 >= RATE_PER_1000_THRESHOLD {
        Some(AiStyleFinding {
            category: AiStyleCategory::Statistic,
            label: "elevated em dash usage".to_string(),
            detail: format!(
                "{em_dash_count} em dash(es) across {word_count} words ({rate_per_1000:.1} per 1000 words)"
            ),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_vocabulary_tells() {
        let report = inspect_ai_style("We need to delve into the intricate landscape of this.");
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == AiStyleCategory::Vocabulary && f.label == "delve"));
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == AiStyleCategory::Vocabulary && f.label == "intricate"));
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == AiStyleCategory::Vocabulary && f.label == "landscape"));
    }

    #[test]
    fn does_not_match_substrings_inside_unrelated_words() {
        // "realm" is a tell; "really" should not trip it via substring match.
        let report = inspect_ai_style("I really like this idea a lot.");
        assert!(!report
            .findings
            .iter()
            .any(|f| f.category == AiStyleCategory::Vocabulary && f.label == "realm"));
    }

    #[test]
    fn flags_phrase_tells() {
        let report =
            inspect_ai_style("It's important to note that this changes things significantly.");
        assert!(report
            .findings
            .iter()
            .any(|f| f.category == AiStyleCategory::Phrase
                && f.label == "it's important to note"));
    }

    #[test]
    fn clean_ordinary_text_has_no_findings() {
        let report = inspect_ai_style("The cat sat on the mat. It was warm in the sun.");
        assert!(report.is_clean());
    }

    #[test]
    fn short_text_never_triggers_statistical_checks() {
        let report = inspect_ai_style("Short sentence. Another one. And one more.");
        assert!(!report
            .findings
            .iter()
            .any(|f| f.category == AiStyleCategory::Statistic));
    }

    #[test]
    fn flags_low_burstiness_on_uniform_sentences() {
        let text = "The team reviewed the plan carefully. \
             The team reviewed the budget carefully. \
             The team reviewed the schedule carefully. \
             The team reviewed the risks carefully. \
             The team reviewed the scope carefully. \
             The team reviewed the timeline carefully.";
        let report = inspect_ai_style(text);
        assert!(report.findings.iter().any(
            |f| f.category == AiStyleCategory::Statistic && f.label == "uniform sentence length"
        ));
    }
}
