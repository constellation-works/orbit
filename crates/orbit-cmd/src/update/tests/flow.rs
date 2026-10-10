use std::io::{Read, Write};
use std::net::TcpListener;

use orbit_common::OrbitError;
use orbit_common::fs::generation::executable_generation;
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME, sha256_hex,
};

use crate::update::flow::run_update_with_restore;
use crate::update::source::{
    HttpReleaseSource, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES, MAX_METADATA_BYTES,
    MAX_SIGNATURE_BYTES, MIRROR_LATEST_FILE, validated_release_url,
};
use crate::update::stage::restore_backup_with_rename;
use crate::update::tests::fixture::{FakeBinary, Fixture, request, tar_gz_named};
use crate::update::{UpdateOutcome, UpdateRequest, run_update};

// Fault injection: a failed rollback must retain the post-install verification
// evidence and distinguish the rejected candidate from the previous executable.
#[test]
fn installed_verification_failures_preserve_both_causes_when_restore_fails() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
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
            fixture.publish_archive(
                "0.19.0",
                &tar_gz_named(&[("orbit", candidate.as_bytes())]),
                true,
            );
            let backup = crate::update::flow::backup_path(&fixture.executable);
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

/// Read an HTTP request through its blank line. Closing a socket with request
/// bytes still unread makes the kernel send RST instead of FIN (macOS does this
/// reliably), and the client can then see "connection reset" before the
/// response the test server wrote.
fn read_request_head(stream: &mut impl Read) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("HTTP request head");
        head.push(byte[0]);
    }
    head
}

#[test]
fn http_rejects_declared_and_streamed_oversized_bodies() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &http_rejects_declared_and_streamed_oversized_bodies,
    )) {
        return;
    }

    let source = HttpReleaseSource::new("unused/repo".to_string());
    for declared in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local test server");
        let address = listener.local_addr().expect("server address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP request");
            assert!(read_request_head(&mut stream).starts_with(b"GET "));
            if declared {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    MAX_METADATA_BYTES + 1
                )
                .expect("response headers");
            } else {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                    .expect("response headers");
                stream
                    .write_all(&vec![b'x'; (MAX_METADATA_BYTES + 1) as usize])
                    .expect("response body");
            }
        });
        let error = source
            .get(
                &format!("http://{address}/release"),
                "metadata",
                MAX_METADATA_BYTES,
            )
            .expect_err("oversized HTTP body");
        server.join().expect("server completed");
        assert!(
            error.to_string().contains("65536-byte release input limit"),
            "{error}"
        );
    }
}

#[test]
fn release_requests_never_leave_https() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &release_requests_never_leave_https,
    )) {
        return;
    }

    for url in [
        "https://github.com/constellation-works/orbit/releases/download/v1.0.0/x.tar.gz",
        "https://api.github.com/repos/constellation-works/orbit/releases/latest",
    ] {
        validated_release_url(url).expect("GitHub release URL over HTTPS");
    }
    for url in [
        "http://github.com/constellation-works/orbit/releases",
        "https://example.com/constellation-works/orbit/releases",
    ] {
        validated_release_url(url).expect_err("release URL off HTTPS or off GitHub");
    }

    // A release host redirecting to plain HTTP is refused, not followed.
    let listener = TcpListener::bind("127.0.0.1:0").expect("local test server");
    let address = listener.local_addr().expect("server address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP request");
        read_request_head(&mut stream);
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: http://{address}/downgraded\r\nContent-Length: 0\r\n\r\n"
        )
        .expect("redirect response");
    });
    let error = HttpReleaseSource::new("unused/repo".to_string())
        .get(
            &format!("http://{address}/release"),
            "metadata",
            MAX_METADATA_BYTES,
        )
        .expect_err("plain-HTTP redirect");
    server.join().expect("server completed");
    assert!(
        error.to_string().contains("non-HTTPS release redirect"),
        "{error}"
    );
}

#[test]
fn oversized_mirror_inputs_fail_before_executable_replacement() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &oversized_mirror_inputs_fail_before_executable_replacement,
    )) {
        return;
    }

    let classes = [
        (MIRROR_LATEST_FILE, MAX_METADATA_BYTES),
        (RELEASE_CHECKSUMS_FILENAME, MAX_MANIFEST_BYTES),
        (RELEASE_CHECKSUMS_SIGNATURE_FILENAME, MAX_SIGNATURE_BYTES),
        ("archive", MAX_ARCHIVE_BYTES),
    ];
    for (asset_class, limit) in classes {
        let fixture = Fixture::new("0.18.0");
        fixture.publish("0.19.0", FakeBinary::Healthy);
        let asset = if asset_class == "archive" {
            crate::update::channel::release_archive_name(crate::update::tests::fixture::TEST_TARGET)
        } else {
            asset_class.to_string()
        };
        let input = fixture.mirror_input("0.19.0", &asset);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&input)
            .expect("published input")
            .set_len(limit + 1)
            .expect("oversized sparse input");
        let original = std::fs::read(&fixture.executable).expect("installed executable");
        let error =
            run_update(&fixture.environment(), &request()).expect_err("oversized mirror input");
        assert!(
            error
                .to_string()
                .contains(&format!("{limit}-byte release input limit")),
            "{asset}: {error}"
        );
        assert_eq!(
            std::fs::read(&fixture.executable).expect("installed executable"),
            original
        );
        assert!(!staging_file_remains(&fixture));
    }
}

#[test]
fn a_tampered_archive_fails_the_checksum_and_never_reaches_the_install_path() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &a_tampered_archive_fails_the_checksum_and_never_reaches_the_install_path,
    )) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    fixture.tamper_with_archive("0.19.0");

    let error = run_update(&fixture.environment(), &request()).expect_err("checksum mismatch");

    assert!(
        error.to_string().contains("checksum verification failed"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert!(!staging_file_remains(&fixture));
}

#[test]
fn an_archive_carrying_more_than_the_orbit_binary_is_rejected() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &an_archive_carrying_more_than_the_orbit_binary_is_rejected,
    )) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    let archive = tar_gz_named(&[
        ("orbit", b"#!/bin/sh\nexit 0\n".as_slice()),
        ("../evil", b"payload".as_slice()),
    ]);
    fixture.publish_archive("0.19.0", &archive, true);

    let error = run_update(&fixture.environment(), &request()).expect_err("extra member");

    assert!(error.to_string().contains("must contain only"), "{error}");
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    assert!(!staging_file_remains(&fixture));
}

#[test]
fn a_second_concurrent_update_is_refused_rather_than_queued() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &a_second_concurrent_update_is_refused_rather_than_queued,
    )) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let install_dir = fixture.executable.parent().expect("bin dir");
    let held = crate::update::lock::UpdateLock::acquire(install_dir).expect("first lock");

    let error = run_update(&fixture.environment(), &request()).expect_err("second update");

    assert!(
        error
            .to_string()
            .contains("another orbit update is already running"),
        "{error}"
    );
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
    drop(held);
    run_update(&fixture.environment(), &request()).expect("update after the lock is released");
}

fn staging_file_remains(fixture: &Fixture) -> bool {
    fixture
        .install_dir_entries()
        .iter()
        .any(|name| name.starts_with(".orbit-update-staged"))
}

#[test]
fn successful_read_only_inspection_cannot_authorize_a_downgrade() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &successful_read_only_inspection_cannot_authorize_a_downgrade,
    )) {
        return;
    }

    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.18.0", FakeBinary::ReadOnlyStore);
    let mut requested = request();
    requested.allow_downgrade = true;
    let before = std::fs::read(&fixture.executable).expect("old executable");
    run_update(&fixture.environment(), &requested).expect_err("read-only candidate refused");
    assert_eq!(
        std::fs::read(&fixture.executable).expect("old executable"),
        before
    );
}

#[test]
fn a_replacement_without_admission_protocol_is_refused_before_installation() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &a_replacement_without_admission_protocol_is_refused_before_installation,
    )) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::NoAdmissionContract);
    let before = std::fs::read(&fixture.executable).expect("installed executable");
    let error = run_update(&fixture.environment(), &request()).expect_err("unprotected candidate");
    assert!(
        error
            .to_string()
            .contains("does not support executable generation admission")
    );
    assert_eq!(
        std::fs::read(&fixture.executable).expect("installed executable"),
        before
    );
    assert!(fixture.invocations().is_empty());
}

#[test]
fn an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release,
    )) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    let mut environment = fixture.environment();
    environment.bundled_bwrap_installed = true;

    let report = run_update(&environment, &request()).expect("update");

    assert_eq!(report.exit_code(), 0, "{report:?}");
    let refresh = "0.19.0: init --host-prerequisites-only --non-interactive";
    assert_eq!(
        fixture.invocations().last().map(String::as_str),
        Some(refresh),
        "the replacement executable must refresh the bundled Bubblewrap, without --root"
    );

    // A host without one installed never has its Bubblewrap touched.
    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::Healthy);
    run_update(&fixture.environment(), &request()).expect("update");
    assert!(
        !fixture
            .invocations()
            .iter()
            .any(|line| line.contains("init")),
        "{:?}",
        fixture.invocations()
    );
}

fn parse_retry_request_from_recovery(report: &crate::update::UpdateReport) -> UpdateRequest {
    let recovery = report.recovery.as_deref().expect("recovery text");
    let marker = "Re-run `";
    let start = recovery.find(marker).expect("Re-run marker") + marker.len();
    let end = recovery[start..].find('`').expect("closing backtick") + start;
    let command = &recovery[start..end];
    let mut request = UpdateRequest::default();
    let words: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        if words[i] == "--version" && i + 1 < words.len() {
            request.target_version = Some(words[i + 1].to_string());
            i += 2;
        } else if words[i] == "--allow-downgrade" {
            request.allow_downgrade = true;
            i += 1;
        } else {
            i += 1;
        }
    }
    request
}

#[test]
fn recovery_hint_targets_requested_version_when_newer_release_is_published() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &recovery_hint_targets_requested_version_when_newer_release_is_published,
    )) {
        return;
    }

    let fixture = Fixture::new("0.18.0");
    fixture.publish("0.19.0", FakeBinary::FailsMigrationConfirm);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.target_version = Some("0.19.0".to_string());
    let report = run_update(&fixture.environment(), &requested).expect("update");

    assert_eq!(report.outcome, UpdateOutcome::NeedsRecovery);
    assert_eq!(report.target_version, "0.19.0");
    assert!(report.replaced);

    let retry_request = parse_retry_request_from_recovery(&report);
    assert_eq!(retry_request.target_version.as_deref(), Some("0.19.0"));
    assert!(!retry_request.allow_downgrade);

    fixture.repair_migration();
    let retry_report = run_update(&fixture.environment(), &retry_request).expect("retry update");

    assert_eq!(retry_report.outcome, UpdateOutcome::AlreadyCurrent);
    assert_eq!(retry_report.target_version, "0.19.0");
    assert_eq!(fixture.installed_reports(), "orbit 0.19.0");
}

#[test]
fn recovery_hint_preserves_allow_downgrade_when_retrying_older_release() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &recovery_hint_preserves_allow_downgrade_when_retrying_older_release,
    )) {
        return;
    }

    let fixture = Fixture::new("0.19.0");
    fixture.publish("0.18.0", FakeBinary::FailsMigrationConfirm);
    fixture.publish("0.20.0", FakeBinary::Healthy);

    let mut requested = request();
    requested.target_version = Some("0.18.0".to_string());
    requested.allow_downgrade = true;
    let report = run_update(&fixture.environment(), &requested).expect("downgrade update");

    assert_eq!(report.outcome, UpdateOutcome::NeedsRecovery);
    assert_eq!(report.target_version, "0.18.0");
    assert_eq!(report.current_version, "0.19.0");
    assert!(report.replaced);

    let retry_request = parse_retry_request_from_recovery(&report);
    assert_eq!(retry_request.target_version.as_deref(), Some("0.18.0"));
    assert!(retry_request.allow_downgrade);

    fixture.repair_migration();
    let retry_report = run_update(&fixture.environment(), &retry_request).expect("retry downgrade");

    assert_eq!(retry_report.outcome, UpdateOutcome::AlreadyCurrent);
    assert_eq!(retry_report.target_version, "0.18.0");
    assert_eq!(fixture.installed_reports(), "orbit 0.18.0");
}
