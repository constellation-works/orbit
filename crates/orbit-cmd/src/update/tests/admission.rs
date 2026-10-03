use std::path::{Path, PathBuf};

use orbit_common::fs::generation::GenerationGuard;

use crate::update::tests::fixture::{FakeBinary, Fixture, request};
use crate::update::{UpdateEnvironment, UpdateOutcome, run_update};

/// Both authorities an override splits the invocation into, in the order
/// `admission_authorities` builds them.
fn split_authorities(environment: &mut UpdateEnvironment) -> (PathBuf, PathBuf) {
    let host_global = environment.admission_roots[0].clone();
    std::fs::create_dir_all(&host_global).expect("host-global generation root");
    let override_root = host_global
        .parent()
        .expect("fixture root")
        .join("override-root");
    std::fs::create_dir_all(&override_root).expect("override generation root");
    environment.admission_roots = vec![override_root.clone(), host_global.clone()];
    (override_root, host_global)
}

#[test]
fn live_client_on_overridden_root_refuses_update_while_host_global_is_quiet() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    let (override_root, _host_global) = split_authorities(&mut environment);
    let _client = live_client(&override_root).expect("live client on override");
    let before = std::fs::read(&fixture.executable).expect("installed bytes");
    let error = run_update(&environment, &request()).expect_err("override pin must refuse");
    let message = error.to_string();
    assert!(message.contains("upgrade admission refused"), "{message}");
    assert!(
        message.contains(&override_root.display().to_string()),
        "the refusal must name the authority holding the pin: {message}"
    );
    assert_eq!(
        std::fs::read(&fixture.executable).expect("installed bytes"),
        before
    );
    assert!(fixture.invocations().is_empty());
}

/// The binary an override-rooted update replaces is still the host-global one,
/// so a client pinned there refuses it just as one pinned on the override does.
#[test]
fn live_host_global_client_refuses_an_update_admitting_against_an_override() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    let (_override_root, host_global) = split_authorities(&mut environment);
    let _host_client = live_client(&host_global).expect("live host-global pin");
    let before = std::fs::read(&fixture.executable).expect("installed bytes");
    let error = run_update(&environment, &request()).expect_err("host-global pin must refuse");
    let message = error.to_string();
    assert!(message.contains("upgrade admission refused"), "{message}");
    assert!(
        message.contains(&host_global.display().to_string()),
        "the refusal must name the authority holding the pin: {message}"
    );
    assert_eq!(
        std::fs::read(&fixture.executable).expect("installed bytes"),
        before
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert!(fixture.invocations().is_empty());
}

/// With both authorities quiet the update proceeds, and each one records the
/// candidate: a host-global record left naming the replaced generation would
/// strand every later host-global process behind a mismatch.
#[test]
fn an_override_update_pins_the_candidate_in_every_authority_it_locked() {
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    let (override_root, host_global) = split_authorities(&mut environment);

    let report = run_update(&environment, &request()).expect("both authorities are quiet");

    assert_eq!(report.outcome, UpdateOutcome::Updated);
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
    let installed = orbit_common::fs::generation::executable_generation(&fixture.executable)
        .expect("installed digest");
    for root in [override_root, host_global] {
        assert_eq!(
            std::fs::read_to_string(root.join(".generation.lock")).expect("generation record"),
            format!("1:{installed}\n"),
            "{} does not record the installed generation",
            root.display()
        );
    }
}

/// A host-global record that cannot be rewritten has to refuse the run up
/// front. Writability belongs to the record rather than the lock, so a check
/// deferred to `pin` would only run once the executable had been replaced,
/// leaving the host-global authority naming a generation that no longer
/// exists and cannot be corrected from here.
#[cfg(unix)]
#[test]
fn an_unwritable_host_global_record_refuses_an_override_update_before_staging() {
    const RECORDED: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    let (_override_root, host_global) = split_authorities(&mut environment);
    drop(GenerationGuard::acquire(&host_global, RECORDED).expect("host-global record"));
    let record = host_global.join(".generation.lock");
    let _frozen = ReadOnlyFile::freeze(record.clone());
    let before = std::fs::read(&fixture.executable).expect("installed bytes");

    let error =
        run_update(&environment, &request()).expect_err("an unwritable authority must refuse");

    let message = error.to_string();
    assert!(message.contains("cannot be written from here"), "{message}");
    assert!(
        message.contains(&host_global.display().to_string()),
        "the refusal must name the authority that cannot record the candidate: {message}"
    );
    assert_eq!(
        std::fs::read(&fixture.executable).expect("installed bytes"),
        before,
        "the executable must not be replaced"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert_eq!(fixture.install_dir_entries(), vec!["orbit".to_string()]);
    assert!(fixture.invocations().is_empty());
    assert_eq!(
        std::fs::read_to_string(&record).expect("generation record"),
        format!("1:{RECORDED}\n"),
        "the refused run must leave the record it could not write intact"
    );
}

/// A record frozen for the lifetime of one test, thawed on drop so the
/// fixture's temporary directory can still be removed.
#[cfg(unix)]
struct ReadOnlyFile {
    path: PathBuf,
}

#[cfg(unix)]
impl ReadOnlyFile {
    fn freeze(path: PathBuf) -> Self {
        chmod(&path, 0o444);
        Self { path }
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyFile {
    fn drop(&mut self) {
        chmod(&self.path, 0o644);
    }
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("metadata {}: {error}", path.display()))
        .permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
}

#[test]
fn live_generation_refuses_before_installation_or_candidate_execution() {
    for behavior in [
        FakeBinary::Healthy,
        FakeBinary::MigrationFails,
        FakeBinary::VersionMismatch,
    ] {
        let fixture = Fixture::new("0.18.0");
        fixture.publish("0.19.0", behavior);
        let environment = fixture.environment();
        let _client = live_client(&environment.admission_roots[0]).expect("live client");
        let before = std::fs::read(&fixture.executable).expect("installed bytes");
        let error = run_update(&environment, &request()).expect_err("live clients refuse");
        assert!(error.to_string().contains("upgrade admission refused"));
        assert_eq!(
            std::fs::read(&fixture.executable).expect("installed bytes"),
            before
        );
        assert!(fixture.invocations().is_empty());
        assert_eq!(fixture.install_dir_entries(), vec!["orbit".to_string()]);
    }
}

/// A live Orbit client pinned on `root`, as this test binary.
fn live_client(root: &Path) -> Result<GenerationGuard, orbit_common::OrbitError> {
    GenerationGuard::acquire(root, orbit_common::fs::generation::process_generation()?)
}
