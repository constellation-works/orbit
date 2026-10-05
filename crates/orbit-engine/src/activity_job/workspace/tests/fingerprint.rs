use std::time::Instant;

use super::super::fingerprint::{diff_chunk_starts, diff_chunks};

#[test]
fn diff_chunk_starts_supported_markers_and_line_boundaries() {
    let diff = concat!(
        "diff --git a/foo.rs b/foo.rs\n",
        "--- a/foo.rs\n",
        "+++ b/foo.rs\n",
        "@@ -1 +1 @@\n",
        "-old\n",
        "+new\n",
        "diff --cc conflict.rs\n",
        "index 1234..5678\n",
        "--- a/conflict.rs\n",
        "+++ b/conflict.rs\n",
        "diff --combined combined.rs\n",
        "index 1234..5678\n",
    );

    let starts = diff_chunk_starts(diff.as_bytes());
    assert_eq!(
        starts.len(),
        3,
        "ORB-14109: diff_chunk_starts must detect all three supported markers at line starts"
    );
    assert_eq!(starts[0], 0);

    let chunks = diff_chunks(diff.as_bytes());
    assert_eq!(chunks.len(), 3);
    assert!(chunks[0].starts_with(b"diff --git a/foo.rs"));
    assert!(chunks[1].starts_with(b"diff --cc conflict.rs"));
    assert!(chunks[2].starts_with(b"diff --combined combined.rs"));
}

#[test]
fn diff_chunk_starts_crlf_and_boundary_rules() {
    // CRLF line endings
    let crlf_diff = "diff --git a/a b/a\r\n+1\r\ndiff --git a/b b/b\r\n+2\r\n";
    let starts = diff_chunk_starts(crlf_diff.as_bytes());
    assert_eq!(
        starts.len(),
        2,
        "ORB-14109: CRLF diffs must be split accurately"
    );
    let chunks = diff_chunks(crlf_diff.as_bytes());
    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].starts_with(b"diff --git a/a b/a\r\n"));
    assert!(chunks[1].starts_with(b"diff --git a/b b/b\r\n"));

    // Leading noise before first diff header
    let leading_noise = "commit header\nauthor info\n\ndiff --git a/file b/file\n+content\n";
    let starts = diff_chunk_starts(leading_noise.as_bytes());
    assert_eq!(starts.len(), 1);
    assert_eq!(
        &leading_noise.as_bytes()[starts[0]..starts[0] + 10],
        b"diff --git"
    );
}

#[test]
fn diff_chunk_starts_ignores_non_header_diff_lines() {
    let diff = concat!(
        "diff --git a/file.rs b/file.rs\n",
        "@@ -1,5 +1,5 @@\n",
        "+diff --git a/false_positive b/false_positive\n",
        "-diff --cc fake\n",
        " diff --combined fake\n",
        " text with diff --git embedded in middle of line\n",
        "diff not-a-valid-marker\n",
        "diff --other-unsupported-flag\n",
    );

    let starts = diff_chunk_starts(diff.as_bytes());
    assert_eq!(
        starts,
        vec![0],
        "ORB-14109: diff_chunk_starts must only match recognized markers at line starts"
    );
}

#[test]
fn diff_chunk_starts_empty_and_short_inputs() {
    assert_eq!(diff_chunk_starts(b""), Vec::<usize>::new());
    assert_eq!(diff_chunk_starts(b"d"), Vec::<usize>::new());
    assert_eq!(diff_chunk_starts(b"diff"), Vec::<usize>::new());
    assert_eq!(diff_chunk_starts(b"diff "), Vec::<usize>::new());
    assert_eq!(diff_chunk_starts(b"diff --"), Vec::<usize>::new());
    assert_eq!(diff_chunks(b""), Vec::<&[u8]>::new());
}

#[test]
fn diff_chunk_starts_scales_linearly_on_many_file_diff() {
    // Construct a large diff (5,000 files, ~2.5 MB) to verify linear scaling
    // guards against the O(chunks * diff size) regression identified in ORB-14109.
    let count = 5_000;
    let mut diff = Vec::with_capacity(3 * 1024 * 1024);
    for i in 0..count {
        diff.extend_from_slice(format!("diff --git a/file_{i}.rs b/file_{i}.rs\nindex 1234..5678 100644\n--- a/file_{i}.rs\n+++ b/file_{i}.rs\n@@ -1 +1 @@\n-old line {i}\n+new line {i}\n").as_bytes());
    }

    let start = Instant::now();
    let starts = diff_chunk_starts(&diff);
    let elapsed = start.elapsed();

    assert_eq!(
        starts.len(),
        count,
        "ORB-14109: all {count} chunk starts must be identified"
    );

    // The quadratic implementation took minutes on this workload.
    // The linear scan completes in a few milliseconds; guard well under 1 second.
    assert!(
        elapsed.as_millis() < 1000,
        "ORB-14109: 5,000-chunk diff splitting must complete in linear time (< 1s), took {elapsed:?}"
    );
}
