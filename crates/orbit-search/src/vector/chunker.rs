//! Paragraph-boundary chunker for fields that exceed a model's context window
//! (per ADR-003). The entry point is [`chunk_text`]; everything else is the
//! private machinery for splitting paragraphs, padding overlap, and falling
//! back to word-level splitting when a single paragraph is too long.

use orbit_common::OrbitError;

use crate::Embedder;

pub fn chunk_text(
    text: &str,
    embedder: &dyn Embedder,
    target_tokens: usize,
    overlap_tokens: usize,
) -> Result<Vec<String>, OrbitError> {
    let target_tokens = target_tokens.max(1);
    if embedder.token_count(text)? <= target_tokens {
        return Ok(vec![text.trim().to_string()]);
    }

    let paragraphs = split_paragraphs(text);
    let paragraph_refs = paragraphs.iter().map(String::as_str).collect::<Vec<_>>();
    let paragraph_counts = embedder.token_counts(&paragraph_refs)?;
    if paragraph_counts.len() != paragraphs.len() {
        return Err(OrbitError::AgentProtocolViolation(format!(
            "embedder returned {} token counts for {} paragraphs",
            paragraph_counts.len(),
            paragraphs.len()
        )));
    }
    let mut chunks = Vec::new();
    let mut current: Vec<CountedParagraph> = Vec::new();
    let mut current_tokens = 0;

    for (paragraph, paragraph_tokens) in paragraphs.into_iter().zip(paragraph_counts) {
        if paragraph_tokens > target_tokens {
            // The over-long paragraph is split on its own; no overlap tail is
            // carried into it, so the buffer and its counter reset together.
            // (The counter used to keep the discarded tail's weight, which
            // flushed the next paragraph early and embedded it twice.)
            if !current.is_empty() {
                chunks.push(join_paragraphs(&current));
                current.clear();
            }
            current_tokens = 0;
            for piece in split_long_paragraph(&paragraph, embedder, target_tokens, overlap_tokens)?
            {
                chunks.push(piece);
            }
            continue;
        }

        if !current.is_empty() && current_tokens + paragraph_tokens > target_tokens {
            chunks.push(join_paragraphs(&current));
            current = overlap_tail(&current, overlap_tokens);
            current_tokens = current.iter().map(|part| part.tokens).sum();
        }
        current.push(CountedParagraph {
            text: paragraph,
            tokens: paragraph_tokens,
        });
        current_tokens += paragraph_tokens;
    }

    if !current.is_empty() {
        chunks.push(join_paragraphs(&current));
    }
    Ok(chunks)
}

#[derive(Clone)]
struct CountedParagraph {
    text: String,
    tokens: usize,
}

fn join_paragraphs(paragraphs: &[CountedParagraph]) -> String {
    paragraphs
        .iter()
        .map(|paragraph| paragraph.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn split_paragraphs(text: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line.trim().to_string());
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join("\n"));
    }
    paragraphs
}

fn overlap_tail(paragraphs: &[CountedParagraph], overlap_tokens: usize) -> Vec<CountedParagraph> {
    if overlap_tokens == 0 {
        return Vec::new();
    }
    let mut selected = Vec::new();
    let mut total = 0;
    for paragraph in paragraphs.iter().rev() {
        if total > 0 && total + paragraph.tokens > overlap_tokens {
            break;
        }
        selected.push(paragraph.clone());
        total += paragraph.tokens;
        if total >= overlap_tokens {
            break;
        }
    }
    selected.reverse();
    selected
}

fn split_long_paragraph(
    paragraph: &str,
    embedder: &dyn Embedder,
    target_tokens: usize,
    overlap_tokens: usize,
) -> Result<Vec<String>, OrbitError> {
    let word_ends = word_ends(paragraph);
    let token_ends = embedder.token_boundaries(paragraph)?;
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < word_ends.len() {
        let preferred_end = preferred_chunk_end(start, &word_ends, &token_ends, target_tokens);

        // Token offsets come from the full paragraph, while the model counts
        // emitted chunks independently. Validate the preferred candidate once,
        // then use its measured overhead plus the boundary index to shrink it
        // without another word-at-a-time RPC loop.
        let candidate_tokens =
            embedder.token_count(word_range(paragraph, start, preferred_end, &word_ends))?;
        let (end, token_overhead) = validated_chunk_end(
            start,
            preferred_end,
            &word_ends,
            &token_ends,
            candidate_tokens,
            target_tokens,
        );

        let chunk = word_range(paragraph, start, end, &word_ends).to_string();
        chunks.push(chunk);
        if end == word_ends.len() {
            break;
        }
        start = overlap_start(
            start,
            end,
            &word_ends,
            &token_ends,
            overlap_tokens,
            token_overhead,
        );
    }
    Ok(chunks)
}

fn validated_chunk_end(
    start: usize,
    preferred_end: usize,
    word_ends: &[usize],
    token_ends: &[usize],
    candidate_tokens: usize,
    target_tokens: usize,
) -> (usize, usize) {
    let visible_tokens = token_span(start, preferred_end, word_ends, token_ends);
    let token_overhead = candidate_tokens.saturating_sub(visible_tokens);
    if candidate_tokens <= target_tokens || preferred_end == start + 1 {
        return (preferred_end, token_overhead);
    }

    let excess = candidate_tokens - target_tokens;
    let visible_budget = visible_tokens.saturating_sub(excess).max(1);
    let adjusted = preferred_chunk_end(start, word_ends, token_ends, visible_budget);
    (
        adjusted.min(preferred_end - 1).max(start + 1),
        token_overhead,
    )
}

fn token_span(start: usize, end: usize, word_ends: &[usize], token_ends: &[usize]) -> usize {
    let start_byte = if start == 0 { 0 } else { word_ends[start - 1] };
    let end_byte = word_ends[end - 1];
    let token_start = token_ends.partition_point(|token_end| *token_end <= start_byte);
    let token_end = token_ends.partition_point(|token_end| *token_end <= end_byte);
    token_end.saturating_sub(token_start)
}

fn word_ends(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut in_word = false;

    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if in_word {
                ends.push(index);
                in_word = false;
            }
        } else {
            in_word = true;
        }
    }
    if in_word {
        ends.push(text.len());
    }
    ends
}

fn preferred_chunk_end(
    start: usize,
    word_ends: &[usize],
    token_ends: &[usize],
    target_tokens: usize,
) -> usize {
    let start_byte = if start == 0 { 0 } else { word_ends[start - 1] };
    let token_start = token_ends.partition_point(|end| *end <= start_byte);
    let token_end = token_ends
        .get(token_start.saturating_add(target_tokens).saturating_sub(1))
        .copied()
        .unwrap_or(usize::MAX);
    let end = word_ends.partition_point(|end| *end <= token_end);

    end.clamp(start + 1, word_ends.len())
}

fn overlap_start(
    start: usize,
    end: usize,
    word_ends: &[usize],
    token_ends: &[usize],
    overlap_tokens: usize,
    token_overhead: usize,
) -> usize {
    let visible_budget = overlap_tokens.saturating_sub(token_overhead);
    if visible_budget == 0 {
        return end;
    }

    let range_start_byte = if start == 0 { 0 } else { word_ends[start - 1] };
    let range_start_token = token_ends.partition_point(|token_end| *token_end <= range_start_byte);
    let end_byte = word_ends[end - 1];
    let end_token = token_ends.partition_point(|token_end| *token_end <= end_byte);
    let first_overlap_token = end_token
        .saturating_sub(visible_budget)
        .max(range_start_token);
    let overlap_start = if first_overlap_token == range_start_token {
        start
    } else {
        // The overlap budget can begin in the middle of a multi-token word.
        // Binary-search the next word boundary whose first token is within
        // budget; including the partly covered word would exceed it.
        let mut low = start;
        let mut high = end;
        while low < high {
            let middle = low + (high - low) / 2;
            let start_byte = if middle == 0 {
                0
            } else {
                word_ends[middle - 1]
            };
            let start_token = token_ends.partition_point(|token_end| *token_end <= start_byte);
            if start_token < first_overlap_token {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    };

    overlap_start.clamp(start + 1, end)
}

fn word_range<'a>(paragraph: &'a str, start: usize, end: usize, word_ends: &[usize]) -> &'a str {
    let start_byte = if start == 0 { 0 } else { word_ends[start - 1] };
    paragraph[start_byte..word_ends[end - 1]].trim()
}
