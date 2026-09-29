use crate::application::distributed::PullSettlementEntry;

fn entry(outcome: &str, detail: Option<&str>) -> PullSettlementEntry {
    PullSettlementEntry {
        owner: "hm_owner/ws_orbit".to_string(),
        drain_run_id: "jrun-drain".to_string(),
        request_id: "req-1".to_string(),
        task_id: Some("ABC-1".to_string()),
        leaf_run_id: Some("jrun-leaf".to_string()),
        outcome: outcome.to_string(),
        detail: detail.map(str::to_string),
    }
}

/// An outcome that leaves work for the operator must say what to run, not just
/// print its token.
#[test]
fn undelivered_and_uncertain_outcomes_carry_their_next_step() {
    for (outcome, remedy) in [
        ("pending_delivery", "orbit run auto --stop"),
        ("owner_unreachable", "orbit run auto --stop"),
        ("launch_uncertain", "Recover"),
        ("no_owner_route", "mcp-destinations.toml"),
    ] {
        let line = entry(outcome, None).describe();
        assert!(line.contains(outcome), "{line}");
        assert!(
            line.contains(remedy) || line.to_lowercase().contains(&remedy.to_lowercase()),
            "{outcome} must name `{remedy}`: {line}"
        );
    }
}

/// A recorded error is the specific explanation; guidance never replaces it,
/// except that an uncertain launch always keeps its warning.
#[test]
fn a_recorded_error_is_kept_and_an_uncertain_launch_keeps_its_warning() {
    let line = entry("pending_delivery", Some("ssh: connection refused")).describe();
    assert!(line.contains("ssh: connection refused"), "{line}");

    let line = entry("launch_uncertain", Some("worker spawn unacknowledged")).describe();
    assert!(line.contains("worker spawn unacknowledged"), "{line}");
    assert!(line.to_lowercase().contains("recover"), "{line}");
}

#[test]
fn a_finished_admission_prints_only_its_subject_and_outcome() {
    let line = entry("settled", None).describe();
    assert!(
        line.contains("ABC-1") && line.contains("jrun-leaf"),
        "{line}"
    );
    assert!(line.ends_with("settled"), "{line}");
}
