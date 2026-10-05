//! Paragraph-first word chunks; no external tokenizer is required.
pub(super) fn chunk_text(text: &str) -> Vec<String> {
    const WORDS: usize = 256;
    const OVERLAP: usize = 32;
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    for paragraph in text.split("\n\n") {
        let words = paragraph.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() {
            continue;
        }
        if current.len() + words.len() > WORDS && !current.is_empty() {
            chunks.push(current.join(" "));
            current.clear();
        }
        if words.len() > WORDS {
            let mut start = 0;
            loop {
                let end = (start + WORDS).min(words.len());
                chunks.push(words[start..end].join(" "));
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
        chunks.push(current.join(" "));
    }
    chunks
}
