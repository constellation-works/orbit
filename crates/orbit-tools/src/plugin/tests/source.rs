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
