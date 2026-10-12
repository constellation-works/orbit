use std::io::{Read, Write};
use std::net::TcpListener;

use orbit_common::OrbitError;
use orbit_common::fs::generation::executable_generation;
use orbit_common::security::release::sha256_hex;

use crate::update::flow::run_update_with_restore;
use crate::update::source::{HttpReleaseSource, MAX_METADATA_BYTES, validated_release_url};
use crate::update::stage::restore_backup_with_rename;
use crate::update::tests::fixture::{Fixture, request, tar_gz_named};

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
            fixture.publish_archive("0.19.0", &tar_gz_named(&[("orbit", candidate.as_bytes())]));
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

fn staging_file_remains(fixture: &Fixture) -> bool {
    fixture
        .install_dir_entries()
        .iter()
        .any(|name| name.starts_with(".orbit-update-staged"))
}
