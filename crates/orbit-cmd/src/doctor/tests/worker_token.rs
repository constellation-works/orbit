use crate::doctor::WorkspaceDoctorStatus;
use crate::doctor::worker_token::{
    ClockCredentials, WorkerTokenFacts, classify_clock_credentials, worker_token_row,
};

const OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const API_KEY: &str = "ANTHROPIC_API_KEY";

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_string()).collect()
}

fn facts(pass: &[&str], clock: ClockCredentials) -> WorkerTokenFacts {
    WorkerTokenFacts {
        macos: true,
        claude_routed: true,
        pass: names(pass),
        clock,
    }
}

/// Classifies a `clock.env` holding `held` (`None` when absent) against `pass`.
fn classify(held: Option<&[&str]>, pass: &[&str]) -> ClockCredentials {
    classify_clock_credentials(Ok(held.map(names)), &names(pass))
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

#[test]
fn clock_file_is_ready_only_when_it_holds_a_credential_the_workspace_admits() {
    // (names held in clock.env, workspace pass, expected row status)
    let cases: [(&[&str], &[&str], WorkspaceDoctorStatus); 6] = [
        (&[API_KEY], &[OAUTH_TOKEN], WorkspaceDoctorStatus::Warning),
        (&[OAUTH_TOKEN], &[API_KEY], WorkspaceDoctorStatus::Warning),
        (&[API_KEY], &[API_KEY], WorkspaceDoctorStatus::Ok),
        (&[OAUTH_TOKEN], &[OAUTH_TOKEN], WorkspaceDoctorStatus::Ok),
        (
            &[OAUTH_TOKEN, API_KEY],
            &[API_KEY],
            WorkspaceDoctorStatus::Ok,
        ),
        (
            &[OAUTH_TOKEN, API_KEY],
            &[OAUTH_TOKEN],
            WorkspaceDoctorStatus::Ok,
        ),
    ];
    for (held, pass, status) in cases {
        let row = worker_token_row(&facts(pass, classify(Some(held), pass)));
        assert_eq!(row.status, status, "held {held:?}, pass {pass:?}: {row:?}");
        if status == WorkspaceDoctorStatus::Warning {
            for name in held {
                assert!(row.message.contains(name), "names only: {row:?}");
            }
        }
    }
}

#[test]
fn absent_empty_refused_and_mismatched_clock_files_stay_distinct() {
    let pass = &[OAUTH_TOKEN];
    assert_eq!(classify(None, pass), ClockCredentials::Missing);
    assert_eq!(classify(Some(&[]), pass), ClockCredentials::Missing);
    assert_eq!(
        classify_clock_credentials(Err("mode 644".into()), &names(pass)),
        ClockCredentials::Refused("mode 644".into())
    );
    assert_eq!(
        classify(Some(&[API_KEY]), pass),
        ClockCredentials::Mismatched(vec![API_KEY.to_string()])
    );
}
