//! Install-time `spec.build` at the install boundary
//! (`docs/design/plugins/3_install_time_build.md`): consent is refused
//! without the flag and in unattended processes, a pin never builds, a
//! non-commit source must ship its outputs, and an installed build's record
//! is checked at load and by doctor.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::plugin_artifact_digest;
use orbit_types::plugin::{
    PLUGIN_BUILD_CONSENT_FLAG, PLUGIN_BUILD_PROFILE_LINUX, PluginBuildConsent,
    PluginBuildOutputRecord, PluginBuildRecord, PluginStatus,
};

use super::super::{PluginAddOptions, install_plugin, plugin_doctor, show_plugin, sync_plugins};
use super::fixture::PluginFixture;
use crate::runtime::plugin::build_witness::{forget_build_witness, record_build_witness};
use crate::runtime::plugin::paths::plugin_namespace_dir;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn commit_source() -> String {
    format!("git+https://example.test/demo.git#{COMMIT}")
}

/// A plugin whose backend only exists once `spec.build` has run.
const BUILD_MANIFEST: &str = r#"schemaVersion: 2
kind: Plugin
metadata:
  name: demo
  version: 1.2.3
  description: Built fixture.
spec:
  backend:
    type: exec
    command: bin/backend
  build:
    programs: [sh]
    command: [sh, -c, "mkdir -p {{build_dir}}/out && printf '#!/bin/sh\\n' > {{build_dir}}/out/backend"]
    outputs:
      - from: out/backend
        to: bin/backend
  tools:
    - name: hello
      description: Hello.
      execution_kind: read_only
"#;

/// Re-run `test` with a `git` on `PATH` that "fetches" [`COMMIT`] into a
/// checkout holding [`BUILD_MANIFEST`], plus `extra_env`.
#[cfg(unix)]
fn enter_fake_git_commit_child(test: &str, extra_env: &[(&str, &str)]) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_PLUGIN_GIT_COMMIT_CHILD")
        .ok()
        .as_deref()
        == Some(&exact_test)
    {
        return true;
    }

    let temp = tempfile::tempdir().expect("fake git directory");
    let manifest = temp.path().join("plugin.yaml");
    std::fs::write(&manifest, BUILD_MANIFEST).expect("write fixture manifest");
    let git = temp.path().join("git");
    std::fs::write(
        &git,
        format!(
            r#"#!/bin/sh
set -eu
while [ "$1" = "-c" ]; do shift 2; done
case "$1" in
  init) for arg in "$@"; do dir=$arg; done; mkdir -p "$dir/.git" ;;
  fetch) ;;
  checkout) mkdir -p .orbit-plugin && cp '{}' .orbit-plugin/plugin.yaml ;;
  rev-parse) echo {COMMIT} ;;
  show) echo 1700000000 ;;
  *) echo "unexpected git $*" >&2; exit 1 ;;
esac
"#,
            manifest.display()
        ),
    )
    .expect("write fake git");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
        .expect("make fake git executable");
    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut child = Command::new(std::env::current_exe().expect("test executable"));
    super::fixture::clear_child_authority(&mut child);
    let output = child
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_GIT_COMMIT_CHILD", &exact_test)
        .env("PATH", std::env::join_paths(paths).expect("fake git PATH"))
        .envs(extra_env.iter().copied())
        .output()
        .expect("run isolated fake-git build test");
    super::fixture::assert_child_passed(&output, &exact_test);
    false
}

fn assert_nothing_installed(fixture: &PluginFixture) {
    assert!(
        fixture
            .runtime
            .stores()
            .plugins()
            .list_plugins()
            .expect("list plugin rows")
            .is_empty(),
        "a refused build must leave no plugin row"
    );
    let namespace = plugin_namespace_dir(&fixture.global_root, "demo");
    assert!(
        std::fs::read_dir(&namespace).map_or(true, |mut entries| entries.next().is_none()),
        "a refused build must leave nothing under {}",
        namespace.display()
    );
}

/// §3.8: without `--allow-build` a building source is refused before any
/// phase runs, with the typed error and nothing written.
#[cfg(unix)]
#[test]
fn a_building_source_is_refused_without_consent() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_building_source_is_refused_without_consent",
    ) {
        return;
    }
    if !enter_fake_git_commit_child("a_building_source_is_refused_without_consent", &[]) {
        return;
    }
    let fixture = PluginFixture::new();

    let error = install_plugin(
        &fixture.runtime,
        &commit_source(),
        &PluginAddOptions::default(),
    )
    .expect_err("a build needs explicit consent");

    assert!(
        matches!(error, OrbitError::PluginBuildConsentRequired(_)),
        "unexpected refusal: {error:?}"
    );
    assert_nothing_installed(&fixture);
}

/// §3.8: a managed run cannot consent, even when it passes the flag.
#[cfg(unix)]
#[test]
fn consent_is_refused_inside_a_managed_run() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "consent_is_refused_inside_a_managed_run",
    ) {
        return;
    }
    if !enter_fake_git_commit_child(
        "consent_is_refused_inside_a_managed_run",
        &[("ORBIT_RUN_ID", "run-under-test")],
    ) {
        return;
    }
    let fixture = PluginFixture::new();

    let error = install_plugin(
        &fixture.runtime,
        &commit_source(),
        &PluginAddOptions {
            allow_build: true,
            ..PluginAddOptions::default()
        },
    )
    .expect_err("a managed run is no operator");

    assert!(
        matches!(error, OrbitError::PluginBuildConsentUnavailable(_)),
        "unexpected refusal: {error:?}"
    );
    assert_nothing_installed(&fixture);
}

/// §3.7: a committed pin whose source would build is reported unsatisfied
/// by sync, and nothing is built or installed.
#[cfg(unix)]
#[test]
fn a_pin_never_starts_a_build() {
    if !super::fixture::enter_isolated_child(module_path!(), "a_pin_never_starts_a_build") {
        return;
    }
    if !enter_fake_git_commit_child("a_pin_never_starts_a_build", &[]) {
        return;
    }
    let fixture = PluginFixture::new();
    write_pins(
        &fixture,
        &format!("  - name: demo\n    source: \"{}\"\n", commit_source()),
    );

    let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, PluginStatus::Missing);
    assert_nothing_installed(&fixture);
}

/// §3.1: a source that never builds installs a `spec.build` manifest only
/// when every declared output already ships, and records no build.
#[test]
fn a_non_commit_source_needs_its_outputs_prebuilt() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_non_commit_source_needs_its_outputs_prebuilt",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = write_build_source(&fixture, false);

    let error = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            allow_build: true,
            ..PluginAddOptions::default()
        },
    )
    .expect_err("the output is missing and a directory never builds");
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "unexpected refusal: {error:?}"
    );
    assert_nothing_installed(&fixture);

    write_prebuilt_backend(&source);
    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("a prebuilt tree installs without consent");
    assert_eq!(summary.build, None);
}

/// §3.2–§3.6 end to end: a consented build runs in the build sandbox, the
/// install keeps only the declared output, and the record and witness agree
/// so the plugin loads. Needs a host where the build profile can run.
#[cfg(target_os = "linux")]
#[test]
fn a_consented_build_installs_its_output_with_a_record() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_consented_build_installs_its_output_with_a_record",
    ) {
        return;
    }
    if !enter_fake_git_commit_child("a_consented_build_installs_its_output_with_a_record", &[]) {
        return;
    }
    // A host that cannot apply the build profile refuses every build; the
    // refusal is the probe's own concern, not this test's.
    if orbit_exec::probe_build_sandbox(false).is_err() {
        return;
    }
    let fixture = PluginFixture::new();

    let summary = install_plugin(
        &fixture.runtime,
        &commit_source(),
        &PluginAddOptions {
            allow_build: true,
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("a consented build installs");

    let build = summary.build.expect("the install records its build");
    assert_eq!(build.commit, COMMIT);
    assert_eq!(build.profile, PLUGIN_BUILD_PROFILE_LINUX);
    assert_eq!(
        build
            .outputs
            .iter()
            .map(|output| (output.to.as_str(), output.mode))
            .collect::<Vec<_>>(),
        vec![("bin/backend", 0o755)]
    );
    assert_eq!(
        orbit_tools::plugin::installed_artifact_digest(
            Path::new(&summary.install_path),
            &build.outputs
        ),
        Ok(build.artifact_digest.clone()),
        "the recorded digest is the installed output's"
    );
    assert_eq!(
        show_plugin(&fixture.reopen(), "demo").expect("show").status,
        PluginStatus::Active,
        "the row and its witness agree"
    );
}

/// §3.6 and §3.9: an enabled row's build record must match its witness, or
/// it registers inactive; doctor shows the build and flags outputs changed
/// since it, and a pin naming a different artifact digest.
#[test]
fn an_installed_build_record_is_checked_at_load_and_by_doctor() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "an_installed_build_record_is_checked_at_load_and_by_doctor",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = write_build_source(&fixture, true);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install the prebuilt plugin");

    // Stand in for a build that ran: a record with no outputs digests to a
    // value the installed tree reproduces.
    let record = build_record(Vec::new());
    attach_build(&fixture, &record);
    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "demo").expect("show").status,
        PluginStatus::Active
    );
    let rows = plugin_doctor(&runtime).expect("doctor");
    assert!(
        rows.iter()
            .any(|row| row.plugin == "demo" && row.intentional),
        "a built plugin gets an informational row: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.plugin == "demo" && !row.intentional && !row.message.is_empty()),
        "an intact build is no finding: {rows:?}"
    );

    // Outputs that no longer hash to the recorded digest are a finding.
    let tampered = build_record(vec![PluginBuildOutputRecord {
        to: "bin/backend".to_string(),
        mode: 0o755,
        sha256: "0".repeat(64),
    }]);
    attach_build(&fixture, &tampered);
    let runtime = fixture.reopen();
    assert_eq!(
        build_findings(&runtime),
        1,
        "the modified output is reported"
    );

    // A pin expecting other bytes is a finding, and sync leaves it
    // unsatisfied rather than rebuilding.
    attach_build(&fixture, &record);
    write_pins(
        &fixture,
        &format!(
            "  - name: demo\n    source: \"{}\"\n    artifact_digest: \"sha256:{}\"\n",
            commit_source(),
            "f".repeat(64)
        ),
    );
    let runtime = fixture.reopen();
    assert_eq!(build_findings(&runtime), 1, "the pin drift is reported");
    let outcomes = sync_plugins(&runtime, false, &[]).expect("sync");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        stored_build(&fixture),
        Some(record.clone()),
        "sync must not rebuild or rewrite an installed build"
    );

    // A record whose witness is gone is refused at load.
    forget_build_witness(&fixture.global_root, "demo");
    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "demo").expect("show").status,
        PluginStatus::Inactive
    );
}

fn build_findings(runtime: &crate::OrbitRuntime) -> usize {
    plugin_doctor(runtime)
        .expect("doctor")
        .iter()
        .filter(|row| row.plugin == "demo" && !row.intentional && !row.message.is_empty())
        .count()
}

fn write_pins(fixture: &PluginFixture, entries: &str) {
    std::fs::write(
        fixture.runtime.shared_root().join("plugins.yaml"),
        format!("schemaVersion: 1\nplugins:\n{entries}"),
    )
    .expect("write the pin file");
}

/// A directory source declaring [`BUILD_MANIFEST`], with or without its
/// output already in place.
fn write_build_source(fixture: &PluginFixture, prebuilt: bool) -> std::path::PathBuf {
    let source = fixture.sources.join("demo");
    let root = orbit_types::plugin::plugin_root_in(&source);
    std::fs::create_dir_all(&root).expect("create plugin root");
    std::fs::write(root.join("plugin.yaml"), BUILD_MANIFEST).expect("write manifest");
    if prebuilt {
        write_prebuilt_backend(&source);
    }
    source
}

fn write_prebuilt_backend(source: &Path) {
    let bin = orbit_types::plugin::plugin_root_in(source).join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let backend = bin.join("backend");
    std::fs::write(&backend, "#!/bin/sh\n").expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
}

fn build_record(outputs: Vec<PluginBuildOutputRecord>) -> PluginBuildRecord {
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

fn stored_build(fixture: &PluginFixture) -> Option<PluginBuildRecord> {
    fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read the row")
        .and_then(|installed| installed.build)
}

/// Record `build` on the row and in its witness, as a consented install does.
fn attach_build(fixture: &PluginFixture, build: &PluginBuildRecord) {
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
