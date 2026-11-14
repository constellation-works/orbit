//! A collector timeout is a temporary evidence gap, never a final classification.

use super::*;
use orbit_types::workflow::handoff::{LandingStopEvidence, LandingWaitGate};

#[test]
fn a_timed_out_job_log_is_read_again_and_classified() {
    for inherited in [false, true] {
        let (_sandbox, forge, candidate, base) = forge_fixture(false, false);
        forge.job(&candidate, failed(1, 11, CANDIDATE_ERROR));
        forge.job(
            &base,
            if inherited {
                failed(2, 21, CANDIDATE_ERROR)
            } else {
                passed(2, 21)
            },
        );
        forge.state.lock().unwrap().log_timeouts.insert(11, 1);
        forge.state.lock().unwrap().after_rerun = Some(failed(1, 12, CANDIDATE_ERROR));
        let (result, waits) = counted_land(&forge, json!({"max_wait_seconds":300}));
        let error = result.expect_err("the readable failure takes its classified route");
        let class = if inherited { "inherited" } else { "candidate" };
        assert!(
            error.to_string().contains(&format!("classified `{class}`")),
            "a timed-out log must be re-read before deciding the landing: {error}"
        );
        assert_eq!(waits[0], 5, "the first log retry backs off");
        assert!(forge.sent("ci.job_log").len() >= 2);
        if !inherited {
            assert_eq!(forge.steps(), vec![HandoffLandingStep::StopForRequeue]);
        }
        assert_not_sent(&forge, &["pr.merge"], class);
    }
}

#[test]
fn exhausted_log_reads_keep_a_waiting_gate_that_clears_while_ci_is_red() {
    for max_wait_seconds in [0, 6, 300] {
        let (_sandbox, forge, candidate, base) = forge_fixture(false, false);
        forge.job(&candidate, failed(1, 11, CANDIDATE_ERROR));
        forge.job(&base, passed(2, 21));
        forge.state.lock().unwrap().log_timeouts.insert(11, 100);
        let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_u64));
        let now = clock.clone();
        let recorded = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = recorded.clone();
        let result = orbit_engine::test_support::with_prerequisite_clock(
            move || now.get(),
            |_, _| {},
            || {
                orbit_engine::test_support::with_vcs_retry_wait(
                    move |duration| {
                        captured.borrow_mut().push(duration.as_secs());
                        // One extra second models provider work taking this poll
                        // past the deadline immediately after the first backoff.
                        clock.set(clock.get() + duration.as_millis() as u64 + 1_000);
                    },
                    || land_with(&forge, json!({"max_wait_seconds":max_wait_seconds})),
                )
            },
        );
        let waits = recorded.borrow().clone();
        let error = result.expect_err("unavailable evidence holds the landing");
        assert!(
            matches!(
                error,
                OrbitError::DeliveryRefused {
                    refusal: DeliveryRefusal::ForgeObservationUnavailable,
                    ..
                }
            ),
            "the held run remains resumable: {error}"
        );
        assert_eq!(
            waits,
            match max_wait_seconds {
                0 => vec![],
                6 => vec![5],
                _ => vec![5, 10, 20],
            }
        );
        assert_eq!(
            forge.sent("ci.job_log").len(),
            if max_wait_seconds == 6 {
                1
            } else {
                waits.len() + 1
            },
            "the expired deadline must not start another provider read"
        );
        assert_not_sent(
            &forge,
            &["ci.rerun_failed", "pr.update_branch", "pr.merge"],
            "missing logs",
        );
        let LandingStopEvidence::OwnerRefusal(refusal) =
            LandingStopEvidence::parse(&forge.last_evidence())
        else {
            panic!("the public landing must retain a typed waiting gate");
        };
        let gate = refusal.waiting_gate.expect("the owner sweep needs a gate");
        assert_eq!(gate, LandingWaitGate::JobLogsUnavailable);
        assert!(!orbit_engine::landing_wait_gate_cleared(&forge, &forge.context, gate).unwrap());
        forge.state.lock().unwrap().log_timeouts.clear();
        assert!(
            orbit_engine::landing_wait_gate_cleared(&forge, &forge.context, gate).unwrap(),
            "a readable red log must wake classification without waiting for green CI"
        );
        forge.state.lock().unwrap().after_rerun = Some(failed(1, 12, CANDIDATE_ERROR));
        let error = land_now(&forge).expect_err("the resumed landing requeues the candidate");
        assert!(
            error.to_string().contains("classified `candidate`"),
            "{error}"
        );
        assert_eq!(
            forge.steps().last(),
            Some(&HandoffLandingStep::StopForRequeue)
        );
    }
}
