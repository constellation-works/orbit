//! Which handover probe answers a long-lived process remembers.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use super::super::GENERATION_CONTRACT;
use super::super::handoff::{RESUME_MCP_STDIO, judge_candidate};

const TIMEOUT: Duration = Duration::from_millis(300);

/// Write an executable shell script. The returned path is stable across later
/// calls that leave the script alone, so its cache key does not change.
fn install(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn contract(admission: &str, resume: &str) -> String {
    format!(r#"echo '{{"admission_contract":"{admission}","resume":[{resume}]}}'"#)
}

fn runs(counter: &Path) -> usize {
    std::fs::read_to_string(counter).map_or(0, |log| log.lines().count())
}

/// A timed-out probe says nothing about the candidate: the next idle boundary
/// asks again and, once the candidate answers, hands over.
#[test]
fn a_probe_that_times_out_is_retried_at_the_next_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    let ready = dir.path().join("ready");
    // The same script and so the same cache key throughout: the first exec is
    // held (it sleeps past the timeout), a later one answers.
    let body = format!(
        "echo run >> '{counter}'\nif [ ! -e '{ready}' ]; then exec sleep 30; fi\n{answer}",
        counter = counter.display(),
        ready = ready.display(),
        answer = contract(GENERATION_CONTRACT, &format!(r#""{RESUME_MCP_STDIO}""#)),
    );
    let candidate = install(dir.path(), "orbit", &body);
    let refused = Mutex::new(None);

    assert_eq!(
        judge_candidate(&refused, candidate.clone(), Some(RESUME_MCP_STDIO), TIMEOUT),
        None
    );
    assert!(refused.lock().unwrap().is_none(), "a timeout was cached");

    std::fs::write(&ready, "").expect("ready");
    assert_eq!(
        judge_candidate(&refused, candidate.clone(), Some(RESUME_MCP_STDIO), TIMEOUT),
        Some(candidate)
    );
    assert_eq!(runs(&counter), 2, "the candidate was not probed again");
}

/// A completed refusal — wrong contract, missing resume capability, or a
/// binary that predates `--contract` and exits non-zero — is not re-probed
/// until the installation changes.
#[test]
fn a_completed_refusal_is_not_probed_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    let log = format!("echo run >> '{}'\n", counter.display());
    let cases = [
        (
            "wrong-contract",
            contract(
                "compatibility-generation-v0",
                &format!(r#""{RESUME_MCP_STDIO}""#),
            ),
        ),
        (
            "no-capability",
            contract(GENERATION_CONTRACT, r#""drain-adopt-v1""#),
        ),
        ("old-binary", "exit 2".to_string()),
    ];
    for (name, answer) in cases {
        std::fs::write(&counter, "").expect("reset counter");
        let candidate = install(dir.path(), name, &format!("{log}{answer}"));
        let refused = Mutex::new(None);
        for _ in 0..3 {
            assert_eq!(
                judge_candidate(&refused, candidate.clone(), Some(RESUME_MCP_STDIO), TIMEOUT),
                None,
                "{name}"
            );
        }
        assert_eq!(runs(&counter), 1, "{name} was probed again");
    }
}
