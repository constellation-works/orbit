//! Bounded reader evidence for both required-validation delivery routes.
//! Classification always reads the complete supervised streams separately.

use std::ops::Range;

use orbit_common::text::{ceil_char_boundary, floor_char_boundary};

use super::super::base::claim::normalize_output;

/// Source bytes per command, shared by stdout and stderr. Ordinary head/tail
/// notices sit outside the count; causal-region notices fit within it.
const MAX_CAPTURED_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_FAILURE_BLOCKS: usize = 128;
const MAX_FAILURE_BLOCK_BYTES: usize = 16 * 1024;
const MAX_FAILURE_BYTES: usize = 64 * 1024;

/// Join what the command wrote, bounded per stream.
///
/// Each non-empty stream gets a share of [`MAX_CAPTURED_OUTPUT_BYTES`]. A
/// stream that fits in its share is kept whole, so a short stderr survives a
/// stdout that would otherwise fill the cap. A stream over its share keeps a
/// head, a tail and fair shares of nextest failure blocks, with explicit
/// omission markers. Stderr stays after stdout, preserving tail diagnostics
/// and the existing short-stderr allocation [ORB-14086].
pub(in crate::executor::automation::vcs) fn capture(stdout: &str, stderr: &str) -> String {
    let stdout = trimmed_stream(stdout);
    let stderr = trimmed_stream(stderr);
    let separator = usize::from(!stdout.is_empty() && !stderr.is_empty());
    if stdout.len() + stderr.len() + separator <= MAX_CAPTURED_OUTPUT_BYTES {
        return join_streams(stdout, stderr);
    }
    let (stdout_budget, stderr_budget) = stream_budgets(stdout.len(), stderr.len());
    join_streams(
        &bound_stream(stdout, stdout_budget, "stdout"),
        &bound_stream(stderr, stderr_budget, "stderr"),
    )
}

/// `text` with trailing whitespace removed, or empty when it is only whitespace.
fn trimmed_stream(text: &str) -> &str {
    if text.trim().is_empty() {
        ""
    } else {
        text.trim_end()
    }
}

/// Byte budgets for the two streams. Their kept content, plus one separator
/// when both are non-empty, stays within [`MAX_CAPTURED_OUTPUT_BYTES`].
/// Truncation notices sit outside that count.
fn stream_budgets(stdout_len: usize, stderr_len: usize) -> (usize, usize) {
    let separator = usize::from(stdout_len > 0 && stderr_len > 0);
    let max = MAX_CAPTURED_OUTPUT_BYTES.saturating_sub(separator);
    if stderr_len == 0 {
        return (max.min(stdout_len), 0);
    }
    if stdout_len == 0 {
        return (0, max.min(stderr_len));
    }
    let half = max / 2;
    if stderr_len <= half {
        (max - stderr_len, stderr_len)
    } else if stdout_len <= half {
        (stdout_len, max - stdout_len)
    } else {
        let stderr_budget = half;
        (max - stderr_budget, stderr_budget)
    }
}

/// Keep `text` when it fits in `budget`. Otherwise keep a head and a tail
/// plus causal blocks whose content and omission notices fit the budget.
fn bound_stream(text: &str, budget: usize, stream: &str) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let (blocks, anchors) = failure_blocks(text);
    if !blocks.is_empty() {
        return bound_failure_stream(text, budget, stream, &blocks, anchors);
    }
    let tail_budget = budget / 2;
    let head_budget = budget - tail_budget;
    let head_end = floor_char_boundary(text, head_budget);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail_budget));
    if tail_start <= head_end {
        let end = floor_char_boundary(text, budget.min(text.len()));
        return text[..end].to_string();
    }
    let head = &text[..head_end];
    let tail = &text[tail_start..];
    let omitted = tail_start - head_end;
    let head_len = head.len();
    let tail_len = tail.len();
    let total = text.len();
    let mut bounded = String::with_capacity(head_len + tail_len + 64);
    if !head.is_empty() {
        bounded.push_str(head);
        bounded.push('\n');
    }
    bounded.push_str(&format!(
        "[{stream} truncated: kept {head_len}+{tail_len} of {total} bytes, {omitted} omitted]"
    ));
    if !tail.is_empty() {
        bounded.push('\n');
        bounded.push_str(tail);
    }
    bounded
}

fn join_streams(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => {
            let mut combined = String::with_capacity(stdout.len() + stderr.len() + 1);
            combined.push_str(stdout);
            combined.push('\n');
            combined.push_str(stderr);
            combined
        }
    }
}

/// Scan the complete stream but retain only bounded range metadata. Color and
/// indentation are ignored for status recognition; original bytes are kept.
/// Captured stdout/stderr sections remain part of the block until the next
/// test status, final rule or summary, as in the GitHub logs collector.
fn failure_blocks(text: &str) -> (Vec<Range<usize>>, usize) {
    let mut blocks = Vec::new();
    let mut open = None;
    let mut anchors = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let plain = normalize_output(line);
        let status = nextest_status(&plain);
        let boundary = status.is_some()
            || plain.starts_with("Summary ")
            || (plain.len() >= 24 && plain.chars().all(|ch| ch == '─'));
        if boundary && let Some(start) = open.take() {
            blocks.push(start..offset);
        }
        if status.is_some_and(|status| {
            matches!(
                status,
                "FAIL" | "TIMEOUT" | "SIGSEGV" | "SIGABRT" | "SIGKILL" | "SIGBUS" | "LEAK-FAIL"
            )
        }) {
            anchors += 1;
            if blocks.len() < MAX_FAILURE_BLOCKS {
                open = Some(offset);
            }
        }
        offset += line.len();
    }
    if let Some(start) = open {
        blocks.push(start..offset);
    }
    (blocks, anchors)
}

fn nextest_status(line: &str) -> Option<&str> {
    let (status, rest) = line.split_once(" [")?;
    let (duration, _) = rest.split_once(']')?;
    (!status.is_empty()
        && status.len() <= 24
        && status
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || matches!(ch, ' ' | '+' | '-'))
        && duration.contains('s')
        && duration
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ' ' | 's')))
    .then_some(status)
}

fn bound_failure_stream(
    text: &str,
    budget: usize,
    stream: &str,
    blocks: &[Range<usize>],
    anchors: usize,
) -> String {
    // Reserve enough for all gap notices and the anchor-count disclosure.
    // Fair shares prevent a huge early failure crowding out later failures.
    let notice_budget = 96 * (2 * blocks.len() + 2);
    let source_budget = budget.saturating_sub(notice_budget);
    let share = (source_budget / 4).min(MAX_FAILURE_BYTES) / blocks.len();
    let share = share.min(MAX_FAILURE_BLOCK_BYTES);
    let mut ranges = Vec::with_capacity(2 * blocks.len() + 2);
    let mut failure_bytes = 0;
    for block in blocks {
        if block.len() <= share {
            ranges.push(block.clone());
            failure_bytes += block.len();
        } else {
            // An oversized block keeps its anchor/scenario and its final
            // diagnostics, with the same gap disclosure as the outer stream.
            let body = &text[block.clone()];
            let tail_budget = share / 4;
            let head = floor_char_boundary(body, share - tail_budget);
            let tail = ceil_char_boundary(body, body.len().saturating_sub(tail_budget));
            ranges.push(block.start..block.start + head);
            ranges.push(block.start + tail..block.end);
            failure_bytes += head + body.len() - tail;
        }
    }
    let remaining = source_budget.saturating_sub(failure_bytes);
    ranges.push(0..floor_char_boundary(text, remaining - remaining / 2));
    ranges.push(ceil_char_boundary(text, text.len().saturating_sub(remaining / 2))..text.len());
    ranges.sort_unstable_by_key(|range| range.start);
    let mut bounded = String::with_capacity(budget);
    let mut offset = 0;
    let mut count_disclosed = false;
    for range in ranges {
        if range.start > offset {
            if anchors > blocks.len() && !count_disclosed {
                bounded.push_str(&format!(
                    "\n[{stream} truncated: selected {} of {anchors} nextest failure blocks]",
                    blocks.len()
                ));
                count_disclosed = true;
            }
            bounded.push_str(&format!(
                "\n[{stream} truncated: {} bytes omitted]\n",
                range.start - offset
            ));
        }
        if range.end > offset {
            bounded.push_str(&text[offset.max(range.start)..range.end]);
            offset = range.end;
        }
    }
    bounded
}
