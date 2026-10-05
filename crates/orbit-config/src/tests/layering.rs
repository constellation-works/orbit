//! Global-over-workspace layering.

use tempfile::tempdir;

use super::{roots, write_config};
use crate::{ConfigSnapshot, ResolvedConfig};

#[test]
fn workspace_file_does_not_inherit_security_relevant_global_keys() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        r#"
[execution.codex]
sandbox = "danger-full-access"
approval_policy = "on-request"

[execution.env]
inherit = true
pass = ["GLOBAL_SECRET"]
"#,
    );
    write_config(workspace.path(), "[scoring]\nenabled = false\n");

    let config = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect("workspace config loads");

    assert_eq!(config.codex_execution.sandbox(), "workspace-write");
    assert_eq!(config.codex_execution.approval_policy(), None);
    assert_eq!(
        config.snapshot.execution_env_pass,
        ConfigSnapshot::default().execution_env_pass
    );
    assert!(!config.execution_env.inherit());
}

#[test]
fn workspace_crew_field_override_keeps_global_crew_fields_and_other_crews() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        r#"
[workflow]
default_crew = "build"

[crews.build]
model = "global-model"
provider = "codex"
backend = "cli"

[crews.review]
model = "review-model"
provider = "claude"
backend = "cli"
"#,
    );
    write_config(
        workspace.path(),
        r#"
[crews.build]
model = "workspace-model"
"#,
    );

    let config = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect("layered crew config loads");

    let build = config.crews.get("build").expect("overridden crew remains");
    assert_eq!(build.assignment.model, "workspace-model");
    assert_eq!(build.assignment.provider, "codex");
    assert_eq!(
        config
            .crews
            .get("review")
            .expect("global-only crew remains")
            .assignment
            .model,
        "review-model"
    );
}
