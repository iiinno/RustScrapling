use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RankedParagraph {
    /// Original zero-based paragraph position (stable when scores tie).
    pub index: usize,
    pub text: String,
    /// BM25 with k1=1.2, b=0.75. Zero when no query is supplied.
    pub score: f64,
}

/// Rank paragraphs with Okapi BM25. Japanese, Chinese and Korean text uses
/// overlapping character bigrams so a query works without word boundaries.
/// Other alphabetic words are case-folded and split at punctuation.
pub fn rank_paragraphs(paragraphs: &[String], query: &str) -> Vec<RankedParagraph> {
    let mut result: Vec<_> = paragraphs
        .iter()
        .enumerate()
        .map(|(index, text)| RankedParagraph {
            index,
            text: text.clone(),
            score: 0.0,
        })
        .collect();
    let query_terms: BTreeSet<String> = tokenize(query).into_iter().collect();
    if paragraphs.is_empty() || query_terms.is_empty() {
        return result;
    }
    let doc_terms: Vec<Vec<String>> = paragraphs.iter().map(|p| tokenize(p)).collect();
    let doc_lengths: Vec<f64> = doc_terms.iter().map(|p| p.len() as f64).collect();
    let avg_length = doc_lengths.iter().sum::<f64>() / doc_lengths.len() as f64;
    if avg_length == 0.0 {
        return result;
    }
    let mut df = BTreeMap::<String, usize>::new();
    for document in &doc_terms {
        for term in document.iter().cloned().collect::<BTreeSet<_>>() {
            *df.entry(term).or_default() += 1;
        }
    }
    let count = paragraphs.len() as f64;
    for ((doc, length), paragraph) in doc_terms.iter().zip(doc_lengths).zip(&mut result) {
        let mut tf = BTreeMap::<&str, usize>::new();
        for term in doc {
            *tf.entry(term).or_default() += 1;
        }
        for query_term in &query_terms {
            let freq = *tf.get(query_term.as_str()).unwrap_or(&0) as f64;
            if freq == 0.0 {
                continue;
            }
            let doc_freq = *df.get(query_term).unwrap_or(&0) as f64;
            let idf = (1.0 + (count - doc_freq + 0.5) / (doc_freq + 0.5)).ln();
            paragraph.score +=
                idf * (freq * 2.2) / (freq + 1.2 * (0.25 + 0.75 * length / avg_length));
        }
    }
    result.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.index.cmp(&b.index)));
    result
}

fn tokenize(input: &str) -> Vec<String> {
    fn cjk(c: char) -> bool {
        matches!(c as u32,
            0x3040..=0x30ff | 0x3400..=0x9fff | 0xf900..=0xfaff |
            0xac00..=0xd7af | 0x31f0..=0x31ff | 0xff66..=0xff9f)
    }
    let mut output = Vec::new();
    let mut word = String::new();
    let mut asian = Vec::<char>::new();
    let flush_word = |word: &mut String, out: &mut Vec<String>| {
        if !word.is_empty() {
            out.push(std::mem::take(word));
        }
    };
    let flush_asian = |asian: &mut Vec<char>, out: &mut Vec<String>| {
        if asian.len() == 1 {
            out.push(asian[0].to_string());
        }
        for pair in asian.windows(2) {
            out.push(pair.iter().collect());
        }
        asian.clear();
    };
    for c in input.chars().flat_map(char::to_lowercase) {
        if cjk(c) {
            flush_word(&mut word, &mut output);
            asian.push(c);
        } else if c.is_alphanumeric() {
            flush_asian(&mut asian, &mut output);
            word.push(c);
        } else {
            flush_asian(&mut asian, &mut output);
            flush_word(&mut word, &mut output);
        }
    }
    flush_asian(&mut asian, &mut output);
    flush_word(&mut word, &mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_without_reordering_ties() {
        let docs = [
            "Rust supports memory safety".into(),
            "Memory management in C".into(),
            "Rust Rust memory safety".into(),
        ];
        let result = rank_paragraphs(&docs, "rust memory safety");
        assert_eq!(result[0].index, 2);
        assert!(result[0].score > result[1].score);
        assert_eq!(
            rank_paragraphs(&docs, "")
                .iter()
                .map(|p| p.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn japanese_substrings_match_without_spaces() {
        let docs = [
            "写真と猫の話です".into(),
            "日本語の音声認識を研究する".into(),
            "今日は良い天気".into(),
        ];
        assert_eq!(rank_paragraphs(&docs, "音声認識")[0].index, 1);
    }
}
