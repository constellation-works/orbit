use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocSearchSource {
    pub path: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_features: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_artifacts: Vec<String>,
    #[serde(skip)]
    pub body: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocSearchResult {
    #[serde(flatten)]
    pub record: DocSearchSource,
    pub score: usize,
    pub matched_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum SearchResult {
    Doc(DocSearchResult),
}

pub fn score_doc_record(record: DocSearchSource, query_lower: &str) -> Option<DocSearchResult> {
    let mut score = 0usize;
    let mut matched_by = Vec::new();
    let summary = record.summary.to_ascii_lowercase();
    if summary.contains(query_lower) {
        score += 80 + query_lower.len();
        matched_by.push("summary".to_string());
    }
    if record.doc_type.contains(query_lower) {
        score += 30;
        matched_by.push(format!("type:{}", record.doc_type));
    }
    for tag in &record.tags {
        let lower = tag.to_ascii_lowercase();
        if lower == query_lower {
            score += 120;
            matched_by.push(format!("tag:{tag}"));
        } else if lower.contains(query_lower) {
            score += 60;
            matched_by.push(format!("tag:{tag}"));
        }
    }
    // Scan the body in place: lowercasing it first copies every doc in the
    // corpus once per query, and the copy was only ever read for this find.
    let snippet = find_ignore_ascii_case(&record.body, query_lower).map(|offset| {
        let mut start = offset.saturating_sub(80);
        while start > 0 && !record.body.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (offset + query_lower.len() + 120).min(record.body.len());
        while end < record.body.len() && !record.body.is_char_boundary(end) {
            end += 1;
        }
        record.body[start..end].trim().replace('\n', " ")
    });
    if snippet.is_some() {
        score += 50 + query_lower.len();
        matched_by.push("body".to_string());
    }
    if score == 0 {
        return None;
    }
    Some(DocSearchResult {
        record,
        score,
        matched_by,
        snippet,
    })
}

/// Byte offset of the first occurrence of `needle_lower` in `haystack`,
/// comparing ASCII letters case-insensitively — what
/// `haystack.to_ascii_lowercase().find(needle_lower)` returns, without the
/// copy. `needle_lower` is already lowercased by the caller.
///
/// The offset is a char boundary of `haystack`: a match starts where the
/// needle's first byte does, and that byte is never a UTF-8 continuation.
// widened for tests per the sibling layout; see test_layout.md
pub(crate) fn find_ignore_ascii_case(haystack: &str, needle_lower: &str) -> Option<usize> {
    let needle = needle_lower.as_bytes();
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

pub fn sort_search_results(results: &mut [SearchResult]) {
    results.sort_by(|left, right| {
        search_result_score(right)
            .cmp(&search_result_score(left))
            .then_with(|| match (left, right) {
                (SearchResult::Doc(left), SearchResult::Doc(right)) => {
                    left.record.path.cmp(&right.record.path)
                }
            })
    });
}

fn search_result_score(result: &SearchResult) -> usize {
    match result {
        SearchResult::Doc(result) => result.score,
    }
}
