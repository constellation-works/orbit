use orbit_tools::plugin::plugin_artifact_digest;
use orbit_types::plugin::{
    PLUGIN_BUILD_CONSENT_FLAG, PLUGIN_BUILD_PROFILE_LINUX, PluginBuildConsent,
};
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
fn commit_source() -> String {
    format!("git+https://example.test/demo.git#{COMMIT}")
}
use super::fixture::PluginFixture;
use crate::runtime::plugin::build_witness::record_build_witness;
use orbit_types::plugin::{PluginBuildOutputRecord, PluginBuildRecord};

pub(super) fn build_record(outputs: Vec<PluginBuildOutputRecord>) -> PluginBuildRecord {
    PluginBuildRecord {
        source: commit_source(),
        commit: COMMIT.to_string(),
        fetch: None,
        command: vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
        programs: Vec::new(),
        toolchain_roots: Vec::new(),
        profile: PLUGIN_BUILD_PROFILE_LINUX.to_string(),
        landlock_abi: None,
        artifact_digest: plugin_artifact_digest(&outputs),
        outputs,
        consent: PluginBuildConsent {
            at: "2026-10-04T00:00:00Z".to_string(),
            os_user: "operator".to_string(),
            orbit_version: env!("CARGO_PKG_VERSION").to_string(),
            flag: PLUGIN_BUILD_CONSENT_FLAG.to_string(),
        },
        log: String::new(),
    }
}

/// Record `build` on the row and in its witness, as a consented install does.
pub(super) fn attach_build(fixture: &PluginFixture, build: &PluginBuildRecord) {
    let mut installed = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read the row")
        .expect("the plugin is installed");
    installed.build = Some(build.clone());
    record_build_witness(&fixture.global_root, "demo", Some(build)).expect("write the witness");
    fixture
        .runtime
        .stores()
        .plugins()
        .upsert_plugin(&installed)
        .expect("record the build");
}
