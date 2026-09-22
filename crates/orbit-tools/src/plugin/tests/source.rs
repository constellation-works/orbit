use std::io::Write;

use orbit_types::plugin::MANIFEST_FILE_NAME;

use super::super::source::resolve_plugin_source;

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
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {args}\nenv | sort > {env}\ncheckout=''\nfor arg in \"$@\"; do checkout=$arg; done\nmkdir -p \"$checkout\"\nprintf 'schemaVersion: 2\\nkind: Plugin\\n' > \"$checkout/plugin.yaml\"\n",
        args = quote(&args_capture),
        env = quote(&env_capture),
    );
    let git = temp.path().join("git");
    std::fs::write(&git, script).expect("write fake git");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
        .expect("make fake git executable");
    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_GIT_CHILD", &exact_test)
        .env("ORBIT_TEST_PLUGIN_GIT_ARGS", &args_capture)
        .env("ORBIT_TEST_PLUGIN_GIT_ENV", &env_capture)
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

    let error = resolve_plugin_source(archive.to_str().expect("utf8"))
        .expect_err("a symlink member must be refused")
        .to_string();
    assert_refuses_symlink(&error, "env");
}

#[test]
fn a_tar_gz_archive_with_a_symlink_entry_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive = temp.path().join("plugin.tar.gz");
    std::fs::write(&archive, gzip(&plugin_tar(true))).expect("write tar.gz");

    let error = resolve_plugin_source(archive.to_str().expect("utf8"))
        .expect_err("a symlink member must be refused")
        .to_string();
    assert_refuses_symlink(&error, "env");
}

#[test]
fn a_tar_archive_without_symlinks_resolves_to_the_manifest_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive = temp.path().join("plugin.tar");
    std::fs::write(&archive, plugin_tar(false)).expect("write tar");

    let resolved = resolve_plugin_source(archive.to_str().expect("utf8")).expect("resolve");
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

    let error = resolve_plugin_source(root.to_str().expect("utf8"))
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
        let error = resolve_plugin_source(source)
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
    let resolved = resolve_plugin_source("git+https://example.com/demo.git#v1.2.3")
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
