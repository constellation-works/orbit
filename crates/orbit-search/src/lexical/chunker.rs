//! Paragraph-first word chunks; no external tokenizer is required.
//!
//! Chunks are slices of the original text spanning whole words, so the
//! whitespace between words (tabs, newlines, repeated spaces) is preserved
//! exactly. The substring supplement searches chunk text for raw substrings;
//! re-joining words with single spaces would lose those matches.
pub(super) fn chunk_text(text: &str) -> Vec<String> {
    const WORDS: usize = 256;
    const OVERLAP: usize = 32;
    let mut chunks = Vec::new();
    // Byte span (start, end) of the words in the chunk being accumulated.
    let mut current: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0;
    for paragraph in text.split("\n\n") {
        let words = word_spans(paragraph, offset);
        offset += paragraph.len() + 2;
        if words.is_empty() {
            continue;
        }
        if current.len() + words.len() > WORDS && !current.is_empty() {
            chunks.push(slice_words(text, &current));
            current.clear();
        }
        if words.len() > WORDS {
            let mut start = 0;
            loop {
                let end = (start + WORDS).min(words.len());
                chunks.push(slice_words(text, &words[start..end]));
                if end == words.len() {
                    break;
                }
                start = end - OVERLAP;
            }
        } else {
            current.extend(words);
        }
    }
    if !current.is_empty() {
        chunks.push(slice_words(text, &current));
    }
    chunks
}

/// The chunk covering `words`: the original text from the first word's start
/// to the last word's end, with every intervening byte intact.
fn slice_words(text: &str, words: &[(usize, usize)]) -> String {
    text[words[0].0..words[words.len() - 1].1].to_string()
}

/// Byte spans of the whitespace-separated words in `text`, offset by `base`.
fn word_spans(text: &str, base: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(word_start) = start.take() {
                spans.push((base + word_start, base + index));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(word_start) = start {
        spans.push((base + word_start, base + text.len()));
    }
    spans
}
