use orbit_common::OrbitError;
use orbit_common::fs::generation::executable_generation;
use orbit_common::security::release::sha256_hex;

use crate::app::update::flow::{backup_path, run_update_with_restore};
use crate::app::update::stage::restore_backup_with_rename;
use crate::app::update::tests::fixture::{Fixture, request, tar_gz_named};

// Fault injection: a failed rollback must retain the post-install verification
// evidence and distinguish the rejected candidate from the previous executable.
#[test]
fn installed_verification_failures_preserve_both_causes_when_restore_fails() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &installed_verification_failures_preserve_both_causes_when_restore_fails,
    )) {
        return;
    }

    let probe_failure = "injected installed-version probe failure";
    for wrong_version in [false, true] {
        for restore_fails in [false, true] {
            let fixture = Fixture::new("0.18.0");
            let previous = executable_generation(&fixture.executable).expect("previous digest");
            let installed_probe = if wrong_version {
                "echo 'orbit 0.17.0'; exit 0".to_string()
            } else {
                format!("echo '{probe_failure}' >&2; exit 1")
            };
            let candidate = format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = --version ]; then\n\
                   case \"${{0##*/}}\" in orbit) {installed_probe};; esac\n\
                   echo 'orbit 0.19.0'; exit 0\n\
                 fi\n\
                 if [ \"$1\" = update ] && [ \"$2\" = --contract ]; then\n\
                   echo '{{\"schema_version\":1,\"contract\":\"executable-generation-v1\"}}'; exit 0\n\
                 fi\n\
                 exit 1\n"
            );
            let candidate_digest = sha256_hex(candidate.as_bytes());
            fixture.publish_archive("0.19.0", &tar_gz_named(&[("orbit", candidate.as_bytes())]));
            let backup = backup_path(&fixture.executable);
            let restore_failure = std::io::Error::from(std::io::ErrorKind::StorageFull).to_string();
            let mut restore_attempted = false;
            let error =
                run_update_with_restore(&fixture.environment(), &request(), |dest, from| {
                    restore_attempted = true;
                    assert_eq!(dest, fixture.executable);
                    assert_eq!(from, backup);
                    assert_eq!(
                        executable_generation(dest).expect("candidate digest"),
                        candidate_digest
                    );
                    assert_eq!(
                        executable_generation(from).expect("backup digest"),
                        previous
                    );
                    restore_backup_with_rename(dest, from, |staged, installed| {
                        if restore_fails {
                            Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
                        } else {
                            std::fs::rename(staged, installed)
                        }
                    })
                })
                .expect_err("installed verification must reject the candidate");
            assert!(restore_attempted);
            let OrbitError::Execution(message) = error else {
                panic!("verification failure must remain an execution error: {error}");
            };
            if wrong_version {
                for version in ["0.19.0", "0.17.0"] {
                    assert!(
                        message.contains(version),
                        "missing mismatched version: {message}"
                    );
                }
            } else {
                assert!(
                    message.contains(probe_failure),
                    "lost probe cause: {message}"
                );
            }
            if restore_fails {
                assert!(
                    message.contains(&restore_failure),
                    "lost restore cause: {message}"
                );
                assert!(
                    message.contains("rejected candidate remains installed"),
                    "{message}"
                );
                for path in [&fixture.executable, &backup] {
                    assert!(
                        message.contains(&path.display().to_string()),
                        "missing recovery path: {message}"
                    );
                }
            } else {
                assert!(
                    !message.contains(&restore_failure),
                    "successful restore: {message}"
                );
                assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
            }
            assert_eq!(
                executable_generation(&fixture.executable).expect("installed digest"),
                if restore_fails {
                    &candidate_digest
                } else {
                    &previous
                }
                .as_str()
            );
            assert_eq!(
                executable_generation(&backup).expect("retained backup digest"),
                previous
            );
            assert!(
                fixture.invocations().is_empty(),
                "no convergence after rejection"
            );
            assert!(
                fixture
                    .install_dir_entries()
                    .iter()
                    .all(|name| !name.starts_with(".orbit-update-restore"))
            );
            assert!(!staging_file_remains(&fixture));
        }
    }
}

fn staging_file_remains(fixture: &Fixture) -> bool {
    fixture
        .install_dir_entries()
        .iter()
        .any(|name| name.starts_with(".orbit-update-staged"))
}
