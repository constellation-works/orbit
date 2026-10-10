use crate::doctor::WorkspaceDoctorStatus;
use crate::doctor::worker_token::{ClockCredentials, WorkerTokenFacts, worker_token_row};

fn facts(pass: &[&str], clock: ClockCredentials) -> WorkerTokenFacts {
    WorkerTokenFacts {
        macos: true,
        claude_routed: true,
        pass: pass.iter().map(|name| (*name).to_string()).collect(),
        clock,
    }
}

#[test]
fn warns_for_each_place_the_worker_token_can_be_lost_and_is_ok_when_none_is() {
    let missing_pass = worker_token_row(&facts(&["HOME"], ClockCredentials::Provided));
    assert_eq!(missing_pass.status, WorkspaceDoctorStatus::Warning);
    assert!(missing_pass.message.contains("execution.env.pass"));

    let missing_clock = worker_token_row(&facts(
        &["CLAUDE_CODE_OAUTH_TOKEN"],
        ClockCredentials::Missing,
    ));
    assert_eq!(missing_clock.status, WorkspaceDoctorStatus::Warning);
    assert!(missing_clock.message.contains("clock"), "{missing_clock:?}");
    assert!(
        missing_clock
            .remediation
            .as_deref()
            .is_some_and(|fix| fix.contains("clock.env")),
        "{missing_clock:?}"
    );

    let refused = worker_token_row(&facts(
        &["ANTHROPIC_API_KEY"],
        ClockCredentials::Refused("mode 644".into()),
    ));
    assert_eq!(refused.status, WorkspaceDoctorStatus::Warning);
    assert!(refused.message.contains("mode 644"));

    for clock in [ClockCredentials::Provided, ClockCredentials::NoClock] {
        let ok = worker_token_row(&facts(&["CLAUDE_CODE_OAUTH_TOKEN"], clock));
        assert_eq!(ok.status, WorkspaceDoctorStatus::Ok, "{ok:?}");
    }
}

#[test]
fn skips_off_macos_and_when_no_routed_crew_uses_claude() {
    let mut off_macos = facts(&[], ClockCredentials::Missing);
    off_macos.macos = false;
    assert_eq!(
        worker_token_row(&off_macos).status,
        WorkspaceDoctorStatus::Skipped
    );
    let mut no_claude = facts(&[], ClockCredentials::Missing);
    no_claude.claude_routed = false;
    assert_eq!(
        worker_token_row(&no_claude).status,
        WorkspaceDoctorStatus::Skipped
    );
}
