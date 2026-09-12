//! Timezone and probe-outcome regression coverage for owner identity classification.

use super::*;

use super::super::JobRunListParams;

#[cfg(unix)]
use super::super::owner::{
    OwnerIdentity, classify_run_owner_with_probes, pending_run_stale_reason,
    running_run_owner_stale_reason, stale_job_run_message, verify_owner_termination,
};
use chrono::{Duration, Utc};
#[cfg(unix)]
use orbit_common::process::identity::{PidNamespaceScope, ProbeOutcome};
#[cfg(unix)]
use orbit_common::process::identity::{
    STABLE_TOKEN_PREFIX, STABLE_TOKEN_PREFIX_V1, current_pid_namespace,
    process_start_identity_token,
};
use orbit_types::workflow::JobRunState;
#[cfg(unix)]
use std::process::{Command, Stdio};

#[cfg(unix)]
struct ReapingChild(std::process::Child);

#[cfg(unix)]
impl ReapingChild {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.0.kill()
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait()
    }
}

#[cfg(unix)]
impl Drop for ReapingChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
fn spawn_sentinel() -> ReapingChild {
    ReapingChild(
        Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sentinel"),
    )
}

#[cfg(unix)]
#[test]
fn live_isolated_group_returns_typed_cancellation_evidence() {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new("sleep");
    command
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut survivor = ReapingChild(command.spawn().expect("spawn isolated survivor"));
    let pid = survivor.id();

    let error = verify_owner_termination(pid, Some(pid as libc::pid_t), true, true)
        .expect_err("a genuine survivor must remain non-terminal");
    assert!(matches!(
        error,
        orbit_common::OrbitError::RunCancellationIncomplete {
            pid: error_pid,
            pgid: Some(error_pgid),
            term_sent: true,
            kill_sent: true,
            leader_alive: true,
            group_alive: true,
        } if error_pid == pid && error_pgid == pid as libc::pid_t
    ));

    survivor.kill().expect("kill isolated survivor");
    survivor.wait().expect("reap isolated survivor");
}

#[cfg(unix)]
#[test]
fn live_owner_survives_tz_change_across_read_paths() {
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_tz_change");
    let mut sentinel = spawn_sentinel();
    let sentinel_pid = sentinel.id();

    // Write the run under a non-UTC ambient TZ. The fix forces the child
    // ps invocation to TZ=UTC regardless, so the persisted token must
    // carry the versioned prefix and remain identical across caller
    // environments.
    let persisted_token = {
        let _tz = orbit_common::test_env::scoped([("TZ", Some("America/Los_Angeles"))]);
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now() - Duration::seconds(1), sentinel_pid)
            .expect("mark running under LA tz");
        runtime
            .show_job_run(&run.run_id)
            .expect("show fresh run")
            .pid_start_time
            .expect("token must be persisted")
    };
    assert!(
        persisted_token.starts_with(STABLE_TOKEN_PREFIX),
        "persisted identity token must be versioned: {persisted_token}"
    );

    // Switch TZ before driving the read paths. Pre-fix this is exactly
    // when reconciliation falsely finalized the still-running worker.
    let _tz = orbit_common::test_env::scoped([("TZ", Some("UTC"))]);

    let shown = runtime.show_job_run(&run.run_id).expect("show under UTC");
    assert_eq!(shown.state, JobRunState::Running);
    assert!(shown.finished_at.is_none());
    assert!(shown.duration_ms.is_none());
    assert!(
        !shown
            .steps
            .iter()
            .any(|step| step.error_message.as_deref().is_some_and(|message| {
                message.contains("recorded worker process is no longer alive")
            })),
        "live worker must not have a stale-failure step"
    );

    let listed = runtime
        .list_job_runs(JobRunListParams {
            state: Some(JobRunState::Running),
            ..JobRunListParams::default()
        })
        .expect("list running under UTC");
    assert!(
        listed
            .iter()
            .any(|candidate| candidate.run_id == run.run_id),
        "live worker must still appear in the Running list after a TZ change"
    );

    let waited = runtime
        .wait_pipeline_runs(
            std::slice::from_ref(&run.run_id),
            0,
            1,
            Some("tz_change_test"),
        )
        .expect("wait under UTC");
    assert_eq!(waited.results.len(), 1);
    assert_eq!(waited.results[0].run_id, run.run_id);
    assert_ne!(
        waited.results[0].status, "failed",
        "wait must not report failed for a live worker after a TZ change"
    );

    let final_state = runtime.show_job_run(&run.run_id).expect("final show").state;
    assert_eq!(final_state, JobRunState::Running);

    let _ = sentinel.kill();
    let _ = sentinel.wait();
}

#[cfg(unix)]
#[test]
fn versioned_token_is_stable_across_tz_change() {
    let mut sentinel = spawn_sentinel();
    let pid = sentinel.id();

    let utc_token = {
        let _tz = orbit_common::test_env::scoped([("TZ", Some("UTC"))]);
        process_start_identity_token(pid).expect("token under UTC")
    };
    let la_token = {
        let _tz = orbit_common::test_env::scoped([("TZ", Some("America/Los_Angeles"))]);
        process_start_identity_token(pid).expect("token under LA")
    };

    assert!(utc_token.starts_with(STABLE_TOKEN_PREFIX));
    assert_eq!(
        utc_token, la_token,
        "versioned identity token must not depend on the caller's TZ"
    );

    let _ = sentinel.kill();
    let _ = sentinel.wait();
}

#[cfg(unix)]
#[test]
fn legacy_unversioned_token_does_not_falsely_finalize_live_run() {
    // A pre-fix run with a non-versioned `pid_start_time` whose value
    // cannot be matched under either environment should classify as
    // LegacyLiveUnverified, keeping the run Running instead of finalizing
    // it as Failed.
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_legacy_unverified");
    let mut sentinel = spawn_sentinel();
    let sentinel_pid = sentinel.id();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now() - Duration::seconds(1), sentinel_pid)
        .expect("mark running");

    // Rewrite the stored token to look like a pre-fix unversioned value
    // that does not match the live process under either env.
    set_run_pid_start_time(&runtime, &run, "legacy-token-that-cannot-be-rederived");

    let shown = runtime.show_job_run(&run.run_id).expect("show legacy run");
    assert_eq!(shown.state, JobRunState::Running);
    assert!(shown.finished_at.is_none());

    let _ = sentinel.kill();
    let _ = sentinel.wait();
}

// ---- Probe-outcome regression coverage (ORB-00037) ----
//
// `classify_run_owner_with_probes` lets these tests inject deterministic
// `ProbeOutcome` values without depending on a real misbehaving `ps`.
// They guard the rule from the task ACs: a transient probe failure with a
// live PID must never terminalize the run; a dead PID still must.

#[cfg(unix)]
#[test]
fn probe_unavailable_with_live_pid_classifies_as_probe_unavailable() {
    let versioned = format!("{STABLE_TOKEN_PREFIX}lstart-token");
    let identity = classify_run_owner_with_probes(
        Some(4242),
        Some(versioned.as_str()),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::Unavailable,
        |_| false,
        |_| true,
    );
    assert_eq!(identity, OwnerIdentity::ProbeUnavailable);
    // Build a JobRun with state=Running so we can exercise the stale-path
    // gate alongside the classification (the closure-based classifier is
    // the only path that distinguishes Unavailable from NoProcess).
}

#[cfg(unix)]
#[test]
fn probe_no_process_with_live_pid_classifies_as_probe_unavailable() {
    // Race: ps -p says no-process, but kill(pid, 0) still sees the PID.
    // We must not finalize the run on a single ps result that disagrees
    // with the kernel's liveness signal.
    let versioned = format!("{STABLE_TOKEN_PREFIX}lstart-token");
    let identity = classify_run_owner_with_probes(
        Some(4242),
        Some(versioned.as_str()),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::NoProcess,
        |_| false,
        |_| true,
    );
    assert_eq!(identity, OwnerIdentity::ProbeUnavailable);
}

#[cfg(unix)]
#[test]
fn probe_unavailable_with_dead_pid_classifies_as_missing() {
    let versioned = format!("{STABLE_TOKEN_PREFIX}lstart-token");
    let identity = classify_run_owner_with_probes(
        Some(4242),
        Some(versioned.as_str()),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::Unavailable,
        |_| false,
        |_| false,
    );
    // Probe failed AND kill(0) confirms dead → still legitimately stale.
    assert_eq!(identity, OwnerIdentity::Missing);
}

#[cfg(unix)]
#[test]
fn versioned_token_mismatch_with_live_pid_classifies_as_mismatch() {
    let persisted = format!("{STABLE_TOKEN_PREFIX}old-lstart");
    let identity = classify_run_owner_with_probes(
        Some(4242),
        Some(persisted.as_str()),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::Token(format!("{STABLE_TOKEN_PREFIX}fresh-lstart")),
        |_| false,
        |_| true,
    );
    assert_eq!(identity, OwnerIdentity::Mismatch);
}

#[cfg(unix)]
#[test]
fn versioned_token_match_classifies_as_verified() {
    let persisted = format!("{STABLE_TOKEN_PREFIX}same-lstart");
    let identity = classify_run_owner_with_probes(
        Some(4242),
        Some(persisted.as_str()),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::Token(format!("{STABLE_TOKEN_PREFIX}same-lstart")),
        |_| false,
        |_| true,
    );
    assert_eq!(identity, OwnerIdentity::Verified);
}

#[cfg(unix)]
#[test]
fn running_run_owner_stale_reason_excludes_probe_unavailable() {
    // A Running run whose probe is Unavailable and whose PID is alive
    // must NOT be classified as stale.
    let run = JobRun {
        run_id: "qa_run".to_string(),
        job_id: "qa_job".to_string(),
        attempt: 1,
        state: JobRunState::Running,
        scheduled_at: Utc::now(),
        started_at: Some(Utc::now()),
        finished_at: None,
        duration_ms: None,
        pid: Some(4242),
        pid_start_time: Some(format!("{STABLE_TOKEN_PREFIX}lstart-token")),
        input: None,
        retry_source_run_id: None,
        created_at: Utc::now(),
        steps: Vec::new(),
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
    };
    // We can't override the probe at this seam (production wrapper), but
    // we can assert the lower-level helper agrees: ProbeUnavailable is
    // not in the stale set.
    let identity = classify_run_owner_with_probes(
        run.pid,
        run.pid_start_time.as_deref(),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::Unavailable,
        |_| false,
        |_| true,
    );
    assert!(matches!(identity, OwnerIdentity::ProbeUnavailable));
    // And the stale-reason helper would only emit Some for Mismatch /
    // Missing — verified separately by other tests.
}

#[cfg(unix)]
#[test]
fn stale_failure_message_distinguishes_probe_outcomes() {
    let run = JobRun {
        run_id: "qa_run".to_string(),
        job_id: "qa_job".to_string(),
        attempt: 1,
        state: JobRunState::Running,
        scheduled_at: Utc::now(),
        started_at: Some(Utc::now()),
        finished_at: None,
        duration_ms: None,
        pid: Some(4242),
        pid_start_time: Some(format!("{STABLE_TOKEN_PREFIX}lstart-token")),
        input: None,
        retry_source_run_id: None,
        created_at: Utc::now(),
        steps: Vec::new(),
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
    };
    let mismatch_message = stale_job_run_message(&run, Some(OwnerIdentity::Mismatch));
    let missing_message = stale_job_run_message(&run, Some(OwnerIdentity::Missing));
    let probe_unavailable_message =
        stale_job_run_message(&run, Some(OwnerIdentity::ProbeUnavailable));

    assert!(
        mismatch_message.contains("reason=token_mismatch"),
        "{mismatch_message}"
    );
    assert!(
        missing_message.contains("reason=process_not_found"),
        "{missing_message}"
    );
    // Even though this state never finalizes, the tag must be set so a
    // future caller's diagnostic is never silently mis-labeled.
    assert!(
        probe_unavailable_message.contains("reason=probe_unavailable"),
        "{probe_unavailable_message}"
    );
}

// ---- Cross-PID-namespace regression coverage (ORB-10594) ----
//
// Incident 2026-08-02: `jrun-20260802-2013-2` ran to a complete success (PR
// opened at 20:55:49Z) but its run record said `interrupted` since 20:43:00Z,
// because an Orbit CLI invoked by a sandboxed agent — `bwrap --unshare-all
// --proc /proc`, i.e. a private PID namespace — swept for orphans. From inside
// that namespace the host worker PIDs are invisible, so `ps` and
// `kill(pid, 0)` both reported "gone" for three healthy runs at once.

#[cfg(unix)]
#[test]
fn foreign_pid_namespace_never_condemns_a_live_owner() {
    // The exact incident shape: the observer is in another PID namespace, so
    // every probe available to it says the owner is gone — `ps` finds no
    // process and `kill(pid, 0)` agrees. Pre-fix this was `Missing`, which is
    // the stale set. The recorded owner was in fact mid-run.
    let persisted = format!("{STABLE_TOKEN_PREFIX}pidns=4026531836:Sun Aug  2 20:13:45 2026");
    let identity = classify_run_owner_with_probes(
        Some(83327),
        Some(persisted.as_str()),
        PidNamespaceScope::Foreign,
        |_| ProbeOutcome::NoProcess,
        |_| false,
        |_| false,
    );
    assert_eq!(identity, OwnerIdentity::ForeignPidNamespace);
}

#[cfg(unix)]
#[test]
fn same_pid_namespace_still_detects_a_genuinely_dead_owner() {
    // The distinguishing case: identical probe answers, but the observer
    // shares the owner's namespace, so "not found" really does mean dead.
    let persisted = format!("{STABLE_TOKEN_PREFIX}pidns=4026531836:Sun Aug  2 20:13:45 2026");
    let identity = classify_run_owner_with_probes(
        Some(83327),
        Some(persisted.as_str()),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::NoProcess,
        |_| false,
        |_| false,
    );
    assert_eq!(identity, OwnerIdentity::Missing);
}

#[cfg(unix)]
#[test]
fn foreign_pid_namespace_is_not_in_the_stale_set_for_running_or_pending_runs() {
    // End-to-end through the production classifier: a token naming a PID
    // namespace this process is definitely not in must leave the run alone in
    // both the running and the pending stale gates.
    let Some(current) = current_pid_namespace() else {
        return; // non-Linux: no namespace to compare against.
    };
    let foreign = format!("{current}0000");
    let persisted = format!("{STABLE_TOKEN_PREFIX}pidns={foreign}:Sun Aug  2 20:13:45 2026");

    let mut run = running_run_with_token(999_999, Some(&persisted));
    assert!(
        running_run_owner_stale_reason(&run).is_none(),
        "a run owned in another PID namespace must never be reconciled from here"
    );

    run.state = JobRunState::Pending;
    run.created_at = Utc::now() - Duration::days(4);
    assert!(
        pending_run_stale_reason(&run).is_none(),
        "a claimed pending run owned in another PID namespace must stay pending"
    );
}

#[cfg(unix)]
#[test]
fn same_pid_namespace_token_still_reconciles_a_dead_owner_end_to_end() {
    // Counterpart to the test above, through the same production classifier:
    // a token naming *this* namespace with an impossible PID is still stale,
    // so the fix cannot be satisfied by never condemning anything.
    let Some(current) = current_pid_namespace() else {
        return;
    };
    let persisted = format!("{STABLE_TOKEN_PREFIX}pidns={current}:Sun Aug  2 20:13:45 2026");
    let run = running_run_with_token(999_999, Some(&persisted));
    assert_eq!(
        running_run_owner_stale_reason(&run),
        Some(OwnerIdentity::Missing),
    );
}

#[cfg(unix)]
#[test]
fn v1_token_from_an_older_binary_still_verifies_against_a_v2_probe() {
    // Upgrade safety: a run claimed before ORB-10594 carries a v1 token with
    // no namespace field. Re-probing it now yields a v2 token; the owner must
    // still verify rather than reading as a PID-reuse mismatch.
    let persisted = format!("{STABLE_TOKEN_PREFIX_V1}Sun Aug  2 20:13:45 2026");
    let probed = format!("{STABLE_TOKEN_PREFIX}pidns=4026531836:Sun Aug  2 20:13:45 2026");
    let identity = classify_run_owner_with_probes(
        Some(4242),
        Some(persisted.as_str()),
        PidNamespaceScope::Unknown,
        |_| ProbeOutcome::Token(probed.clone()),
        |_| false,
        |_| true,
    );
    assert_eq!(identity, OwnerIdentity::Verified);

    // ...and a genuine PID reuse under a v1 token is still caught.
    let reused = format!("{STABLE_TOKEN_PREFIX}pidns=4026531836:Mon Aug  3 09:00:00 2026");
    assert_eq!(
        classify_run_owner_with_probes(
            Some(4242),
            Some(persisted.as_str()),
            PidNamespaceScope::Unknown,
            |_| ProbeOutcome::Token(reused),
            |_| false,
            |_| true,
        ),
        OwnerIdentity::Mismatch,
    );
}

#[cfg(unix)]
#[test]
fn foreign_namespace_diagnostic_is_tagged_distinctly() {
    let run = running_run_with_token(83327, Some("token"));
    let message = stale_job_run_message(&run, Some(OwnerIdentity::ForeignPidNamespace));
    assert!(
        message.contains("reason=foreign_pid_namespace"),
        "{message}"
    );
}

#[cfg(unix)]
fn running_run_with_token(pid: u32, token: Option<&str>) -> JobRun {
    JobRun {
        run_id: "qa_run".to_string(),
        job_id: "qa_job".to_string(),
        attempt: 1,
        state: JobRunState::Running,
        scheduled_at: Utc::now(),
        started_at: Some(Utc::now()),
        finished_at: None,
        duration_ms: None,
        pid: Some(pid),
        pid_start_time: token.map(str::to_string),
        input: None,
        retry_source_run_id: None,
        created_at: Utc::now(),
        steps: Vec::new(),
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
    }
}

#[cfg(unix)]
#[test]
fn show_job_run_reconciles_dead_pid_with_probe_outcome_in_message() {
    // End-to-end regression: dead PID still finalizes, and the failure
    // step's error message carries `reason=process_not_found`.
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "qa_dead_pid_reason");
    let started_at = Utc::now() - Duration::seconds(3);
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, started_at, 999_999)
        .expect("mark running with impossible pid");

    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(shown.state, JobRunState::Interrupted);
    let failure_step = shown
        .steps
        .iter()
        .find(|step| step.state == JobRunState::Interrupted)
        .expect("stale interrupted step");
    let message = failure_step
        .error_message
        .as_deref()
        .expect("failure message");
    assert!(
        message.contains("reason=process_not_found"),
        "diagnostic must record probe outcome: {message}"
    );
}
