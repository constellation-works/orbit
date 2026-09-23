// Migrated from the inline `state_env_var_tests` block in src/context.rs
// when the module was decomposed (ORB-10015). The `state_env_vars` cases went
// with the v1 executor transport in [ORB-10395]; the v2 child-environment
// coverage lives in `activity_job::cli_runner::tests::orchestrator`.
use super::super::{ProvenanceEnv, provenance_env};

#[test]
fn provenance_env_preserves_both_namespace_contracts() {
    let vars = provenance_env(ProvenanceEnv {
        orbit_run_id: Some("jrun-42"),
        orbit_managed_run_context: true,
        orbit_agent_name: Some("codex"),
        orbit_agent_model: Some("gpt-6-sol"),
        orbit_session_id: Some("session-7"),
        orbit_task_id: Some("ORB-10344"),
        orbit_active_task: true,
        agent_run_id: Some("jrun-42"),
        agent_model: Some("gpt-6-sol"),
        agent_task_id: Some("ORB-10344"),
    });

    assert_eq!(
        vars,
        vec![
            ("ORBIT_RUN_ID".to_string(), "jrun-42".to_string()),
            ("ORBIT_MANAGED_RUN_CONTEXT".to_string(), "1".to_string()),
            ("ORBIT_AGENT_NAME".to_string(), "codex".to_string()),
            ("ORBIT_AGENT_MODEL".to_string(), "gpt-6-sol".to_string()),
            ("ORBIT_SESSION_ID".to_string(), "session-7".to_string()),
            ("ORBIT_TASK_ID".to_string(), "ORB-10344".to_string()),
            ("ORBIT_ACTIVE_TASK_ID".to_string(), "ORB-10344".to_string()),
            ("AGENT_RUN_ID".to_string(), "jrun-42".to_string()),
            ("AGENT_MODEL".to_string(), "gpt-6-sol".to_string()),
            ("AGENT_TASK".to_string(), "ORB-10344".to_string()),
        ]
    );
}

#[test]
fn provenance_env_omits_unknown_fields() {
    assert!(provenance_env(ProvenanceEnv::default()).is_empty());
}
