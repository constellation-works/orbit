//! Required-check records keep a stable id across an attempt's report
//! revisions [ORB-14370].
//!
//! Command-text matching falsely blocked correct candidates: a reviewer that
//! renames, rewraps or concretises a command between revisions dropped the
//! obligation as far as the gate could tell. Reports reach the gate through
//! the public `orbit.task.artifact.put` tool, which refuses a revision that
//! drops an earlier record id while the reviewer can still correct it, and
//! the deterministic settlement then compares by id.

use orbit_automation::review::certificate_acceptable;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, RetiredValidation, ReviewCertificate, ReviewReportHistory,
    ReviewVerdict, ValidationOutcome,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

fn report(fixture: &Fixture, verdict: &str, validation: Value, retired: Value) -> Value {
    json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": verdict,
        "summary": "Checked the candidate.",
        "findings": [],
        "validation": validation,
        "retired_validation": retired,
        "escalation": (verdict == "incomplete").then_some("validation still running"),
    })
}

fn certificate(fixture: &Fixture) -> ReviewCertificate {
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
        .unwrap()
        .expect("settlement issues a certificate");
    serde_json::from_slice(&artifact.content).unwrap()
}

fn history_len(fixture: &Fixture) -> usize {
    let history = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .unwrap()
        .expect("report history");
    ReviewReportHistory::parse(&history.content)
        .unwrap()
        .revisions
        .len()
}

/// ORB-14360 recorded a prose-named check `not_run` and later the real
/// command `passed`; ORB-14260 recorded a `<workspace>` placeholder and
/// later the absolute path. Carrying the record id forward settles both.
#[test]
fn a_carried_record_id_settles_when_the_command_text_changes() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::a_carried_record_id_settles_when_the_command_text_changes",
    ) {
        return;
    }
    // `None`: the absolute path of the fixture's own checkout.
    for (incident, earlier, current) in [
        (
            "ORB-14360",
            "focused CLI reference invocation verification",
            Some("python3 .orbit/tmp/verify-reference-examples.py"),
        ),
        (
            "ORB-14260",
            "cargo test --manifest-path <workspace>/Cargo.toml -p orbit-core",
            None,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.admit();
        let current = current.map_or_else(
            || {
                format!(
                    "cargo test --manifest-path {}/Cargo.toml -p orbit-core",
                    fixture.repo.display()
                )
            },
            str::to_string,
        );
        fixture.put_report(&report(
            &fixture,
            "incomplete",
            json!([{"id": "V1", "command": earlier, "outcome": "not_run", "role": "required"}]),
            json!([]),
        ));
        fixture.put_report(&report(
            &fixture,
            "accept",
            json!([{"id": "V1", "command": current, "outcome": "passed", "role": "required"}]),
            json!([]),
        ));

        let settled = fixture
            .settle()
            .unwrap_or_else(|error| panic!("{incident}: the carried id settles: {error}"));
        assert_eq!(settled["gate"], "passed", "{incident}");
        let certificate = certificate(&fixture);
        assert!(certificate.validation_complete, "{incident}");
        assert_eq!(
            certificate
                .retained_obligations
                .iter()
                .map(|obligation| obligation.validation.command.as_str())
                .collect::<Vec<_>>(),
            vec![earlier],
            "{incident}: the earlier record stays visible on the certificate"
        );
        assert_eq!(certificate_acceptable(&certificate), Ok(()), "{incident}");
    }
}

/// A revision that drops an earlier record id is refused at attach with
/// the record named, and so is retiring a record that failed; the reviewer
/// then corrects the report in the same attempt and it settles.
#[test]
fn a_revision_dropping_a_record_id_is_refused_at_attach_and_resubmitted() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::a_revision_dropping_a_record_id_is_refused_at_attach_and_resubmitted",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    let first = report(
        &fixture,
        "incomplete",
        json!([
            {"id": "V1", "command": "make ci-fast", "outcome": "failed", "role": "required"},
            {"id": "V2", "command": "make docs-check", "outcome": "not_run", "role": "required"},
        ]),
        json!([]),
    );
    fixture.put_report(&first);

    let dropped = fixture
        .try_put_report(&report(
            &fixture,
            "accept",
            json!([{"id": "V2", "command": "make docs-check", "outcome": "passed"}]),
            json!([]),
        ))
        .expect_err("a revision that omits V1 is refused");
    let message = dropped.to_string();
    assert!(
        message.contains("required validation record `V1` (`make ci-fast`)")
            && message.contains("this report omits it"),
        "the refusal names the dropped record: {message}"
    );

    let retired_failure = fixture
        .try_put_report(&report(
            &fixture,
            "accept",
            json!([{"id": "V2", "command": "make docs-check", "outcome": "passed"}]),
            json!([{"id": "V1", "reason": "not needed"}]),
        ))
        .expect_err("a failed record cannot be retired");
    assert!(
        retired_failure
            .to_string()
            .contains("record `V1` (`make ci-fast`) was recorded failed")
            && retired_failure
                .to_string()
                .contains("retires it although it failed"),
        "{retired_failure}"
    );

    // Neither refusal replaced the report or entered the history.
    let current = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_ARTIFACT)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&current.content).unwrap(),
        first
    );
    assert_eq!(history_len(&fixture), 1);

    // The corrected revision reruns V1 under a wrapped command and retires
    // V2, which never ran.
    fixture.put_report(&report(
        &fixture,
        "accept",
        json!([{
            "id": "V1",
            "command": "set -o pipefail; make ci-fast 2>&1 | tee .orbit/tmp/ci-fast.log",
            "outcome": "passed",
            "role": "required",
        }]),
        json!([{"id": "V2", "reason": "the docs target was removed from this repository"}]),
    ));
    assert_eq!(history_len(&fixture), 2);

    let settled = fixture.settle().expect("the corrected revision settles");
    assert_eq!(settled["gate"], "passed");
    let certificate = certificate(&fixture);
    assert!(certificate.validation_complete);
    assert_eq!(
        certificate.retired_validation,
        vec![RetiredValidation {
            id: "V2".into(),
            reason: "the docs target was removed from this repository".into(),
        }]
    );
    assert_eq!(certificate_acceptable(&certificate), Ok(()));
    let comment = fixture
        .runtime
        .get_task_comments(&fixture.task_id)
        .unwrap()
        .into_iter()
        .map(|comment| comment.message)
        .rfind(|message| message.starts_with("before-PR review settled attempt"))
        .expect("settlement comments its verdict");
    assert!(
        comment.contains(
            "V2 `make docs-check` not_run (retired: the docs target was removed from this \
             repository)"
        ),
        "the retirement is disclosed: {comment}"
    );
}

/// Reports written without record ids attach and settle as before: a
/// command-identity match resolves the earlier record, and a renamed command
/// is still a dropped obligation.
#[test]
fn reports_without_record_ids_keep_the_command_identity_rules() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::reports_without_record_ids_keep_the_command_identity_rules",
    ) {
        return;
    }
    for (earlier, current, passes) in [
        (
            "make ci-fast",
            "TMPDIR=\"$PWD/.orbit/tmp\" make  ci-fast",
            true,
        ),
        (
            "focused CLI reference invocation verification",
            "python3 .orbit/tmp/verify-reference-examples.py",
            false,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.admit();
        fixture.put_report(&report(
            &fixture,
            "incomplete",
            json!([{"command": earlier, "outcome": "not_run"}]),
            json!([]),
        ));
        fixture.put_report(&report(
            &fixture,
            "accept",
            json!([{"command": current, "outcome": "passed"}]),
            json!([]),
        ));
        let settled = fixture.settle();
        let certificate = certificate(&fixture);
        assert_eq!(settled.is_ok(), passes, "{earlier} -> {current}");
        assert_eq!(certificate.validation_complete, passes);
        assert_eq!(
            certificate.retained_obligations[0].validation.outcome,
            ValidationOutcome::NotRun
        );
        if !passes {
            assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
            let escalation = certificate.escalation.unwrap_or_default();
            assert!(
                escalation.contains(&format!(
                    "required check `{earlier}` was recorded not_run by an earlier report \
                     revision of this attempt and the final report omits it"
                )),
                "{escalation}"
            );
        }
    }
}
