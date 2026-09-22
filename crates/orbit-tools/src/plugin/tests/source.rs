use std::io::Write;

use orbit_types::plugin::MANIFEST_FILE_NAME;

use super::super::source::{
    ArchiveLimits, PluginSourceRequest, resolve_plugin_source, unpack_tar, unpack_zip,
};

const MANIFEST: &str = "\
schemaVersion: 2
kind: Plugin
metadata:
  name: demo
  version: 1.0.0
  description: Fixture.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: hello
      description: Hello.
      execution_kind: read_only
";

fn append_regular(builder: &mut tar::Builder<Vec<u8>>, name: &str, body: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_path(name).expect("set path");
    header.set_cksum();
    builder.append(&header, body).expect("append regular");
}

fn append_symlink(builder: &mut tar::Builder<Vec<u8>>, name: &str, target: &str) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o644);
    header.set_path(name).expect("set path");
    header.set_link_name(target).expect("set symlink target");
    header.set_cksum();
    builder
        .append(&header, &[] as &[u8])
        .expect("append symlink");
}

fn plugin_tar(with_symlink: bool) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    append_regular(&mut builder, MANIFEST_FILE_NAME, MANIFEST.as_bytes());
    append_regular(&mut builder, "bin/backend.sh", b"#!/bin/sh\n");
    if with_symlink {
        append_symlink(&mut builder, "env", "/proc/self/environ");
    }
    builder.into_inner().expect("finish tar")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).expect("gzip");
    encoder.finish().expect("finish gzip")
}

/// A source with no pinned digest, as `orbit plugin add <path>` supplies one.
fn unpinned(source: &str) -> PluginSourceRequest<'_> {
    PluginSourceRequest {
        source,
        expected_digest: None,
        digest_origin: PIN_LABEL,
    }
}

fn assert_refuses_symlink(error: &str, entry: &str) {
    assert!(
        error.contains(entry),
        "refusal must name the offending entry {entry:?}: {error}"
    );
    assert!(
        error.contains("symbolic link"),
        "refusal must say why: {error}"
    );
}

#[cfg(unix)]
fn enter_fake_git_child(test: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_PLUGIN_GIT_CHILD").ok().as_deref() == Some(&exact_test) {
        return true;
    }

    let temp = tempfile::tempdir().expect("fake git directory");
    let args_capture = temp.path().join("args");
    let env_capture = temp.path().join("env");
    let quote =
        |value: &std::path::Path| format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"));
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {args}\nenv | sort > {env}\ncheckout=''\nfor arg in \"$@\"; do checkout=$arg; done\nmkdir -p \"$checkout/.git\"\nprintf 'schemaVersion: 2\\nkind: Plugin\\n' > \"$checkout/plugin.yaml\"\nprintf 'url=https://user:secret@example.com/demo.git\\n' > \"$checkout/.git/config\"\n",
        args = quote(&args_capture),
        env = quote(&env_capture),
    );
    let git = temp.path().join("git");
    std::fs::write(&git, script).expect("write fake git");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
        .expect("make fake git executable");

    // The fetch shim serves whatever the child test wrote to the body path,
    // so an HTTPS source can be exercised end to end without a network or a
    // TLS server, and its argv can be asserted the same way Git's is.
    let curl_args_capture = temp.path().join("curl-args");
    let body = temp.path().join("body");
    let curl_script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {args}\nout=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = '--output' ]; then out=$arg; fi\n  prev=$arg\ndone\n[ -n \"$out\" ] || exit 2\n[ -f {body} ] || exit 22\ncat {body} > \"$out\"\n",
        args = quote(&curl_args_capture),
        body = quote(&body),
    );
    let curl = temp.path().join("curl");
    std::fs::write(&curl, curl_script).expect("write fake curl");
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755))
        .expect("make fake curl executable");

    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_GIT_CHILD", &exact_test)
        .env("ORBIT_TEST_PLUGIN_GIT_ARGS", &args_capture)
        .env("ORBIT_TEST_PLUGIN_GIT_ENV", &env_capture)
        .env("ORBIT_TEST_PLUGIN_CURL_ARGS", &curl_args_capture)
        .env("ORBIT_TEST_PLUGIN_CURL_BODY", &body)
        .env("PATH", std::env::join_paths(paths).expect("fake git PATH"))
        .output()
        .expect("run isolated fake-git test");
    assert!(
        output.status.success(),
        "fake-git child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[test]
fn a_tar_archive_with_a_symlink_entry_is_refused_before_the_tree_is_returned() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive = temp.path().join("plugin.tar");
    std::fs::write(&archive, plugin_tar(true)).expect("write tar");

    let error = resolve_plugin_source(&unpinned(archive.to_str().expect("utf8")))
        .expect_err("a symlink member must be refused")
        .to_string();
    assert_refuses_symlink(&error, "env");
}

#[test]
fn a_tar_gz_archive_with_a_symlink_entry_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive = temp.path().join("plugin.tar.gz");
    std::fs::write(&archive, gzip(&plugin_tar(true))).expect("write tar.gz");

    let error = resolve_plugin_source(&unpinned(archive.to_str().expect("utf8")))
        .expect_err("a symlink member must be refused")
        .to_string();
    assert_refuses_symlink(&error, "env");
}

#[test]
fn a_tar_archive_without_symlinks_resolves_to_the_manifest_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive = temp.path().join("plugin.tar");
    std::fs::write(&archive, plugin_tar(false)).expect("write tar");

    let resolved =
        resolve_plugin_source(&unpinned(archive.to_str().expect("utf8"))).expect("resolve");
    assert!(resolved.root.join(MANIFEST_FILE_NAME).is_file());
    assert!(!resolved.root.join("env").exists());
}

#[cfg(unix)]
#[test]
fn a_directory_source_with_a_symlink_to_a_file_outside_the_tree_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(root.join("bin")).expect("mkdir");
    std::fs::write(root.join(MANIFEST_FILE_NAME), MANIFEST).expect("manifest");
    std::fs::write(root.join("bin/backend.sh"), "#!/bin/sh\n").expect("backend");
    let secret = temp.path().join("secret");
    std::fs::write(&secret, "SECRET-CONTENT-OUTSIDE-PLUGIN-TREE").expect("secret");
    std::os::unix::fs::symlink(&secret, root.join("leaked")).expect("symlink");

    let error = resolve_plugin_source(&unpinned(root.to_str().expect("utf8")))
        .expect_err("an outside-tree symlink must be refused")
        .to_string();
    assert_refuses_symlink(&error, "leaked");
    assert!(
        !error.contains("SECRET-CONTENT-OUTSIDE-PLUGIN-TREE"),
        "the diagnostic names the link, not the target file's bytes: {error}"
    );
}

#[cfg(unix)]
#[test]
fn unsafe_git_sources_are_refused_before_git_is_spawned() {
    if !enter_fake_git_child("unsafe_git_sources_are_refused_before_git_is_spawned") {
        return;
    }
    let args_capture = std::path::PathBuf::from(
        std::env::var_os("ORBIT_TEST_PLUGIN_GIT_ARGS").expect("args capture path"),
    );
    for source in [
        "git+ext::sh -c 'exit 0' %S",
        "git+file:///tmp/plugin",
        "git+-uplugin",
        "git+https://example.com/demo.git#-upload-pack=payload",
    ] {
        let error = resolve_plugin_source(&unpinned(source))
            .expect_err("unsafe Git source must be refused")
            .to_string();
        assert!(
            error.contains(source),
            "refusal must name the source entry {source:?}: {error}"
        );
    }
    assert!(
        !args_capture.exists(),
        "Git must not be spawned for any refused source"
    );
}

#[cfg(unix)]
#[test]
fn tagged_https_git_source_uses_hardened_argv_and_environment() {
    if !enter_fake_git_child("tagged_https_git_source_uses_hardened_argv_and_environment") {
        return;
    }
    let resolved = resolve_plugin_source(&unpinned("git+https://example.com/demo.git#v1.2.3"))
        .expect("tagged HTTPS source resolves");
    assert!(resolved.root.join(MANIFEST_FILE_NAME).is_file());

    let args = std::fs::read_to_string(
        std::env::var_os("ORBIT_TEST_PLUGIN_GIT_ARGS").expect("args capture path"),
    )
    .expect("read Git argv")
    .lines()
    .map(str::to_string)
    .collect::<Vec<_>>();
    assert_eq!(
        &args[..args.len() - 1],
        [
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "protocol.ssh.allow=always",
            "clone",
            "--depth",
            "1",
            "--branch",
            "v1.2.3",
            "--",
            "https://example.com/demo.git",
        ],
        "Git protocol policy and option terminator must precede the URL"
    );
    assert_eq!(
        args.last().map(String::as_str),
        Some(resolved.root.to_str().expect("UTF-8 checkout path")),
        "the final argument is the checkout path"
    );

    let environment = std::fs::read_to_string(
        std::env::var_os("ORBIT_TEST_PLUGIN_GIT_ENV").expect("environment capture path"),
    )
    .expect("read Git environment");
    assert!(
        environment
            .lines()
            .any(|line| line == "GIT_PROTOCOL_FROM_USER=0"),
        "Git must not honor user-selected protocols: {environment}"
    );
    assert!(
        environment
            .lines()
            .any(|line| line == "GIT_TERMINAL_PROMPT=0"),
        "Git must not prompt for credentials: {environment}"
    );
}

#[cfg(unix)]
#[test]
fn an_untagged_git_source_clones_the_default_branch_and_scrubs_git_metadata() {
    if !enter_fake_git_child(
        "an_untagged_git_source_clones_the_default_branch_and_scrubs_git_metadata",
    ) {
        return;
    }
    let resolved = resolve_plugin_source(&unpinned("git+https://example.com/demo.git"))
        .expect("untagged HTTPS source resolves");
    assert!(resolved.root.join(MANIFEST_FILE_NAME).is_file());
    assert!(
        !resolved.root.join(".git").exists(),
        "clone metadata can contain source credentials and must not enter the plugin tree"
    );

    let args = std::fs::read_to_string(
        std::env::var_os("ORBIT_TEST_PLUGIN_GIT_ARGS").expect("args capture path"),
    )
    .expect("read Git argv");
    assert!(
        !args.lines().any(|arg| arg == "--branch"),
        "an untagged source must use the repository's default branch: {args}"
    );
}

// ---------------------------------------------------------------------------
// Fetched archives: the digest pin, the URL rules, and the unpack bounds.
// ---------------------------------------------------------------------------

const ARCHIVE_URL: &str = "https://example.com/demo-1.0.0.tar.gz";
const PIN_LABEL: &str = "the `.orbit/plugins.yaml` pin for 'demo'";

fn sha256_pin(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        orbit_common::security::release::sha256_hex(bytes)
    )
}

/// A digest no archive in these tests hashes to.
const WRONG_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn pinned<'a>(source: &'a str, digest: Option<&'a str>) -> PluginSourceRequest<'a> {
    PluginSourceRequest {
        source,
        expected_digest: digest,
        digest_origin: PIN_LABEL,
    }
}

#[cfg(unix)]
fn serve(body: &[u8]) {
    let path = std::env::var_os("ORBIT_TEST_PLUGIN_CURL_BODY").expect("fetch body path");
    std::fs::write(path, body).expect("write fetch body");
}

#[cfg(unix)]
fn fetch_argv() -> Vec<String> {
    std::fs::read_to_string(
        std::env::var_os("ORBIT_TEST_PLUGIN_CURL_ARGS").expect("fetch args path"),
    )
    .expect("read fetch argv")
    .lines()
    .map(str::to_string)
    .collect()
}

#[cfg(unix)]
fn fetch_was_attempted() -> bool {
    std::path::Path::new(&std::env::var_os("ORBIT_TEST_PLUGIN_CURL_ARGS").expect("fetch args path"))
        .exists()
}

#[cfg(unix)]
#[test]
fn a_pinned_https_tar_gz_archive_installs_and_records_its_digest() {
    if !enter_fake_git_child("a_pinned_https_tar_gz_archive_installs_and_records_its_digest") {
        return;
    }
    let body = gzip(&plugin_tar(false));
    serve(&body);
    let digest = sha256_pin(&body);

    let resolved = resolve_plugin_source(&pinned(ARCHIVE_URL, Some(&digest)))
        .expect("pinned archive resolves");
    assert!(resolved.root.join(MANIFEST_FILE_NAME).is_file());
    assert_eq!(
        resolved.archive_digest.as_deref(),
        Some(digest.trim_start_matches("sha256:")),
        "the verified archive digest is carried back for the install record"
    );
}

/// ORB-12812's URL rules have to hold for every hop, not just the first, so
/// they are the fetcher's own options rather than a check on the URL string.
#[cfg(unix)]
#[test]
fn a_fetched_archive_is_https_only_with_no_cross_scheme_redirect() {
    if !enter_fake_git_child("a_fetched_archive_is_https_only_with_no_cross_scheme_redirect") {
        return;
    }
    let body = gzip(&plugin_tar(false));
    serve(&body);
    resolve_plugin_source(&pinned(ARCHIVE_URL, Some(&sha256_pin(&body)))).expect("resolve");

    let argv = fetch_argv();
    for expected in [
        ["--proto", "=https"],
        ["--proto-redir", "=https"],
        ["--max-redirs", "5"],
    ] {
        let position = argv
            .iter()
            .position(|arg| arg == expected[0])
            .unwrap_or_else(|| panic!("{} must be passed: {argv:?}", expected[0]));
        assert_eq!(
            argv.get(position + 1).map(String::as_str),
            Some(expected[1]),
            "{} must be {}: {argv:?}",
            expected[0],
            expected[1]
        );
    }
    assert!(
        argv.contains(&"--disable".to_string()),
        "a host .curlrc must not be able to re-enable another scheme: {argv:?}"
    );
    let terminator = argv
        .iter()
        .position(|arg| arg == "--")
        .expect("the option terminator must precede the URL");
    assert_eq!(
        argv.get(terminator + 1).map(String::as_str),
        Some(ARCHIVE_URL),
        "the URL is the only argument after `--`: {argv:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_archive_whose_digest_does_not_match_the_pin_is_refused() {
    if !enter_fake_git_child("an_archive_whose_digest_does_not_match_the_pin_is_refused") {
        return;
    }
    let body = gzip(&plugin_tar(false));
    serve(&body);

    let error = resolve_plugin_source(&pinned(ARCHIVE_URL, Some(WRONG_DIGEST)))
        .expect_err("an archive that is not the pinned one must be refused")
        .to_string();
    assert!(
        error.contains(PIN_LABEL),
        "the refusal must name the pin entry to correct: {error}"
    );
    assert!(
        error.contains(&sha256_pin(&body)) && error.contains(WRONG_DIGEST),
        "the refusal must report both the observed and the pinned digest: {error}"
    );
}

/// No trust on first use, and no download either: an unpinned archive source
/// is refused before the fetcher is ever spawned.
#[cfg(unix)]
#[test]
fn an_unpinned_archive_source_is_refused_before_anything_is_fetched() {
    if !enter_fake_git_child("an_unpinned_archive_source_is_refused_before_anything_is_fetched") {
        return;
    }
    let error = resolve_plugin_source(&pinned(ARCHIVE_URL, None))
        .expect_err("an archive source without a digest must be refused")
        .to_string();
    assert!(
        error.contains(PIN_LABEL),
        "the refusal must name where the digest belongs: {error}"
    );
    assert!(
        !fetch_was_attempted(),
        "nothing may be downloaded for an unpinned archive source"
    );
}

#[cfg(unix)]
#[test]
fn a_malformed_pin_digest_is_refused_before_anything_is_fetched() {
    if !enter_fake_git_child("a_malformed_pin_digest_is_refused_before_anything_is_fetched") {
        return;
    }
    let error = resolve_plugin_source(&pinned(ARCHIVE_URL, Some("sha256:nope")))
        .expect_err("a malformed digest must be refused")
        .to_string();
    assert!(
        error.contains(PIN_LABEL),
        "the refusal must name the pin entry: {error}"
    );
    assert!(
        !fetch_was_attempted(),
        "nothing may be downloaded for a malformed digest"
    );
}

#[test]
fn a_dash_prefixed_source_is_refused() {
    let error = resolve_plugin_source(&unpinned("-https://example.com/p.tar.gz"))
        .expect_err("a `-`-prefixed source must be refused")
        .to_string();
    assert!(
        error.contains("begins with `-`"),
        "the refusal must say why: {error}"
    );
}

#[test]
fn a_plain_http_archive_url_is_not_a_source() {
    let error = resolve_plugin_source(&pinned("http://example.com/p.tar.gz", Some(WRONG_DIGEST)))
        .expect_err("only https is fetched")
        .to_string();
    assert!(
        error.contains("is not a directory, an archive"),
        "a non-HTTPS URL is not a source Orbit fetches: {error}"
    );
}

// --- Extraction bounds and escapes -----------------------------------------

fn unpack_tar_bytes(bytes: &[u8], limits: ArchiveLimits) -> Result<tempfile::TempDir, String> {
    let dest = tempfile::tempdir().expect("tempdir");
    unpack_tar(bytes, dest.path(), "fixture.tar", limits)
        .map(|()| dest)
        .map_err(|error| error.to_string())
}

/// Append a member under a name `tar::Header::set_path` refuses to write.
///
/// `set_path` rejects `..` and absolute paths, which is exactly what a
/// hostile archive carries, so these fixtures write the name field directly.
fn append_raw_name(builder: &mut tar::Builder<Vec<u8>>, name: &str, body: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    let raw = header.as_mut_bytes();
    let bytes = name.as_bytes();
    assert!(
        bytes.len() < 100,
        "fixture name must fit the tar name field"
    );
    raw[..bytes.len()].copy_from_slice(bytes);
    header.set_cksum();
    builder
        .append(&header, body)
        .expect("append raw-named entry");
}

fn tar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, body) in entries {
        append_raw_name(&mut builder, name, body);
    }
    builder.into_inner().expect("finish tar")
}

#[test]
fn a_tar_member_that_traverses_out_of_the_root_is_refused() {
    // `tar`'s own `unpack_in` silently *skips* such a member and reports
    // success, which would install a quietly incomplete tree.
    let archive = tar_with(&[
        (MANIFEST_FILE_NAME, MANIFEST.as_bytes()),
        ("../escaped.txt", b"owned"),
    ]);
    let error = unpack_tar_bytes(&archive, ArchiveLimits::DEFAULT)
        .expect_err("a traversing member must be refused");
    assert!(
        error.contains("escaped.txt") && error.contains(".."),
        "the refusal must name the entry and why: {error}"
    );
}

#[test]
fn an_absolute_tar_member_is_refused() {
    let archive = tar_with(&[
        (MANIFEST_FILE_NAME, MANIFEST.as_bytes()),
        ("/etc/orbit-owned", b"owned"),
    ]);
    let error = unpack_tar_bytes(&archive, ArchiveLimits::DEFAULT)
        .expect_err("an absolute member must be refused");
    assert!(
        error.contains("absolute path"),
        "the refusal must say why: {error}"
    );
}

#[test]
fn a_tar_past_the_entry_bound_is_refused() {
    let archive = tar_with(&[("a", b"1"), ("b", b"2"), ("c", b"3")]);
    let error = unpack_tar_bytes(
        &archive,
        ArchiveLimits {
            entries: 2,
            ..ArchiveLimits::DEFAULT
        },
    )
    .expect_err("an archive past the entry bound must be refused");
    assert!(
        error.contains("more than 2 entries"),
        "the refusal must name the bound: {error}"
    );
}

#[test]
fn a_tar_that_unpacks_past_the_size_bound_is_refused() {
    let archive = tar_with(&[("big", &[b'x'; 4096])]);
    let error = unpack_tar_bytes(
        &archive,
        ArchiveLimits {
            unpacked_bytes: 512,
            ..ArchiveLimits::DEFAULT
        },
    )
    .expect_err("an archive past the size bound must be refused");
    assert!(
        error.contains("more than 512 bytes"),
        "the refusal must name the bound: {error}"
    );
}

/// A compressed archive is bounded by what it *inflates* to, not by how many
/// bytes arrived: a few kilobytes of zeros expand without limit otherwise.
#[test]
fn a_compression_bomb_is_bounded_by_its_unpacked_size() {
    let archive = gzip(&tar_with(&[("bomb", &[0u8; 1 << 20])]));
    assert!(
        archive.len() < 8192,
        "the fixture must be small compressed, or it is not testing the bound"
    );
    let dest = tempfile::tempdir().expect("tempdir");
    let error = unpack_tar(
        flate2::read::GzDecoder::new(archive.as_slice()),
        dest.path(),
        "bomb.tar.gz",
        ArchiveLimits {
            unpacked_bytes: 4096,
            ..ArchiveLimits::DEFAULT
        },
    )
    .expect_err("an inflating archive must be refused")
    .to_string();
    assert!(
        error.contains("4096 bytes"),
        "the refusal must name the unpacked bound: {error}"
    );
}

fn zip_with(entries: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body, mode) in entries {
        let mut options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        if let Some(mode) = mode {
            options = options.unix_permissions(*mode);
        }
        writer
            .start_file(name.to_string(), options)
            .expect("start zip entry");
        writer.write_all(body).expect("write zip entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

fn unpack_zip_bytes(bytes: &[u8], limits: ArchiveLimits) -> Result<tempfile::TempDir, String> {
    let staging = tempfile::tempdir().expect("tempdir");
    let path = staging.path().join("fixture.zip");
    std::fs::write(&path, bytes).expect("write zip");
    let dest = tempfile::tempdir().expect("tempdir");
    let file = std::fs::File::open(&path).expect("open zip");
    unpack_zip(file, dest.path(), "fixture.zip", limits)
        .map(|()| dest)
        .map_err(|error| error.to_string())
}

#[test]
fn a_zip_resolves_and_keeps_the_backend_executable() {
    let archive = zip_with(&[
        (MANIFEST_FILE_NAME, MANIFEST.as_bytes(), None),
        ("bin/backend.sh", b"#!/bin/sh\n", Some(0o755)),
    ]);
    let dest = unpack_zip_bytes(&archive, ArchiveLimits::DEFAULT).expect("unpack zip");
    assert!(dest.path().join(MANIFEST_FILE_NAME).is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.path().join("bin/backend.sh"))
            .expect("backend metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "an executable backend stays executable"
        );
    }
}

#[test]
fn a_zip_member_that_traverses_out_of_the_root_is_refused() {
    let archive = zip_with(&[("../escaped.txt", b"owned", None)]);
    let error = unpack_zip_bytes(&archive, ArchiveLimits::DEFAULT)
        .expect_err("a traversing member must be refused");
    assert!(
        error.contains("escaped.txt") && error.contains(".."),
        "the refusal must name the entry and why: {error}"
    );
}

#[test]
fn an_absolute_zip_member_is_refused() {
    let archive = zip_with(&[("/etc/orbit-owned", b"owned", None)]);
    let error = unpack_zip_bytes(&archive, ArchiveLimits::DEFAULT)
        .expect_err("an absolute member must be refused");
    assert!(
        error.contains("absolute path"),
        "the refusal must say why: {error}"
    );
}

#[test]
fn a_zip_symlink_member_is_refused() {
    // A symbolic link, which `copy_tree` would follow out of the install root.
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .add_symlink::<_, _, ()>("env", "/proc/self/environ", Default::default())
        .expect("add symlink");
    let archive = writer.finish().expect("finish zip").into_inner();
    let error = unpack_zip_bytes(&archive, ArchiveLimits::DEFAULT)
        .expect_err("a symlink member must be refused");
    assert_refuses_symlink(&error, "env");
}

#[test]
fn a_zip_past_the_entry_bound_is_refused() {
    let archive = zip_with(&[("a", b"1", None), ("b", b"2", None), ("c", b"3", None)]);
    let error = unpack_zip_bytes(
        &archive,
        ArchiveLimits {
            entries: 2,
            ..ArchiveLimits::DEFAULT
        },
    )
    .expect_err("an archive past the entry bound must be refused");
    assert!(
        error.contains("more than 2 entries"),
        "the refusal must name the bound: {error}"
    );
}

#[test]
fn a_zip_that_unpacks_past_the_size_bound_is_refused() {
    let archive = zip_with(&[("big", &[b'x'; 4096], None)]);
    let error = unpack_zip_bytes(
        &archive,
        ArchiveLimits {
            unpacked_bytes: 512,
            ..ArchiveLimits::DEFAULT
        },
    )
    .expect_err("an archive past the size bound must be refused");
    assert!(
        error.contains("more than 512 bytes"),
        "the refusal must name the bound: {error}"
    );
}
