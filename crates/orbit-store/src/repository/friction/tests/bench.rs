//! Repeatable scan-memory benchmark for ORB-10680.
//!
//! Ignored by default; `scripts/bench-friction-scan.sh` runs it. Both arms see
//! the same generated corpus in the same process: the baseline replays the
//! retired file scan (discover every record, parse every envelope and body,
//! collect, then filter/sort/paginate), and the candidate issues the same two
//! requests against SQLite.
//!
//! Peak RSS comes from `/proc/self/status` `VmHWM`, sampled around each arm.
//! The candidate runs first so the baseline's high-water mark is attributable
//! to the baseline rather than inherited from it.

use std::path::Path;
use std::time::Instant;

use orbit_types::record::{FrictionRecord, FrictionStatus};

use super::super::{FrictionListFilter, FrictionStore};
use super::support::{at, store};
use crate::driver::file::friction_store::{friction_record_paths, read_record_at, write_record_at};

/// Corpus size, overridable with `ORBIT_FRICTION_BENCH_N`.
const DEFAULT_CORPUS: usize = 5_000;
/// The page `orbit friction list --status open --limit 50` asks for.
const PAGE: usize = 50;

#[test]
#[ignore = "benchmark; run through scripts/bench-friction-scan.sh"]
#[expect(
    clippy::print_stdout,
    reason = "benchmark harness output is the deliverable"
)]
fn bench_friction_scan_baseline_versus_candidate() {
    let corpus: usize = std::env::var("ORBIT_FRICTION_BENCH_N")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_CORPUS);
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_bench");
    generate_corpus(&source, corpus);

    println!("corpus: {corpus} records at {}", source.display());
    println!("baseline peak RSS after generation: {} kB", peak_rss_kb());

    let frictions = FrictionStore::open(store(temp.path()), "ws_bench", &source)
        .expect("open and import friction store");

    let candidate_list = measure(|| {
        frictions
            .list(&FrictionListFilter {
                status: Some(FrictionStatus::Open),
                limit: Some(PAGE),
                ..FrictionListFilter::default()
            })
            .expect("candidate list")
            .len()
    });
    let candidate_stats = measure(|| {
        frictions.stats(&[]).expect("candidate stats")["total"]
            .as_u64()
            .unwrap_or_default() as usize
    });

    let baseline_list = measure(|| baseline_list_open_page(&source));
    let baseline_stats = measure(|| baseline_stats(&source));

    report(
        "candidate friction list --status open --limit 50",
        &candidate_list,
    );
    report("candidate friction stats", &candidate_stats);
    report(
        "baseline  friction list --status open --limit 50",
        &baseline_list,
    );
    report("baseline  friction stats", &baseline_stats);

    assert_eq!(candidate_list.result, PAGE.min(corpus));
    assert_eq!(baseline_list.result, PAGE.min(corpus));
    assert_eq!(candidate_stats.result, corpus);
    assert_eq!(baseline_stats.result, corpus);
}

struct Measurement {
    millis: u128,
    rss_before_kb: u64,
    rss_after_kb: u64,
    result: usize,
}

fn measure(op: impl FnOnce() -> usize) -> Measurement {
    let rss_before_kb = peak_rss_kb();
    let started = Instant::now();
    let result = op();
    Measurement {
        millis: started.elapsed().as_millis(),
        rss_before_kb,
        rss_after_kb: peak_rss_kb(),
        result,
    }
}

#[expect(
    clippy::print_stdout,
    reason = "benchmark harness output is the deliverable"
)]
fn report(label: &str, measurement: &Measurement) {
    println!(
        "{label}: {} ms, peak RSS {} kB -> {} kB (+{} kB)",
        measurement.millis,
        measurement.rss_before_kb,
        measurement.rss_after_kb,
        measurement
            .rss_after_kb
            .saturating_sub(measurement.rss_before_kb),
    );
}

/// The retired read path, kept here verbatim so the comparison is honest.
fn baseline_list_open_page(root: &Path) -> usize {
    let mut records = Vec::new();
    for path in friction_record_paths(root).expect("walk corpus") {
        let stored = read_record_at(&path).expect("parse record");
        if stored.record.status == FrictionStatus::Open {
            records.push(stored);
        }
    }
    records.sort_by(|left, right| {
        left.record
            .created_at
            .cmp(&right.record.created_at)
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
    records.into_iter().take(PAGE).count()
}

fn baseline_stats(root: &Path) -> usize {
    let mut records = Vec::new();
    for path in friction_record_paths(root).expect("walk corpus") {
        records.push(read_record_at(&path).expect("parse record"));
    }
    records.len()
}

fn generate_corpus(root: &Path, count: usize) {
    for index in 0..count {
        let month = 1 + (index / 900) as u32;
        let seq = 1 + (index % 900) as u32;
        let id = format!("F2026-{month:02}-{seq:03}");
        let record = FrictionRecord {
            id: id.clone(),
            title: Some(format!("Generated friction {index}")),
            model: if index % 2 == 0 { "codex" } else { "claude" }.to_string(),
            created_at: at(1 + (index % 27) as u32, (index % 24) as u32),
            status: if index % 4 == 0 {
                FrictionStatus::Resolved
            } else {
                FrictionStatus::Open
            },
            tags: vec!["tooling".to_string()],
            resolved_at: None,
            during_task: None,
            resolved_by_task: None,
            rehome_to: None,
            // A realistic report body: the parse cost the scan paid per record.
            body: format!(
                "## What happened\n\nGenerated report {index}.\n\n## Evidence\n\n{}\n",
                "log line filler ".repeat(40)
            ),
        };
        write_record_at(
            &root
                .join(format!("2026-{month:02}"))
                .join(format!("F{seq:03}.md")),
            &record,
        )
        .expect("write generated record");
    }
}

/// Process high-water RSS in kB, or 0 where `/proc` is unavailable.
fn peak_rss_kb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}
