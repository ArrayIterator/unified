//! seo - analysis.

use chrono::{Utc};
use uuid::Uuid;
use super::{types::*, state::*, handlers::*, module::*};



// Analysis engine
pub(crate) fn count_word_chars(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}


/// Strip HTML tags, replacing each tag with a space so words on
/// either side of a tag never glue together (and tags themselves
/// are never counted as words).
pub(crate) fn strip_html_tags(content: &str) -> String {
    let mut stripped = String::with_capacity(content.len());
    let mut in_tag = false;
    for ch in content.chars() {
        match ch {
            '<' => {
                in_tag = true;
                stripped.push(' ');
            }
            '>' => {
                in_tag = false;
                stripped.push(' ');
            }
            _ if !in_tag => stripped.push(ch),
            _ => {}
        }
    }
    stripped
}


/// Count occurrences of `needle` in `haystack` that fall on word
/// boundaries, so `fox` matches `fox` but not `foxy` or `firefox`.
/// A match counts when the character before it (if any) and the
/// character after it (if any) are both non-alphanumeric.
pub(crate) fn count_word_boundary_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let at = start + pos;
        let before_ok = at == 0
            || !haystack[..at]
                .chars()
                .last()
                .is_some_and(char::is_alphanumeric);
        let after = at + needle.len();
        let after_ok = after >= haystack.len()
            || !haystack[after..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric);
        if before_ok && after_ok {
            count += 1;
        }
        start = after;
    }
    count
}


pub(crate) fn count_sentences(text: &str) -> usize {
    let count = text
        .chars()
        .filter(|ch| matches!(ch, '.' | '!' | '?'))
        .count();
    count.max(1)
}


/// Rough syllable estimate: count groups of vowels in each word, with a
/// minimum of one syllable per word.
pub(crate) fn count_syllables(words: &[String]) -> usize {
    let mut total = 0usize;
    for word in words {
        let lower = word.to_lowercase();
        let mut syllables = 0usize;
        let mut previous_vowel = false;
        for ch in lower.chars() {
            let is_vowel = matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
            if is_vowel && !previous_vowel {
                syllables += 1;
            }
            previous_vowel = is_vowel;
        }
        // A trailing silent "e" usually does not add a spoken syllable.
        if lower.ends_with('e') && syllables > 1 {
            syllables -= 1;
        }
        total += syllables.max(1);
    }
    total
}


/// Flesch reading ease, clamped to 0..=100. Higher is easier to read.
pub(crate) fn reading_ease(words: &[String], sentences: usize, syllables: usize) -> f64 {
    if words.is_empty() {
        return 0.0;
    }
    let words_per_sentence = words.len() as f64 / sentences as f64;
    let syllables_per_word = syllables as f64 / words.len() as f64;
    (206.835 - 1.015 * words_per_sentence - 84.6 * syllables_per_word).clamp(0.0, 100.0)
}


pub(crate) fn count_tag(content_lower: &str, tag: &str) -> usize {
    content_lower.matches(tag).count()
}


pub(crate) fn grade_for(score: u8) -> String {
    match score {
        90..=100 => "A",
        75..=89 => "B",
        60..=74 => "C",
        40..=59 => "D",
        _ => "F",
    }
    .to_string()
}


/// Run every check and return a scored analysis. `post_id` links the
/// analysis to a published post when the analyzer runs from a hook.
pub fn analyze_content(
    title: &str,
    content: &str,
    keyword: Option<&str>,
    meta_description: Option<&str>,
    url: Option<String>,
    post_id: Option<Uuid>,
) -> SeoAnalysis {
    // Word, sentence, and syllable counts run on the plain text
    // with HTML tags stripped, so tags never inflate the word count
    // or dilute keyword density. Heading counts below still use the
    // raw content, where the tags are the signal.
    let plain_text = strip_html_tags(content);
    let words = count_word_chars(&plain_text);
    let word_count = words.len();
    let sentences = count_sentences(&plain_text);
    let syllables = count_syllables(&words);
    let ease = reading_ease(&words, sentences, syllables);
    let content_lower = content.to_lowercase();
    let plain_lower = plain_text.to_lowercase();
    let title_lower = title.to_lowercase();
    let title_len = title.chars().count();

    let keyword_lower = keyword.map(str::to_lowercase);
    let keyword_occurrences = keyword_lower
        .as_ref()
        .map(|keyword| count_word_boundary_occurrences(&plain_lower, keyword))
        .unwrap_or(0);
    let keyword_density = if word_count == 0 {
        0.0
    } else {
        keyword_occurrences as f64 / word_count as f64 * 100.0
    };
    let keyword_in_title = keyword_lower
        .as_ref()
        .is_some_and(|keyword| title_lower.contains(keyword.as_str()));

    let h1_count = count_tag(&content_lower, "<h1");
    let h2_count = count_tag(&content_lower, "<h2");
    let h3_count = count_tag(&content_lower, "<h3");
    let meta_len = meta_description.map(|text| text.chars().count());

    let mut checks: Vec<SeoCheck> = Vec::new();
    let mut issues: Vec<String> = Vec::new();
    let mut suggestions: Vec<String> = Vec::new();
    let mut score: u8 = 0;

    // Title length: 30-60 characters is the ideal range. (20 points)
    let title_points = if (30..=60).contains(&title_len) {
        20
    } else if (20..=70).contains(&title_len) {
        12
    } else {
        5
    };
    score += title_points;
    let title_passed = (30..=60).contains(&title_len);
    checks.push(SeoCheck {
        name: "title_length".to_string(),
        passed: title_passed,
        weight: 20,
        detail: format!("Title is {title_len} characters long (ideal: 30-60)."),
    });
    if !title_passed {
        issues.push(format!("Title length is {title_len} characters; aim for 30-60."));
        suggestions.push("Rewrite the title to fit 30-60 characters.".to_string());
    }

    // Keyword in title. (15 points)
    match &keyword_lower {
        Some(_) => {
            if keyword_in_title {
                score += 15;
            } else {
                issues.push("Keyword does not appear in the title.".to_string());
                suggestions.push("Include the focus keyword in the title.".to_string());
            }
            checks.push(SeoCheck {
                name: "keyword_in_title".to_string(),
                passed: keyword_in_title,
                weight: 15,
                detail: if keyword_in_title {
                    "Keyword found in the title.".to_string()
                } else {
                    "Keyword missing from the title.".to_string()
                },
            });
        }
        None => {
            checks.push(SeoCheck {
                name: "keyword_in_title".to_string(),
                passed: false,
                weight: 15,
                detail: "No focus keyword was provided.".to_string(),
            });
            suggestions.push("Provide a focus keyword to check title relevance.".to_string());
        }
    }

    // Keyword density: 0.5%-2.5% is the healthy range. (15 points)
    match &keyword_lower {
        Some(_) => {
            let density_points = if (0.5..=2.5).contains(&keyword_density) {
                15
            } else if keyword_occurrences > 0 {
                7
            } else {
                0
            };
            score += density_points;
            checks.push(SeoCheck {
                name: "keyword_density".to_string(),
                passed: (0.5..=2.5).contains(&keyword_density),
                weight: 15,
                detail: format!(
                    "Keyword appears {keyword_occurrences} times ({keyword_density:.2}% density)."
                ),
            });
            if keyword_occurrences == 0 {
                issues.push("Keyword does not appear in the content at all.".to_string());
            } else if !(0.5..=2.5).contains(&keyword_density) {
                issues.push(format!(
                    "Keyword density is {keyword_density:.2}%; aim for 0.5%-2.5%."
                ));
            }
        }
        None => {
            checks.push(SeoCheck {
                name: "keyword_density".to_string(),
                passed: false,
                weight: 15,
                detail: "No focus keyword was provided.".to_string(),
            });
        }
    }

    // Meta description length: 120-160 characters is ideal. (15 points)
    match meta_len {
        Some(len) => {
            let points = if (120..=160).contains(&len) {
                15
            } else if (50..=160).contains(&len) {
                8
            } else {
                3
            };
            score += points;
            checks.push(SeoCheck {
                name: "meta_description_length".to_string(),
                passed: (120..=160).contains(&len),
                weight: 15,
                detail: format!("Meta description is {len} characters long (ideal: 120-160)."),
            });
            if !(120..=160).contains(&len) {
                issues.push(format!(
                    "Meta description is {len} characters; aim for 120-160."
                ));
            }
        }
        None => {
            checks.push(SeoCheck {
                name: "meta_description_length".to_string(),
                passed: false,
                weight: 15,
                detail: "No meta description was provided.".to_string(),
            });
            issues.push("No meta description was provided.".to_string());
            suggestions.push("Write a meta description of 120-160 characters.".to_string());
        }
    }

    // Heading structure: exactly one H1, at least one H2. (15 points)
    let headings_ok = h1_count == 1 && h2_count >= 1;
    score += if headings_ok {
        15
    } else if h1_count == 1 || h2_count >= 1 {
        8
    } else {
        0
    };
    checks.push(SeoCheck {
        name: "heading_structure".to_string(),
        passed: headings_ok,
        weight: 15,
        detail: format!("Found {h1_count} H1, {h2_count} H2, and {h3_count} H3 headings."),
    });
    if h1_count != 1 {
        issues.push(format!(
            "Expected exactly one H1 heading, found {h1_count}."
        ));
        suggestions.push("Use exactly one H1 heading per page.".to_string());
    }
    if h2_count == 0 {
        issues.push("No H2 headings found.".to_string());
        suggestions.push("Break the content into sections with H2 headings.".to_string());
    }

    // Content length: at least 300 words. (10 points)
    let length_points = if word_count >= 300 {
        10
    } else if word_count >= 150 {
        5
    } else {
        0
    };
    score += length_points;
    checks.push(SeoCheck {
        name: "content_length".to_string(),
        passed: word_count >= 300,
        weight: 10,
        detail: format!("Content has {word_count} words (recommended: at least 300)."),
    });
    if word_count < 300 {
        issues.push(format!("Content is only {word_count} words long."));
        suggestions.push("Expand the content to at least 300 words.".to_string());
    }

    // Readability: Flesch reading ease of 60 or higher. (10 points)
    let readability_points = if ease >= 60.0 {
        10
    } else if ease >= 30.0 {
        5
    } else {
        0
    };
    score += readability_points;
    checks.push(SeoCheck {
        name: "readability".to_string(),
        passed: ease >= 60.0,
        weight: 10,
        detail: format!("Flesch reading ease is {ease:.1} (60+ is easy to read)."),
    });
    if ease < 60.0 {
        issues.push(format!("Readability score is {ease:.1}; aim for 60 or higher."));
        suggestions.push("Use shorter sentences and simpler words.".to_string());
    }

    SeoAnalysis {
        id: Uuid::now_v7(),
        post_id,
        url,
        title: title.to_string(),
        keyword: keyword.map(str::to_string),
        score: score.min(100),
        grade: grade_for(score.min(100)),
        word_count,
        reading_ease: (ease * 10.0).round() / 10.0,
        keyword_density: (keyword_density * 100.0).round() / 100.0,
        h1_count,
        h2_count,
        h3_count,
        checks,
        issues,
        suggestions,
        created_at: Utc::now(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_word_chars_splits_on_whitespace() {
        assert_eq!(
            count_word_chars("hello world"),
            vec!["hello".to_string(), "world".to_string()]
        );
        assert!(count_word_chars("").is_empty());
        assert_eq!(count_word_chars("  one   two  ").len(), 2);
    }

    #[test]
    fn count_sentences_counts_terminators_with_minimum_one() {
        assert_eq!(count_sentences("Hello. World! Really?"), 3);
        assert_eq!(count_sentences("No punctuation"), 1);
        assert_eq!(count_sentences(""), 1);
    }

    #[test]
    fn count_syllables_groups_vowels_and_drops_silent_e() {
        assert_eq!(count_syllables(&["hello".to_string()]), 2);
        assert_eq!(count_syllables(&["the".to_string()]), 1);
        assert_eq!(count_syllables(&["reading".to_string()]), 2);
    }

    #[test]
    fn reading_ease_is_zero_without_words_and_capped() {
        assert_eq!(reading_ease(&[], 1, 0), 0.0);
        let words = vec!["the".to_string(), "cat".to_string()];
        assert_eq!(reading_ease(&words, 1, 2), 100.0);
    }

    #[test]
    fn reading_ease_falls_with_long_words_and_sentences() {
        let words: Vec<String> = "The extraordinary organization communicated the implementation"
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let syllables = count_syllables(&words);
        let ease = reading_ease(&words, 1, syllables);
        assert!(ease < 60.0);
        assert!(ease >= 0.0);
    }

    #[test]
    fn count_tag_counts_occurrences() {
        assert_eq!(count_tag("<h1>a</h1> <h1>b</h1>", "<h1"), 2);
        assert_eq!(count_tag("no headings", "<h1"), 0);
    }

    #[test]
    fn grade_for_maps_score_bands() {
        assert_eq!(grade_for(100).as_str(), "A");
        assert_eq!(grade_for(90).as_str(), "A");
        assert_eq!(grade_for(89).as_str(), "B");
        assert_eq!(grade_for(75).as_str(), "B");
        assert_eq!(grade_for(74).as_str(), "C");
        assert_eq!(grade_for(60).as_str(), "C");
        assert_eq!(grade_for(59).as_str(), "D");
        assert_eq!(grade_for(40).as_str(), "D");
        assert_eq!(grade_for(39).as_str(), "F");
        assert_eq!(grade_for(0).as_str(), "F");
    }

    fn sample_analysis() -> SeoAnalysis {
        // The tags stand alone as tokens here: counting the raw HTML
        // would give 22 words, but the true text has 18.
        analyze_content(
            "The quick brown fox guide for testing now",
            "<h1> Guide </h1> <h2> Part one </h2> The quick brown fox jumps over the lazy dog. The quick brown fox is fast.",
            Some("fox"),
            None,
            None,
            None,
        )
    }

    #[test]
    fn analyze_content_counts_words_and_headings() {
        let analysis = sample_analysis();
        assert_eq!(analysis.word_count, 18);
        assert_eq!(analysis.h1_count, 1);
        assert_eq!(analysis.h2_count, 1);
        assert_eq!(analysis.keyword_density, 11.11);
    }

    #[test]
    fn strip_html_tags_replaces_tags_with_spaces_without_gluing_words() {
        assert_eq!(
            strip_html_tags("<h1>Guide</h1><p>Next</p>").as_str(),
            "  Guide    Next  "
        );
        assert_eq!(strip_html_tags("plain text").as_str(), "plain text");
        assert_eq!(strip_html_tags("").as_str(), "");
    }

    #[test]
    fn count_word_boundary_occurrences_ignores_substring_matches() {
        assert_eq!(
            count_word_boundary_occurrences("foxy firefox fox", "fox"),
            1
        );
        assert_eq!(count_word_boundary_occurrences("fox fox", "fox"), 2);
        assert_eq!(count_word_boundary_occurrences("(fox)", "fox"), 1);
        assert_eq!(count_word_boundary_occurrences("foxes", "fox"), 0);
        assert_eq!(
            count_word_boundary_occurrences("quick brown fox", "quick brown"),
            1
        );
        assert_eq!(count_word_boundary_occurrences("fox", ""), 0);
    }

    #[test]
    fn analyze_content_ignores_html_tags_when_counting_words() {
        let analysis = analyze_content(
            "The quick brown fox guide for testing now",
            "<div> <p> The quick brown fox </p> </div>",
            Some("fox"),
            None,
            None,
            None,
        );
        assert_eq!(analysis.word_count, 4);
        assert_eq!(analysis.keyword_density, 25.0);
    }

    #[test]
    fn analyze_content_matches_keywords_on_word_boundaries_only() {
        let analysis = analyze_content(
            "The quick brown fox guide for testing now",
            "The foxy firefox fox runs.",
            Some("fox"),
            None,
            None,
            None,
        );
        assert_eq!(analysis.word_count, 5);
        assert_eq!(analysis.keyword_density, 20.0);
    }

    #[test]
    fn analyze_content_checks_title_keyword_and_headings() {
        let analysis = sample_analysis();
        let by_name = |name: &str| analysis.checks.iter().find(|c| c.name == name).unwrap();
        assert!(by_name("title_length").passed);
        assert!(by_name("keyword_in_title").passed);
        assert!(!by_name("keyword_density").passed);
        assert!(by_name("heading_structure").passed);
        assert!(!by_name("content_length").passed);
        assert!(analysis.score <= 100);
        assert_eq!(analysis.grade, grade_for(analysis.score));
        assert!(analysis.reading_ease > 60.0);
    }

    #[test]
    fn analyze_content_without_keyword_reports_no_keyword_checks() {
        let analysis = analyze_content(
            "Short title",
            "Plain content without headings.",
            None,
            None,
            None,
            None,
        );
        let by_name = |name: &str| analysis.checks.iter().find(|c| c.name == name).unwrap();
        assert!(!by_name("keyword_in_title").passed);
        assert!(!by_name("keyword_density").passed);
        assert_eq!(analysis.word_count, 4);
    }
}
