//! Unit tests for `install` — sibling layout under commands/tests/.

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
)))]
use super::super::install::path_execution_fallback_rationale;
use super::super::install::{
    COMPANION_STALLED_READ_TIMEOUT, CompanionIntegrity, CompanionLaunchMode, ManagedCompanion,
    SemanticInstallParams, checksum_from_manifest, companion_download_client,
    companion_launch_mode, default_release_download_source, download_companion_to_temp,
    install_companion_to_temp, run, sha256_hex,
};

use orbit_common::security::release::verify_checksum_signature_with_key;

use crate::companion::{
    ensure_semantic_search_supported_for_platform, unsafe_companion_overrides_enabled,
};
use crate::{CompanionPaths, locate_companion, platform_companion_filename};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

#[test]
fn companion_install_streams_a_slow_one_mebibyte_download_without_a_total_timeout() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    let body = vec![b'x'; 1024 * 1024];
    let checksum = sha256_hex(&body);
    let url = serve_throttled_response(body, Duration::from_millis(2_400));
    let temp_path = fixture.paths.bin_dir.join("downloaded-companion");

    remove_env("ORBIT_SEARCH_COMPANION");
    set_env("NO_PROXY", "127.0.0.1,localhost");
    set_env("no_proxy", "127.0.0.1,localhost");
    set_env("ORBIT_SEARCH_COMPANION_URL", &url);
    set_env("ORBIT_SEARCH_COMPANION_SHA256", &checksum);
    set_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE", "1");

    let started = Instant::now();
    let actual_checksum = install_companion_to_temp(&temp_path)
        .expect("a healthy response taking more than 35 seconds should install");

    assert!(
        started.elapsed() > Duration::from_secs(35),
        "the fixture must outlast the idle bound so only a total deadline could reject it"
    );
    assert_eq!(actual_checksum, checksum);
    assert_eq!(
        std::fs::metadata(temp_path)
            .expect("download metadata")
            .len(),
        1024 * 1024
    );
}

#[test]
fn companion_download_bounds_a_stalled_response_after_partial_progress() {
    let _guard = EnvGuard::new();
    let temp = tempdir().expect("tempdir");
    let url = serve_stalled_response();

    set_env("NO_PROXY", "127.0.0.1,localhost");
    set_env("no_proxy", "127.0.0.1,localhost");

    let client = companion_download_client().expect("configure companion download");
    let started = Instant::now();
    let error = download_companion_to_temp(&client, &url, &temp.path().join("companion"))
        .expect_err("a response that stops sending body bytes must not block forever");
    let elapsed = started.elapsed();

    assert!(
        error
            .to_string()
            .contains("failed to read companion download"),
        "{error}"
    );
    assert!(
        elapsed >= COMPANION_STALLED_READ_TIMEOUT,
        "the reads that did succeed must not count against the idle bound; failed after {elapsed:?}"
    );
    assert!(
        elapsed < COMPANION_STALLED_READ_TIMEOUT * 3,
        "the stalled read must fail near {COMPANION_STALLED_READ_TIMEOUT:?}; took {elapsed:?}"
    );
}

#[test]
fn companion_install_reports_a_dead_connection() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve local port");
    let address = listener.local_addr().expect("listener address");
    drop(listener);
    let temp_path = fixture.paths.bin_dir.join("downloaded-companion");

    remove_env("ORBIT_SEARCH_COMPANION");
    set_env(
        "ORBIT_SEARCH_COMPANION_URL",
        &format!("https://{address}/companion"),
    );
    set_env("ORBIT_SEARCH_COMPANION_SHA256", &"0".repeat(64));

    let error = install_companion_to_temp(&temp_path)
        .expect_err("a closed local port should fail the connection");

    assert!(
        error.to_string().contains("failed to download companion"),
        "{error}"
    );
}

/// Serves `body` in 64 KiB chunks separated by `delay_between_chunks`, so the
/// whole transfer takes far longer than any single quiet period.
///
/// The listener stays open and accepts in a loop (see
/// [`accept_real_request`]) rather than a single `accept()`, because reqwest
/// may open and drop a pre-flight/probe connection before the connection
/// that actually carries the request (ORB-12141 / F2026-09-080): a
/// single-accept fixture races that probe for the one accepted stream and,
/// under concurrent load that widens the race window, loses it often enough
/// to flake `cargo test` runs that share the host with other jobs.
fn serve_throttled_response(body: Vec<u8>, delay_between_chunks: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind throttled HTTP server");
    let address = listener.local_addr().expect("throttled server address");

    thread::spawn(move || {
        let Some(mut stream) = accept_real_request(&listener) else {
            return;
        };
        std::io::Write::write_all(
            &mut stream,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .expect("write response headers");

        for (index, chunk) in body.chunks(64 * 1024).enumerate() {
            if index != 0 {
                thread::sleep(delay_between_chunks);
            }
            std::io::Write::write_all(&mut stream, chunk).expect("write throttled response chunk");
        }
    });

    format!("http://{address}/companion")
}

/// Serves response headers and one 64 KiB chunk of a body that promises one
/// more byte, then holds the socket open without ever sending it. The client
/// must therefore observe a stalled read rather than a premature EOF.
///
/// See [`serve_throttled_response`] for why this loops on accept rather than
/// taking exactly one connection.
fn serve_stalled_response() -> String {
    const DELIVERED_PREFIX: usize = 64 * 1024;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stalled HTTP server");
    let address = listener.local_addr().expect("stalled server address");

    thread::spawn(move || {
        let Some(mut stream) = accept_real_request(&listener) else {
            return;
        };
        std::io::Write::write_all(
            &mut stream,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                DELIVERED_PREFIX + 1
            )
            .as_bytes(),
        )
        .expect("write response headers");
        std::io::Write::write_all(&mut stream, &vec![b'x'; DELIVERED_PREFIX])
            .expect("write partial response body");

        // Outlast the assertion window by a wide margin: an unbounded client
        // must be seen hanging, not rescued by the fixture closing the socket.
        thread::sleep(COMPANION_STALLED_READ_TIMEOUT * 6);
    });

    format!("http://{address}/companion")
}

/// Accepts connections until one sends a complete request head, then returns
/// that stream. Connections that never complete a head — a dropped probe, a
/// connect that the client abandons, one that idles past
/// `PROBE_HEAD_TIMEOUT` — are discarded and the loop keeps accepting, so the
/// fixture tolerates any number of non-request connections ahead of the real
/// one. Returns `None` only if the listener itself errors out, which does not
/// happen in these tests short of process teardown.
fn accept_real_request(listener: &TcpListener) -> Option<TcpStream> {
    for stream in listener.incoming() {
        let mut stream = stream.ok()?;
        if consume_request_head(&mut stream) {
            return Some(stream);
        }
    }
    None
}

/// Bounds how long [`accept_real_request`] waits for a single connection to
/// send its request head before giving up on it and accepting the next one.
/// Far below every assertion window in this module, so even a few discarded
/// probes cannot push a test past its elapsed-time bounds.
const PROBE_HEAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads and discards the request head so a fixture responds only after the
/// client has finished sending its request. hyper rejects a response that
/// overlaps the request it is still writing (`UnexpectedMessage`), which makes
/// an eager fixture fail the exchange at random (ORB-11761).
///
/// Returns whether a complete head (`\r\n\r\n`) was received. A connection
/// that closes early or sends nothing within `PROBE_HEAD_TIMEOUT` is treated
/// as a probe, not an error, so the caller can move on to the next accepted
/// connection instead of panicking.
fn consume_request_head(stream: &mut TcpStream) -> bool {
    stream
        .set_read_timeout(Some(PROBE_HEAD_TIMEOUT))
        .expect("set probe read timeout");

    let mut head = Vec::new();
    let mut byte = [0; 1];

    while !head.ends_with(b"\r\n\r\n") {
        match std::io::Read::read(stream, &mut byte) {
            Ok(0) => return false,
            Ok(_) => head.push(byte[0]),
            Err(_) => return false,
        }
    }
    true
}

#[test]
#[cfg(unix)]
fn stale_installed_companion_is_replaced_and_reported_as_changed() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_installed_companion("0.3.1", "old");
    fixture.write_source_companion(env!("CARGO_PKG_VERSION"), "fresh");

    let result = run(SemanticInstallParams {
        model: None,
        force: false,
    })
    .expect("install should replace stale companion");

    assert!(result.companion_changed);
    assert!(!result.model_installed);
    assert!(
        std::fs::read_to_string(fixture.paths.companion_path())
            .expect("read replaced companion")
            .contains("replacement-marker: fresh")
    );
    let json = serde_json::to_value(&result).expect("serialize result");
    assert_eq!(json["companion_changed"], true);
    assert_eq!(json.get("companion_installed"), None);
}

#[test]
#[cfg(unix)]
fn force_replaces_current_companion() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_installed_companion(env!("CARGO_PKG_VERSION"), "old-current");
    fixture.write_source_companion(env!("CARGO_PKG_VERSION"), "forced-fresh");

    let result = run(SemanticInstallParams {
        model: None,
        force: true,
    })
    .expect("forced install should replace current companion");

    assert!(result.companion_changed);
    assert!(
        std::fs::read_to_string(fixture.paths.companion_path())
            .expect("read replaced companion")
            .contains("replacement-marker: forced-fresh")
    );
}

#[test]
#[cfg(unix)]
fn current_companion_is_left_in_place_without_force() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_installed_companion(env!("CARGO_PKG_VERSION"), "kept-current");
    fixture.write_source_companion(env!("CARGO_PKG_VERSION"), "unused-source");

    let result = run(SemanticInstallParams {
        model: None,
        force: false,
    })
    .expect("current install should be accepted");

    assert!(!result.companion_changed);
    assert!(
        std::fs::read_to_string(fixture.paths.companion_path())
            .expect("read kept companion")
            .contains("replacement-marker: kept-current")
    );
}

#[test]
#[cfg(unix)]
fn current_companion_freshness_hash_uses_open_descriptor() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_installed_companion(env!("CARGO_PKG_VERSION"), "descriptor-held");

    let companion = ManagedCompanion::open_current(&fixture.paths.companion_path())
        .expect("current install should verify");
    let original_checksum = companion
        .descriptor_checksum()
        .expect("hash opened descriptor");
    fixture.replace_installed_companion(env!("CARGO_PKG_VERSION"), "path-swapped");
    let path_checksum = sha256_hex(
        &std::fs::read(fixture.paths.companion_path()).expect("read swapped companion path"),
    );

    assert_ne!(original_checksum, path_checksum);
    assert_eq!(
        companion
            .descriptor_checksum()
            .expect("hash retained descriptor"),
        original_checksum
    );
}

/// End-to-end coverage for the fexecve launch path on platforms that
/// actually use it. Writes an ELF mock companion that records its own marker
/// on `--download-model`, opens it as a `ManagedCompanion`, swaps the binary
/// at the install path, then drives the model download. The marker file
/// proves the fd-held binary ran, not the path-swapped one.
///
/// The mock must be a real ELF image: a shebang script executed via
/// `execveat(fd, "", AT_EMPTY_PATH)` also needs the kernel to hand the
/// interpreter `/proc/self/fd/<n>`, which fails when `/proc` is `noexec`.
#[test]
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn fd_launch_executes_descriptor_not_path_after_swap() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    let model_dir = fixture.paths.models_dir.join("test-model");
    std::fs::create_dir_all(&model_dir).expect("create model dir");

    write_marker_companion(
        &fixture.paths.companion_path(),
        env!("CARGO_PKG_VERSION"),
        "original",
    );
    write_test_companion_integrity(&fixture.paths.companion_path(), env!("CARGO_PKG_VERSION"));

    let companion = ManagedCompanion::open_current(&fixture.paths.companion_path())
        .expect("current install should verify");

    // Swap the install path so a path-based exec would pick up "swapped"
    // while the fd-based launch must still run "original".
    //
    // Move the opened dentry aside before placing the replacement. A
    // rename-over unlinks the fd's dentry, and `execveat(AT_EMPTY_PATH)`
    // then fails with ENOENT when `/proc` is `noexec` — for ELF as well as
    // shebang, because the kernel reconstructs the unlinked exe path via
    // `/proc/self/fd`. Relocating the dentry keeps the descriptor's inode
    // named without consulting `/proc`.
    let held_path = fixture.paths.bin_dir.join("held-original-companion");
    std::fs::rename(fixture.paths.companion_path(), &held_path)
        .expect("move original companion dentry aside");
    let replacement_path = fixture.paths.bin_dir.join("replacement-companion");
    write_marker_companion(&replacement_path, env!("CARGO_PKG_VERSION"), "swapped");
    std::fs::rename(&replacement_path, fixture.paths.companion_path())
        .expect("rename replacement into place");
    write_test_companion_integrity(&fixture.paths.companion_path(), env!("CARGO_PKG_VERSION"));

    companion
        .download_model("test-model", &model_dir)
        .expect("fd-based model download succeeds");

    let marker = std::fs::read_to_string(model_dir.join("companion-identity.txt"))
        .expect("mock companion should have written its identity marker");
    assert_eq!(
        marker.trim(),
        "original",
        "fexecve must run the descriptor held by ManagedCompanion, not the path-swapped binary"
    );
}

#[test]
fn companion_launch_mode_matches_platform_support() {
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    assert_eq!(companion_launch_mode(), CompanionLaunchMode::FileDescriptor);

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )))]
    {
        assert_eq!(companion_launch_mode(), CompanionLaunchMode::Path);
        assert!(
            path_execution_fallback_rationale().contains("fexecve"),
            "fallback rationale should name the missing fd-exec primitive"
        );
    }
}

#[test]
fn semantic_install_rejects_intel_mac_platform_before_download() {
    let error = ensure_semantic_search_supported_for_platform("macos-x86_64", None)
        .expect_err("Intel macOS should not have a released semantic companion");

    assert!(
        error
            .to_string()
            .contains("semantic search unsupported on this platform (macos-x86_64)"),
        "{error}"
    );
}

#[test]
fn semantic_install_accepts_released_companion_platforms() {
    ensure_semantic_search_supported_for_platform("macos-aarch64", None)
        .expect("macOS arm64 companion is released");
    ensure_semantic_search_supported_for_platform("linux-x86_64", Some("2.38"))
        .expect("Linux x86_64 companion is released on glibc 2.38+");
    ensure_semantic_search_supported_for_platform("linux-aarch64", Some("2.39"))
        .expect("Linux aarch64 companion is released on glibc 2.38+");
}

#[test]
fn semantic_install_rejects_linux_glibc_below_floor() {
    let error = ensure_semantic_search_supported_for_platform("linux-x86_64", Some("2.35"))
        .expect_err("glibc 2.35 should be below the companion runtime floor");

    assert!(
        error.to_string().contains("requires glibc >= 2.38"),
        "{error}"
    );
    assert!(error.to_string().contains("detected glibc 2.35"), "{error}");
}

#[test]
#[cfg(unix)]
fn checksum_mismatch_rejects_replacement_before_install() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_installed_companion("0.3.1", "old");
    fixture.write_source_companion(env!("CARGO_PKG_VERSION"), "tampered");
    set_env("ORBIT_SEARCH_COMPANION_SHA256", &"0".repeat(64));

    let error = run(SemanticInstallParams {
        model: None,
        force: false,
    })
    .expect_err("checksum mismatch should reject install");

    assert!(
        error
            .to_string()
            .contains("companion checksum verification failed"),
        "{error}"
    );
    assert!(
        std::fs::read_to_string(fixture.paths.companion_path())
            .expect("read retained companion")
            .contains("replacement-marker: old")
    );
}

#[test]
#[cfg(unix)]
fn unsafe_url_override_is_rejected_without_explicit_opt_in() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_installed_companion("0.3.1", "old");
    remove_env("ORBIT_SEARCH_COMPANION");
    remove_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE");
    set_env(
        "ORBIT_SEARCH_COMPANION_URL",
        "http://example.invalid/companion",
    );

    let error = run(SemanticInstallParams {
        model: None,
        force: true,
    })
    .expect_err("http override should be rejected before download");

    assert!(error.to_string().contains("must use https"), "{error}");
    assert!(
        std::fs::read_to_string(fixture.paths.companion_path())
            .expect("read retained companion")
            .contains("replacement-marker: old")
    );
}

#[test]
fn default_download_source_requires_release_checksum_manifest() {
    let _guard = EnvGuard::new();
    remove_env("ORBIT_SEARCH_COMPANION");
    remove_env("ORBIT_SEARCH_COMPANION_URL");
    remove_env("ORBIT_SEARCH_COMPANION_SHA256");
    remove_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE");

    let source = default_release_download_source("orbit-search-companion-linux-x86_64".to_string())
        .expect("default source");

    match source.integrity {
        CompanionIntegrity::ReleaseSignedChecksum {
            checksums_url,
            signature_url,
            asset_name,
        } => {
            assert!(checksums_url.ends_with("/orbit-checksums.txt"));
            assert!(signature_url.ends_with("/orbit-checksums.txt.sig"));
            assert_eq!(asset_name, "orbit-search-companion-linux-x86_64");
        }
        other => panic!("default source should require signed release checksum: {other:?}"),
    }
}

#[test]
fn checksum_manifest_selects_platform_asset() {
    let expected = "64ec88ca00b268e5ba1a35678a1b5316d212f4f366b2477232534a8aeca37f3c";
    let manifest = format!(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other\n{expected}  ./{}\n",
        platform_companion_filename()
    );

    let checksum =
        checksum_from_manifest(&manifest, &platform_companion_filename()).expect("checksum entry");

    assert_eq!(checksum, expected);
}

#[test]
fn release_checksum_signature_verifies_trusted_manifest() {
    let manifest = "64ec88ca00b268e5ba1a35678a1b5316d212f4f366b2477232534a8aeca37f3c  orbit-search-companion-macos-aarch64";
    let signature = decode_hex(
        "7a30d97455a621a030176db6584a7a32875832e2d614b8faa63ac7ebd72f74100b7a9cdb3702c59d065c79fa653d0ca58dc90e19a0cd0a0699b28ff0699d89717a26d7605164d4653b6fa65d1b45db9c9a8532e47f85f3479ae6ca1175b90b3626410ebd6d511c480fdca8bce7e3eeef815d2276cfd5e8a3144a38856f5c4e21e4ee1e6962c9b3330a85e2f1a0c19c5ac921ad31394c65c2f4e477db79e901571b2a80721af082bfbb55d4c50e88ed07f69cf9434550c4625db23bc0df5b9aa4a56d683970e17b09db4a06f7544bc3b807a0c8ddda2a17e18a534fad22eca49da268f543dd510a7201a79f3259fbbe7c73d55212a93aa0a065bc07193ac81e02",
    );
    let public_key = r#"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAsXuoUfxsHxU/MZ9ZiBos
oxyAiXDWb1QunevWWTT0++xvSxMJwTLkY2CIIQ7Bot17bMo/deoE5yqVDN9KJIzZ
/JeX2Mf/E4/4iUroCmZw6p/Z0Z2eHpZlg9mZMXviCUq6IDz1APzt6m1lV8lhZvD9
g3J50rxPizO91a30yUpYAIag02f9VPafyuuHA8SoDsWVwIyZ+Tazn7qAVKOgRWnU
Ua2MN0GwlGsg4AySpNCH1cDKvoRDMbB0ngEm/B9r6Yiqi4stJOdOSBL0Z2VEwfpZ
JXAFgkhiMRYeVULLjCacqxXMFDtH1J7uoowGuJaKUVA7fzq+vk2eBO8i1Wm0fVyK
iQIDAQAB
-----END PUBLIC KEY-----"#;

    verify_checksum_signature_with_key(manifest.as_bytes(), &signature, public_key)
        .expect("valid signature should verify");
    let tampered = manifest.replace("macos-aarch64", "linux-x86_64");
    let error = verify_checksum_signature_with_key(tampered.as_bytes(), &signature, public_key)
        .expect_err("tampered manifest should fail signature verification");

    assert!(
        error
            .to_string()
            .contains("release checksum signature verification failed"),
        "{error}"
    );
}

#[test]
fn unsafe_override_gate_accepts_mixed_case_values() {
    let _guard = EnvGuard::new();
    set_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE", "Yes");
    assert!(unsafe_companion_overrides_enabled());

    set_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE", "True");
    assert!(unsafe_companion_overrides_enabled());
}

#[test]
#[cfg(unix)]
fn locate_companion_prefers_managed_install_over_override() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_installed_companion(env!("CARGO_PKG_VERSION"), "managed");
    fixture.write_source_companion(env!("CARGO_PKG_VERSION"), "override");

    let located = locate_companion().expect("managed companion should be located");

    assert_eq!(located, fixture.paths.companion_path());
}

#[test]
#[cfg(unix)]
fn runtime_override_requires_explicit_unsafe_opt_in() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    fixture.write_source_companion(env!("CARGO_PKG_VERSION"), "override");
    remove_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE");

    let error = locate_companion().expect_err("override without unsafe gate should fail");

    assert!(
        error
            .to_string()
            .contains("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE"),
        "{error}"
    );
}

#[test]
#[cfg(unix)]
fn runtime_override_rejects_non_executable_path() {
    let _guard = EnvGuard::new();
    let fixture = InstallFixture::new();
    std::fs::write(&fixture.source_path, "#!/bin/sh\nexit 0\n").expect("write source");

    let error = locate_companion().expect_err("non-executable override should fail");

    assert!(error.to_string().contains("not executable"), "{error}");
}

#[test]
#[cfg(unix)]
fn path_candidates_are_not_executed_in_normal_lookup() {
    let _guard = EnvGuard::new();
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&bin).expect("create bin");
    write_mock_companion(
        &bin.join("orbit-search-companion"),
        env!("CARGO_PKG_VERSION"),
        "path",
    );
    set_env("HOME", &home.to_string_lossy());
    set_env("USERPROFILE", &home.to_string_lossy());
    set_env("PATH", &bin.to_string_lossy());
    remove_env("ORBIT_SEARCH_COMPANION");
    remove_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE");

    let error = locate_companion().expect_err("PATH candidate should not be used");

    assert!(
        error.to_string().contains("Semantic search not enabled"),
        "{error}"
    );
}

struct InstallFixture {
    _temp: TempDir,
    paths: CompanionPaths,
    source_path: PathBuf,
}

impl InstallFixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        // `source_path` is used as an `ORBIT_SEARCH_COMPANION` override, whose
        // validator rejects a group/world-writable parent — pin the root so the
        // fixture doesn't depend on the ambient umask (ORB-10350).
        orbit_common::test_env::harden_dir(temp.path());
        let home = temp.path().join("home");
        let source_path = temp.path().join("source-companion");
        std::fs::create_dir_all(&home).expect("create home");
        set_env("HOME", &home.to_string_lossy());
        set_env("USERPROFILE", &home.to_string_lossy());
        set_env("ORBIT_SEARCH_COMPANION", &source_path.to_string_lossy());
        set_env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE", "1");
        remove_env("ORBIT_SEARCH_COMPANION_URL");
        remove_env("ORBIT_SEARCH_COMPANION_SHA256");

        let paths = CompanionPaths::default_under_home().expect("paths");
        std::fs::create_dir_all(&paths.bin_dir).expect("create bin");
        let model_dir = paths.model_dir(crate::default_model().alias);
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        std::fs::write(model_dir.join("orbit-model.json"), "{}").expect("write marker");

        Self {
            _temp: temp,
            paths,
            source_path,
        }
    }

    #[cfg(unix)]
    fn write_installed_companion(&self, version: &str, marker: &str) {
        write_mock_companion(&self.paths.companion_path(), version, marker);
        write_test_companion_integrity(&self.paths.companion_path(), version);
    }

    #[cfg(unix)]
    fn replace_installed_companion(&self, version: &str, marker: &str) {
        let replacement_path = self.paths.bin_dir.join("replacement-companion");
        write_mock_companion(&replacement_path, version, marker);
        std::fs::rename(&replacement_path, self.paths.companion_path())
            .expect("replace installed companion");
        write_test_companion_integrity(&self.paths.companion_path(), version);
    }

    #[cfg(unix)]
    fn write_source_companion(&self, version: &str, marker: &str) {
        write_mock_companion(&self.source_path, version, marker);
    }
}

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    vars: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new() -> Self {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let names = [
            "HOME",
            "USERPROFILE",
            "PATH",
            "ORBIT_SEARCH_COMPANION",
            "ORBIT_SEARCH_COMPANION_URL",
            "ORBIT_SEARCH_COMPANION_SHA256",
            "ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE",
            "NO_PROXY",
            "no_proxy",
        ];
        let vars = names
            .into_iter()
            .map(|name| (name, std::env::var(name).ok()))
            .collect::<Vec<_>>();
        Self { _lock: lock, vars }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.vars {
            match value {
                Some(value) => set_env(name, value),
                None => remove_env(name),
            }
        }
    }
}

#[cfg(unix)]
fn write_mock_companion(path: &std::path::Path, version: &str, marker: &str) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        r#"#!/bin/sh
# replacement-marker: {marker}
if [ "$1" = "--version-info" ]; then
  printf '%s\n' '{{"id":0,"result":{{"model_id":"bge-small-en-v1.5","dim":0,"max_input_tokens":0,"version":"{version}"}}}}'
  exit 0
fi
exit 0
"#
    );
    std::fs::write(path, script).expect("write companion");
    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod companion");
}

/// Token stamped into the compiled ELF so each fixture can carry a distinct
/// identity without a second rustc invocation. Must stay the same length as
/// the `ORB_MARKER` array in `marker_companion_source`.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
const MARKER_TOKEN: &[u8; 32] = b"ORB10835_FD_LAUNCH_MARKER_______";

/// A companion variant that records its identity to `<model_dir>/companion-identity.txt`
/// on `--download-model`, used by the fd-launch end-to-end test. The fixture is
/// a compiled ELF image so `fexecve` does not need a shebang interpreter lookup
/// through `/proc/self/fd`. Argument parsing is minimal because the real
/// companion accepts `--model X --model-path Y --download-model`.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn write_marker_companion(path: &std::path::Path, _version: &str, marker: &str) {
    use std::os::unix::fs::PermissionsExt;

    let bytes = stamp_marker_companion(marker);
    std::fs::write(path, bytes).expect("write marker companion");
    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod marker companion");
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn stamp_marker_companion(marker: &str) -> Vec<u8> {
    assert!(
        marker.len() <= MARKER_TOKEN.len(),
        "marker longer than ELF identity token"
    );
    let mut stamped = vec![b' '; MARKER_TOKEN.len()];
    stamped[..marker.len()].copy_from_slice(marker.as_bytes());
    let mut bytes = marker_companion_template().to_vec();
    let mut replacements = 0usize;
    let mut index = 0;
    while index + MARKER_TOKEN.len() <= bytes.len() {
        if bytes[index..index + MARKER_TOKEN.len()] == *MARKER_TOKEN {
            bytes[index..index + MARKER_TOKEN.len()].copy_from_slice(&stamped);
            replacements += 1;
            index += MARKER_TOKEN.len();
        } else {
            index += 1;
        }
    }
    assert!(
        replacements > 0,
        "compiled marker companion is missing the identity token"
    );
    bytes
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn marker_companion_template() -> &'static [u8] {
    static TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();
    TEMPLATE.get_or_init(compile_marker_companion_template)
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn compile_marker_companion_template() -> Vec<u8> {
    let scratch = tempfile::tempdir().expect("marker companion scratch dir");
    let src_path = scratch.path().join("marker_companion.rs");
    let bin_path = scratch.path().join("marker_companion");
    std::fs::write(&src_path, marker_companion_source()).expect("write marker companion source");

    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = std::process::Command::new(rustc)
        .arg("--edition")
        .arg("2021")
        .arg("-C")
        .arg("debuginfo=0")
        .arg("-C")
        .arg("strip=symbols")
        .arg("-C")
        .arg("opt-level=0")
        .arg("-o")
        .arg(&bin_path)
        .arg(&src_path)
        .output()
        .expect("spawn rustc for marker companion");
    assert!(
        output.status.success(),
        "rustc failed to build fd-launch marker companion: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bytes = std::fs::read(&bin_path).expect("read compiled marker companion");
    assert!(
        bytes.starts_with(b"\x7fELF"),
        "fd-launch fixture must be an ELF image, not a shebang script"
    );
    assert!(
        bytes
            .windows(MARKER_TOKEN.len())
            .any(|window| window == MARKER_TOKEN),
        "compiled marker companion is missing the identity token"
    );
    bytes
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn marker_companion_source() -> &'static str {
    r#"
#[used]
#[no_mangle]
static ORB_MARKER: [u8; 32] = *b"ORB10835_FD_LAUNCH_MARKER_______";

fn main() {
    let marker = core::str::from_utf8(&ORB_MARKER).unwrap_or("").trim();
    let mut model_path = None;
    let mut download = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model-path" => model_path = args.next(),
            "--download-model" => download = true,
            _ => {}
        }
    }
    if download {
        if let Some(dir) = model_path {
            let path = std::path::Path::new(&dir).join("companion-identity.txt");
            let _ = std::fs::write(path, format!("{marker}\n"));
        }
    }
}
"#
}

#[cfg(unix)]
fn write_test_companion_integrity(path: &std::path::Path, version: &str) {
    let bytes = std::fs::read(path).expect("read companion for checksum");
    let checksum = sha256_hex(&bytes);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("companion file name");
    let manifest_path = path.with_file_name(format!("{file_name}.sha256"));
    std::fs::write(
        manifest_path,
        serde_json::json!({
            "version": version,
            "sha256": checksum,
        })
        .to_string(),
    )
    .expect("write companion integrity");
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks(2)
        .map(|chunk| {
            let hex = std::str::from_utf8(chunk).expect("hex is utf8");
            u8::from_str_radix(hex, 16).expect("hex byte")
        })
        .collect()
}

fn set_env(name: &str, value: &str) {
    // SAFETY: tests that mutate process environment hold EnvGuard's global lock.
    unsafe { std::env::set_var(name, value) }
}

fn remove_env(name: &str) {
    // SAFETY: tests that mutate process environment hold EnvGuard's global lock.
    unsafe { std::env::remove_var(name) }
}
