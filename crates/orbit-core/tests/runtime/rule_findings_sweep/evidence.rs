//! Evidence selection through the public filer, including payload-copy work.
use super::{document, fixture, result};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;

thread_local! {
    // Count only allocations with the distinctive binary payload length.
    // Unrelated test threads and JSON/string allocations do not enter the count.
    static PAYLOAD_ALLOCATIONS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

struct EvidenceCountingAllocator;

fn record_evidence_allocation(size: usize) {
    let _ = PAYLOAD_ALLOCATIONS.try_with(|counter| {
        if let Some((payload_size, count)) = counter.get()
            && size == payload_size
        {
            counter.set(Some((payload_size, count + 1)));
        }
    });
}

// SAFETY: All operations delegate unchanged to System. The thread-local
// counter is allocation-free and never reads or modifies allocated memory.
unsafe impl GlobalAlloc for EvidenceCountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_evidence_allocation(layout.size());
        // SAFETY: System receives the caller's layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_evidence_allocation(layout.size());
        // SAFETY: System receives the caller's layout unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: System receives its original pointer and layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_evidence_allocation(size);
        // SAFETY: System receives the pointer, original layout and requested size unchanged.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static EVIDENCE_ALLOCATOR: EvidenceCountingAllocator = EvidenceCountingAllocator;

pub(super) fn evidence_payload_allocations_ignore_candidate_multiplicity() {
    writeln!(
        std::io::stderr().lock(),
        "public filer measurement source: {}",
        env!("CARGO_MANIFEST_DIR")
    )
    .expect("allocation measurement source");
    // Two small non-power-of-two lengths distinguish payload clones from
    // allocator growth and unrelated buffers. No timing or RSS assertion.
    for payload_size in [4093, 8191] {
        let mut counts = Vec::new();
        for (copies, extra_candidates) in [(1, false), (4, false), (4, true)] {
            let (root, runtime) = fixture("[ux_rules]\nmax_new_tasks = 2\nfile_at = \"low\"\n");
            let repo = root.path().join("repo");
            let bytes: Vec<_> = (0..payload_size).map(|n| (n % 256) as u8).collect();
            std::fs::write(repo.join("sample.bin"), &bytes).unwrap();
            let mut entries = vec![
                result("fail", "critical", "dup", json!({"winner": true})),
                result("error", "high", "err1", json!({"target": 1})),
                result("fail", "medium", "over", json!({})),
            ];
            if extra_candidates {
                // Lower severity and tied duplicates lose to the first winner;
                // extra error members aggregate, and new medium findings exceed the cap.
                entries.extend([
                    result("fail", "low", "dup", json!({"winner": false})),
                    result("fail", "critical", "dup", json!({"winner": false})),
                    result("error", "high", "err2", json!({"target": 2})),
                    result("fail", "medium", "over2", json!({})),
                ]);
            }
            for entry in &mut entries {
                entry["evidence"] = json!(vec!["sample.bin"; copies]);
            }
            let path = repo.join("results.json");
            std::fs::write(&path, serde_json::to_vec(&document(entries)).unwrap()).unwrap();
            let input = json!({"results_file": path});
            let params = json!({});
            PAYLOAD_ALLOCATIONS.with(|counter| counter.set(Some((payload_size, 0))));
            let outcome = runtime.run_deterministic(
                "file_rule_findings",
                &params,
                &input,
                ToolContext::default(),
            );
            let count = PAYLOAD_ALLOCATIONS.with(|counter| counter.take().unwrap().1);
            let outcome = outcome.unwrap();
            assert_eq!(outcome["filed_count"], 2, "{outcome}");
            assert_eq!(outcome["filed"][0]["fingerprint"], "dup");
            assert_eq!(outcome["filed"][1]["fingerprint"], "error|cli.example");
            for filed in outcome["filed"].as_array().unwrap() {
                let id = filed["task_id"].as_str().unwrap();
                assert_eq!(runtime.get_task_artifact_manifest(id).unwrap().len(), 1);
                assert_eq!(
                    runtime
                        .get_task_artifact(id, "sample.bin")
                        .unwrap()
                        .unwrap()
                        .content,
                    bytes
                );
            }
            counts.push(count);
        }
        writeln!(
            std::io::stderr().lock(),
            "public filer payload-size={payload_size} allocations: single={}, repeated={}, expanded={}",
            counts[0], counts[1], counts[2]
        ).expect("allocation measurement counts");
        assert!(
            counts[0] > 0,
            "measurement must observe owned payload buffers"
        );
        assert_eq!(
            counts[1], counts[0],
            "repeated evidence must not multiply payload copies before selection"
        );
        assert_eq!(
            counts[2], counts[0],
            "duplicate fingerprints, error members and over-cap findings must not multiply payload copies before selection"
        );
    }
}

pub(super) fn repeated_evidence_is_attached_once_after_selection() {
    let (root, runtime) = fixture("[ux_rules]\nmax_new_tasks = 2\nfile_at = \"low\"\n");
    let repo = root.path().join("repo");
    let bytes = [0, 255, 7, 0, 128];
    std::fs::write(repo.join("sample.bin"), bytes).unwrap();
    let mut low = result("fail", "low", "dup", json!({"winner": false}));
    let mut high = result("fail", "critical", "dup", json!({"winner": true}));
    let mut tied = result("fail", "critical", "dup", json!({"winner": false}));
    let mut error = result("error", "high", "err1", json!({"target": 1}));
    let mut error2 = result("error", "high", "err2", json!({"target": 2}));
    let mut over = result("fail", "medium", "over", json!({}));
    for entry in [
        &mut low,
        &mut high,
        &mut tied,
        &mut error,
        &mut error2,
        &mut over,
    ] {
        entry["evidence"] = json!(["sample.bin", "sample.bin", "sample.bin", "sample.bin"]);
    }
    let doc = document(vec![low, high, tied, error, error2, over]);
    let path = repo.join("results.json");
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let input = json!({"results_file": path});
    let run = || {
        runtime
            .run_deterministic(
                "file_rule_findings",
                &json!({}),
                &input,
                ToolContext::default(),
            )
            .unwrap()
    };
    // Invalid losing and over-cap evidence both refuse before filing.
    for index in [0, 5] {
        let mut invalid = doc.clone();
        invalid["results"][index]["evidence"] = json!(["missing.bin"]);
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            runtime
                .run_deterministic(
                    "file_rule_findings",
                    &json!({}),
                    &input,
                    ToolContext::default(),
                )
                .is_err()
        );
        assert!(
            runtime
                .list_tasks_by_tags(&["ux-rules".to_string()])
                .unwrap()
                .is_empty()
        );
    }
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let outcome = run();
    assert_eq!(outcome["filed_count"], 2, "{outcome}");
    assert_eq!(outcome["filed"][0]["fingerprint"], "dup");
    assert_eq!(outcome["filed"][1]["fingerprint"], "error|cli.example");
    assert_eq!(outcome["skipped_over_cap"].as_array().unwrap().len(), 1);
    for entry in outcome["filed"].as_array().unwrap() {
        let id = entry["task_id"].as_str().unwrap();
        let task = runtime.get_task(id).unwrap();
        if entry["error_task"] == true {
            assert!(task.description.contains("2 target(s)"));
        } else {
            assert!(task.description.contains("\"winner\":true"));
        }
        let manifest = runtime.get_task_artifact_manifest(id).unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(
            manifest[0].sha256,
            "fb2063645fe3a979832380b671be9f118202efc6f23ff9f7485caaffe7b04fe8"
        );
        assert_eq!(
            runtime
                .get_task_artifact(id, "sample.bin")
                .unwrap()
                .unwrap()
                .content,
            bytes
        );
    }
    // Validation also precedes updates to existing owners, including the
    // pass loop which otherwise comments before candidate selection.
    for index in [0, 5] {
        let tasks_before = runtime
            .list_tasks_by_tags(&["ux-rules".to_string()])
            .unwrap();
        let comments_before: Vec<_> = tasks_before
            .iter()
            .map(|task| runtime.get_task_comments(&task.id).unwrap().len())
            .collect();
        let mut invalid = doc.clone();
        invalid["results"][index]["evidence"] = json!(["missing.bin"]);
        invalid["results"][1]["status"] = json!("pass");
        invalid["results"][3]["measurements"] = json!({"target": "changed"});
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            runtime
                .run_deterministic(
                    "file_rule_findings",
                    &json!({}),
                    &input,
                    ToolContext::default(),
                )
                .is_err()
        );
        assert_eq!(
            runtime
                .list_tasks_by_tags(&["ux-rules".to_string()])
                .unwrap()
                .len(),
            tasks_before.len()
        );
        for (task, comment_count) in tasks_before.iter().zip(comments_before) {
            assert_eq!(
                runtime.get_task_comments(&task.id).unwrap().len(),
                comment_count,
                "invalid losing/over-cap evidence must refuse before comments"
            );
        }
    }
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let again = run();
    // The former over-cap finding is eligible on a subsequent run.
    assert_eq!(again["filed_count"], 1, "{again}");
    assert_eq!(run()["filed_count"], 0);
}
