use std::io::{Read, Write};
use std::net::TcpListener;

use crate::update::source::{HttpReleaseSource, MAX_METADATA_BYTES, validated_release_url};

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
    if crate::tests::lib::run_isolated_test(std::any::type_name_of_val(
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
    if crate::tests::lib::run_isolated_test(std::any::type_name_of_val(
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
