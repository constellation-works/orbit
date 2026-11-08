//! [ORB-23049] Inherit mode still withholds privilege-bearing Orbit names.

use super::super::ExecutionEnvPolicy;

fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, held)| held.as_str())
}

fn assert_privilege_names_omitted(env: &[(String, String)], path: &str) {
    assert_eq!(
        value(env, "ORBIT_OPERATOR"),
        None,
        "{path}: ORBIT_OPERATOR must not reach an agent child"
    );
    assert_eq!(
        value(env, "ORBIT_WORKSPACE_CLAIM_TOKEN"),
        None,
        "{path}: ORBIT_WORKSPACE_CLAIM_TOKEN must not reach an agent child"
    );
    assert_eq!(
        value(env, "ORBIT_NOT_AN_ENVELOPE"),
        None,
        "{path}: a non-envelope ORBIT_ name is privilege-bearing and must be omitted"
    );
    assert_eq!(
        value(env, "ORBIT_RUN_ID"),
        Some("jrun-inherit-privilege"),
        "{path}: an envelope name is not privilege-bearing and must still be forwarded"
    );
    assert_eq!(
        value(env, "DATABASE_URL"),
        Some("postgres://svc:hunter2@db.internal"),
        "{path}: inherit mode still forwards ambient credentials"
    );
    assert_eq!(
        value(env, "HOME"),
        Some("/home/inherit-fixture"),
        "{path}: inherit mode still forwards ordinary ambient names"
    );
}

#[test]
fn inherit_mode_omits_privilege_bearing_orbit_names() {
    const CHILD: &str = "ORBIT_CONFIG_INHERIT_PRIVILEGE_CHILD";
    const TEST: &str =
        "resolved::tests::execution_env::inherit_mode_omits_privilege_bearing_orbit_names";
    if std::env::var_os(CHILD).is_none() {
        let output = tempfile::tempdir().expect("child output directory");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1");
        let result = orbit_common::test_env::run_child_test(&mut command, TEST, output.path());
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            result.status,
            &result.stdout,
            &result.stderr,
        );
        return;
    }

    let _env = orbit_common::test_env::scoped([
        ("ORBIT_OPERATOR", Some("1")),
        ("ORBIT_WORKSPACE_CLAIM_TOKEN", Some("claim-token-fixture")),
        ("ORBIT_NOT_AN_ENVELOPE", Some("custom")),
        ("ORBIT_RUN_ID", Some("jrun-inherit-privilege")),
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("HOME", Some("/home/inherit-fixture")),
    ]);

    let mut snapshotted = ExecutionEnvPolicy::default();
    snapshotted.inherit = true;
    let snapshotted = snapshotted.with_defaults(&[]);
    assert_privilege_names_omitted(&snapshotted.agent_subprocess_env(&[]), "parent snapshot");

    let mut live = ExecutionEnvPolicy::default();
    live.inherit = true;
    assert_privilege_names_omitted(&live.agent_subprocess_env(&[]), "live parent");
}
