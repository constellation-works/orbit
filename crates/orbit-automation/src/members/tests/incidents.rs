#[test]
fn incident_identity_uses_cause_and_episode_and_requires_settled_authority() {
    use crate::members::incidents::{IncidentFacts, incident_key};
    let mut facts = IncidentFacts {
        workspace: "ws".into(),
        episode: Some("root-attempt".into()),
        cause: Some("child-step".into()),
        failure: true,
        recovery_settled: true,
        current_failure_coupling: true,
        cancellation: false,
    };
    let key = incident_key(&facts).unwrap();
    assert_eq!(key, incident_key(&facts).unwrap());
    facts.recovery_settled = false;
    assert!(incident_key(&facts).is_err());
    facts.recovery_settled = true;
    facts.cancellation = true;
    assert!(incident_key(&facts).is_err());
    facts.cancellation = false;
    facts.current_failure_coupling = false;
    assert!(incident_key(&facts).is_err());
    facts.current_failure_coupling = true;
    facts.episode = Some("new-authorized-execution".into());
    assert_ne!(key, incident_key(&facts).unwrap());
}
