use orbit_types::workflow::{CommitRole, orbit_role_for_email};

#[test]
fn orbit_invalid_recognizes_only_written_roles_and_keeps_orbit_local_history() {
    for (email, expected) in [
        ("implementer@orbit.invalid", Some(CommitRole::Implementer)),
        ("reviewer@orbit.invalid", Some(CommitRole::Reviewer)),
        ("recovery@orbit.invalid", Some(CommitRole::Recovery)),
        ("orbit@orbit.invalid", Some(CommitRole::Orbit)),
        ("someone@orbit.invalid", None),
        ("agent@orbit.invalid", None),
        ("@orbit.invalid", None),
        ("claude@orbit.local", Some(CommitRole::Implementer)),
        ("codex-reviewer@orbit.local", Some(CommitRole::Reviewer)),
        ("orbit-recovery@orbit.local", Some(CommitRole::Recovery)),
        ("orbit@orbit.local", Some(CommitRole::Orbit)),
    ] {
        assert_eq!(orbit_role_for_email(email), expected, "{email}");
    }
}
