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
    let mut chunks = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_tokens = 0;

    for paragraph in paragraphs {
        let paragraph_tokens = embedder.token_count(&paragraph)?;
        if paragraph_tokens > target_tokens {
            // The over-long paragraph is split on its own; no overlap tail is
            // carried into it, so the buffer and its counter reset together.
            // (The counter used to keep the discarded tail's weight, which
            // flushed the next paragraph early and embedded it twice.)
            if !current.is_empty() {
                chunks.push(current.join("\n\n"));
                current.clear();
                current_tokens = 0;
            }
            for piece in split_long_paragraph(&paragraph, embedder, target_tokens, overlap_tokens)?
            {
                chunks.push(piece);
            }
            continue;
        }

        if !current.is_empty() && current_tokens + paragraph_tokens > target_tokens {
            chunks.push(current.join("\n\n"));
            current = overlap_tail(&current, embedder, overlap_tokens)?;
            current_tokens = count_parts(&current, embedder)?;
        }
        current.push(paragraph);
        current_tokens += paragraph_tokens;
    }

    if !current.is_empty() {
        chunks.push(current.join("\n\n"));
    }
    Ok(chunks)
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

fn overlap_tail(
    paragraphs: &[String],
    embedder: &dyn Embedder,
    overlap_tokens: usize,
) -> Result<Vec<String>, OrbitError> {
    if overlap_tokens == 0 {
        return Ok(Vec::new());
    }
    let mut selected = Vec::new();
    let mut total = 0;
    for paragraph in paragraphs.iter().rev() {
        let tokens = embedder.token_count(paragraph)?;
        if total > 0 && total + tokens > overlap_tokens {
            break;
        }
        selected.push(paragraph.clone());
        total += tokens;
        if total >= overlap_tokens {
            break;
        }
    }
    selected.reverse();
    Ok(selected)
}

fn count_parts(parts: &[String], embedder: &dyn Embedder) -> Result<usize, OrbitError> {
    parts
        .iter()
        .try_fold(0, |sum, part| Ok(sum + embedder.token_count(part)?))
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
        let mut end = preferred_chunk_end(start, &word_ends, &token_ends, target_tokens);

        // Token offsets come from the full paragraph, while the model counts
        // emitted chunks independently. Validate each candidate instead of
        // assuming those counts are additive or monotone across word ranges.
        while embedder.token_count(word_range(paragraph, start, end, &word_ends))? > target_tokens {
            if end == start + 1 {
                // A word that alone exceeds the limit is an unbreakable unit:
                // emit it intact rather than lose text or stall forever.
                break;
            }
            end -= 1;
        }

        let chunk = word_range(paragraph, start, end, &word_ends).to_string();
        chunks.push(chunk);
        if end == word_ends.len() {
            break;
        }
        start = overlap_start(paragraph, start, end, &word_ends, embedder, overlap_tokens)?;
    }
    Ok(chunks)
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
    paragraph: &str,
    start: usize,
    end: usize,
    word_ends: &[usize],
    embedder: &dyn Embedder,
    overlap_tokens: usize,
) -> Result<usize, OrbitError> {
    if overlap_tokens == 0 {
        return Ok(end);
    }

    let mut overlap_start = end;
    while overlap_start > start + 1 {
        let candidate_start = overlap_start - 1;
        if embedder.token_count(word_range(paragraph, candidate_start, end, word_ends))?
            > overlap_tokens
        {
            break;
        }
        overlap_start = candidate_start;
    }

    Ok(overlap_start.max(start + 1))
}

fn word_range<'a>(paragraph: &'a str, start: usize, end: usize, word_ends: &[usize]) -> &'a str {
    let start_byte = if start == 0 { 0 } else { word_ends[start - 1] };
    paragraph[start_byte..word_ends[end - 1]].trim()
}
