use super::*;
use orbit_engine::Dispatch;

const COMPOSED: &str = "FIXTURE_COMPOSED_CODEX_OVERRIDE";
const AMBIENT: &str = "FIXTURE_AMBIENT_CODEX_OVERRIDE";

#[test]
fn composed_provider_override_reaches_executor_resolution() {
    if !isolated(
        "dispatch_admission::provider_override::composed_provider_override_reaches_executor_resolution",
    ) {
        return;
    }
    // The override is in the environment the runtime is composed with.
    let composed = orbit_common::test_env::scoped([("ORBIT_V2_CLI_CODEX", Some(COMPOSED))]);
    let (_root, runtime, _repo) = runtime();
    drop(composed);
    // The ambient environment then names another binary. Resolution keeps the
    // composed value instead of capturing the ambient environment again.
    let _ambient = orbit_common::test_env::scoped([("ORBIT_V2_CLI_CODEX", Some(AMBIENT))]);

    let executor = Dispatch::resolve_cli_executor(
        &orbit_core::test_support::EngineHost::new(&runtime),
        "codex",
    )
    .expect("the composed override resolves the codex executor");
    assert_eq!(executor.command, COMPOSED);
}
