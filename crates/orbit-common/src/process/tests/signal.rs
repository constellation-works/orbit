//! Kernel-level identity-bound delivery under deterministic numeric PID reuse.

use super::BoundProcess;

/// Substitute a real bystander's number after binding the original, simulating
/// the otherwise unforceable PID-reuse window without exhausting host PIDs.
#[test]
fn retained_signal_identity_never_targets_a_reused_numeric_pid() {
    let original = crate::test_env::spawn_unrelated_process();
    let replacement = crate::test_env::spawn_unrelated_process();
    let mut bound = BoundProcess::bind(original.pid(), None).unwrap().unwrap();
    assert!(bound.is_alive().unwrap());
    // Positive control: the ordinary bound signal reaches a live original.
    assert!(bound.signal(libc::SIGTERM).unwrap());
    drop(original);
    bound.pid = replacement.pid();
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        assert!(!bound.is_alive().unwrap());
        assert!(!bound.signal(signal).unwrap());
        assert!(
            super::super::identity::process_is_alive(replacement.pid()),
            "a retained identity must never signal the replacement numeric PID"
        );
    }
}
