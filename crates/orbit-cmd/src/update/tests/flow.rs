use std::io::{Read, Write};
use std::net::TcpListener;

use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME,
};

use crate::update::run_update;
use crate::update::source::{
    HttpReleaseSource, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES, MAX_METADATA_BYTES,
    MAX_SIGNATURE_BYTES, MIRROR_LATEST_FILE, validated_release_url,
};
use crate::update::tests::fixture::{FakeBinary, Fixture, request, tar_gz_named};

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
    let error =
        run_update(&fixture.environment(), &requested).expect_err("read-only candidate refused");
    assert!(error.to_string().contains("Read-only inspection success"));
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
