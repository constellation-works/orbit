//! [ORB-14822] A step after `commit` whose recovery repairs the worktree is
//! retried on a commit the host makes; the recovery agent never commits.
//!
//! The substitute provider only writes files and its decision; it runs no
//! Git command. A repair the host must not commit is refused by name before
//! the index changes, and the step fails without its retry.

use std::sync::{Arc, Mutex};

use tracing_subscriber::prelude::*;

use super::*;

/// The candidate the implementation committed, which `validate` judges.
fn commit_candidate(worktree: &Path) {
    std::fs::write(worktree.join("candidate.txt"), "broken\n").unwrap();
    git(worktree, &["add", "candidate.txt"]);
    git(worktree, &["commit", "-q", "-m", "implementation"]);
}

/// The output every `git_commit` outcome records.
fn committed() -> (&'static str, Value) {
    (
        "commit",
        json!({
            "phase": "commit",
            "decision": "performed",
            "committed": true,
            "skipped_no_diff_expected": false,
            "task_id": TASK,
        }),
    )
}

const REPAIR: &str =
    r#"printf 'fixed\n' > "$root/candidate.txt"; bound retry "fixed the candidate" > "$path""#;

fn failure_text(observed: &Observed) -> String {
    match &observed.result {
        Ok((_, message)) => message.clone().unwrap_or_default(),
        Err(message) => message.clone(),
    }
}

fn a_validate_repair_is_committed_by_the_host_and_retried_on_that_head() {
    let fixture = fixture();
    // A claimed leaf's `validate` has step recovery too, and the same commit.
    for (name, claimed, recovery_model) in [
        ("owner_repair", false, Some(RECOVERY_MODEL)),
        ("claimed_repair", true, Some(RECOVERY_MODEL)),
        ("owner_unknown_model", false, None),
        ("claimed_unknown_model", true, None),
    ] {
        let observed = run(
            &fixture,
            &Case {
                name,
                producer: REPAIR,
                claimed,
                recovery_model,
                prepare: commit_candidate,
                before: vec![committed()],
                ..Case::default()
            },
        );
        assert_eq!(observed.result, Ok((true, None)), "{name}");
        assert!(observed.decision().retry_admitted, "{name}");
        assert!(observed.projection.attempts[0].retry_admitted, "{name}");
        assert_eq!(observed.post_recovery, ["success"], "{name}");
        let [(implementation, failed_status), (retried, retried_status)] =
            observed.attempts_saw.as_slice()
        else {
            panic!("{name}: two validate attempts: {:?}", observed.attempts_saw);
        };
        assert_eq!(failed_status, "", "{name}: the failure saw a clean tree");
        assert_ne!(
            retried, implementation,
            "{name}: the retry ran on a new head"
        );
        assert_eq!(
            retried_status, "",
            "{name}: the retry saw the repair committed, not left dirty"
        );
        let worktree = &observed.worktree;
        assert_eq!(
            git_stdout(worktree, &["rev-parse", &format!("{retried}^")]),
            *implementation,
            "{name}: the repair is one commit on top of the candidate"
        );
        assert_eq!(
            git_stdout(worktree, &["show", "--format=", "--name-only", retried]),
            "candidate.txt",
            "{name}: exactly the repaired path"
        );
        assert_eq!(
            git_stdout(worktree, &["show", &format!("{retried}:candidate.txt")]),
            "fixed"
        );
        // Orbit's process identity commits; the author names the recovery
        // activity's resolved model (or its historical fallback), and the
        // trailers record the recovery that made the change [ORB-16768].
        assert_eq!(
            git_stdout(
                worktree,
                &["show", "-s", "--format=%an <%ae>|%cn <%ce>", retried]
            ),
            format!(
                "{} <recovery@orbit.invalid>|orbit <orbit@orbit.invalid>",
                recovery_model
                    .map(|model| format!("orbit[{model}]"))
                    .unwrap_or_else(|| "orbit".to_string())
            ),
            "{name}"
        );
        let message = git_stdout(worktree, &["show", "-s", "--format=%B", retried]);
        // [ORB-25009] The recovery commit meets the commit-message standard.
        orbit_types::workflow::validate(&message)
            .unwrap_or_else(|violation| panic!("{name}: {violation}\n{message}"));
        for trailer in [
            format!("Orbit-Recovery-Run: run-{name}"),
            "Orbit-Recovery-Step: validate".to_string(),
            "Orbit-Recovery-Activity: step_failure_recovery".to_string(),
        ] {
            assert!(message.contains(&trailer), "{name}: {trailer}: {message}");
        }
        assert!(message.contains(&format!("[{TASK}]")), "{name}: {message}");
    }

    // A review admitted on an earlier candidate (before a final-recovery
    // resume moved it) does not pin the current head.
    let stale_review = run(
        &fixture,
        &Case {
            name: "stale_review",
            producer: REPAIR,
            prepare: commit_candidate,
            before: vec![
                committed(),
                (
                    "review_gate_admit",
                    crate::review_admission_shape::admitted_review(
                        "attempt-0",
                        "lineage",
                        &"0".repeat(40),
                    ),
                ),
            ],
            ..Case::default()
        },
    );
    assert_eq!(stale_review.result, Ok((true, None)));
    assert_ne!(
        stale_review.attempts_saw[0].0,
        stale_review.attempts_saw[1].0
    );

    // A recovery that changes nothing in the worktree leaves the candidate
    // as it was: no commit, and the retry runs on the same head.
    let clean = run(
        &fixture,
        &Case {
            name: "clean_repair",
            producer: r#"bound retry "the runner recovered" > "$path""#,
            prepare: commit_candidate,
            before: vec![committed()],
            ..Case::default()
        },
    );
    assert_eq!(clean.result, Ok((true, None)));
    let heads = clean
        .attempts_saw
        .iter()
        .map(|(head, _)| head)
        .collect::<Vec<_>>();
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0], heads[1], "nothing to commit");

    // Before the candidate is committed, the `commit` step owns the repair.
    let uncommitted = run(
        &fixture,
        &Case {
            name: "before_commit",
            producer: REPAIR,
            prepare: commit_candidate,
            ..Case::default()
        },
    );
    assert_eq!(uncommitted.result, Ok((true, None)));
    assert_eq!(
        uncommitted.attempts_saw[0].0, uncommitted.attempts_saw[1].0,
        "no host commit before the commit step ran"
    );
    assert_eq!(uncommitted.attempts_saw[1].1, "M candidate.txt");
}

fn a_repair_the_host_must_not_commit_is_refused_by_name_and_nothing_is_committed() {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let subscriber =
        tracing_subscriber::registry().with(HostRefusalWarnings(Arc::clone(&warnings)));
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let fixture = fixture();
    let admitted_review = (
        "review_gate_admit",
        crate::review_admission_shape::admitted_review("attempt-1", "lineage", "HEAD"),
    );
    let settled_review = ("review_gate_settle", json!({"reviewed_head_sha": "HEAD"}));
    let mut no_diff = committed();
    no_diff.1["skipped_no_diff_expected"] = json!(true);
    let cases = [
        (
            "protected_path",
            r#"printf 'TOKEN=x\n' > "$root/.env""#,
            vec![committed()],
            "?? .env",
        ),
        (
            "outside_footprint",
            r#"ln -s /etc/hosts "$root/hosts""#,
            vec![committed()],
            "?? hosts",
        ),
        ("no_diff_route", "", vec![no_diff], ""),
        (
            "reviewed_candidate",
            "",
            vec![committed(), admitted_review],
            "",
        ),
    ];
    let cases = cases.into_iter().chain([(
        // A claimed leaf validates the reviewed head.
        "reviewed_candidate",
        "",
        vec![committed(), settled_review],
        "",
    )]);
    for (index, (reason, extra, before, leftover)) in cases.enumerate() {
        let name = format!("{reason}_{index}");
        // Each refused repair also fixes the candidate, which must stay
        // uncommitted with the rest of it.
        let producer = format!("{extra}\n{REPAIR}");
        let observed = run(
            &fixture,
            &Case {
                name: &name,
                producer: &producer,
                prepare: commit_candidate,
                before,
                ..Case::default()
            },
        );
        assert_eq!(observed.decision().status, "verified", "{reason}");
        assert_eq!(
            observed.decision().verdict.as_deref(),
            Some("retry"),
            "{reason}"
        );
        assert!(!observed.decision().retry_admitted, "{reason}");
        let projected = &observed.projection.attempts[0];
        assert!(!projected.retry_admitted, "{reason}");
        let decision = projected.decision.as_ref().unwrap();
        assert_eq!(decision.status, "verified", "{reason}");
        assert_eq!(decision.verdict.as_deref(), Some("retry"), "{reason}");
        assert_eq!(observed.deliveries, 1, "{reason}: no post-recovery attempt");
        assert!(observed.post_recovery.is_empty(), "{reason}");
        let failure = failure_text(&observed);
        assert!(
            failure.contains(&format!("recovery_commit_refused:{reason}:"))
                && failure.contains(ORIGINAL_FAILURE),
            "{reason}: the step fails with the typed refusal and keeps the original: {failure}"
        );
        // A verified `retry` that the host vetoes must not be reported as a
        // decision that admitted nothing. The warning names host admission
        // and repeats the refusal the step returns.
        let warning = {
            let recorded = warnings.lock().unwrap();
            let matched = recorded
                .iter()
                .filter(|warning| warning.run_id == format!("run-{name}"))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                matched.len(),
                1,
                "{reason}: one no-retry warning for this run: {recorded:?}"
            );
            matched.into_iter().next().unwrap()
        };
        assert!(
            warning.message.contains("host admission")
                && !warning.message.contains("recovery decision admits"),
            "{reason}: the warning must blame host admission, not the verified decision: {}",
            warning.message
        );
        assert_eq!(
            warning.decision_verdict.as_deref(),
            Some("retry"),
            "{reason}: the warning keeps the decision-file verdict"
        );
        assert_eq!(
            warning.retry_admitted,
            Some(false),
            "{reason}: the warning reports that host admission refused the retry"
        );
        let refusal = warning
            .host_refusal
            .as_deref()
            .unwrap_or_else(|| panic!("{reason}: the warning carries the host refusal"));
        assert!(
            refusal.starts_with(&format!("recovery_commit_refused:{reason}:"))
                && failure.contains(refusal),
            "{reason}: the warning refusal must be the refusal the step returns\n\
             warning: {refusal}\nerror: {failure}"
        );
        let worktree = &observed.worktree;
        assert_eq!(
            git_stdout(worktree, &["rev-parse", "HEAD"]),
            observed.attempts_saw[0].0,
            "{reason}: nothing was committed"
        );
        assert_eq!(
            git_stdout(worktree, &["diff", "--cached", "--name-only"]),
            "",
            "{reason}: the index is untouched"
        );
        let status = git_stdout(worktree, &["status", "--porcelain"]);
        assert!(
            status.contains("M candidate.txt") && status.contains(leftover),
            "{reason}: the repair is left as recovery wrote it: {status}"
        );
    }
}

/// `orbit.engine.job_executor` no-retry warnings emitted while a host guard
/// refuses a verified retry.
#[derive(Clone, Debug)]
struct HostRefusalWarning {
    run_id: String,
    message: String,
    decision_verdict: Option<String>,
    retry_admitted: Option<bool>,
    host_refusal: Option<String>,
}

struct HostRefusalWarnings(Arc<Mutex<Vec<HostRefusalWarning>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for HostRefusalWarnings {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "orbit.engine.job_executor" {
            return;
        }
        let mut fields = WarningFields::default();
        event.record(&mut fields);
        if !fields.message.contains("post-recovery attempt") {
            return;
        }
        self.0.lock().unwrap().push(HostRefusalWarning {
            run_id: fields.run_id,
            message: fields.message,
            decision_verdict: fields.decision_verdict,
            retry_admitted: fields.retry_admitted,
            host_refusal: fields.host_refusal,
        });
    }
}

#[derive(Default)]
struct WarningFields {
    run_id: String,
    message: String,
    decision_verdict: Option<String>,
    retry_admitted: Option<bool>,
    host_refusal: Option<String>,
}

impl tracing::field::Visit for WarningFields {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "retry_admitted" {
            self.retry_admitted = Some(value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "run_id" => self.run_id = value.to_string(),
            "message" => self.message = value.to_string(),
            "decision_verdict" => self.decision_verdict = Some(value.to_string()),
            "host_refusal" => self.host_refusal = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        let text = rendered.trim_matches('"');
        if field.name() == "retry_admitted" {
            self.retry_admitted = match text {
                "true" => Some(true),
                "false" => Some(false),
                _ => self.retry_admitted,
            };
            return;
        }
        self.record_str(field, text);
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_validate_repair_is_committed_by_the_host_and_retried_on_that_head_scenarios,
    a_validate_repair_is_committed_by_the_host_and_retried_on_that_head,
    a_repair_the_host_must_not_commit_is_refused_by_name_and_nothing_is_committed
);
