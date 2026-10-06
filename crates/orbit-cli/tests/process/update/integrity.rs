//! `orbit update` integrity at the CLI boundary, against a fake release mirror.
//!
//! The installed executable is a copy of this test's `orbit` binary in a
//! managed directory. Release bytes are a shell stand-in signed by a throwaway
//! key the child is told to trust. Refusals leave that copy and a seeded
//! generation record untouched.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::output_of;

const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_SLICE: Duration = Duration::from_millis(20);
const STALE_SLICE_DEADLINE: Duration = Duration::from_secs(15);

struct Install {
    _root: TempDir,
    home: PathBuf,
    cwd: PathBuf,
    mirror: PathBuf,
    executable: PathBuf,
    private_key: PathBuf,
    trust_file: PathBuf,
    generation: PathBuf,
    before: Vec<u8>,
    generation_before: Vec<u8>,
}

impl Install {
    fn new(revoked_at: Option<&str>) -> Self {
        let root = tempfile::tempdir().expect("install root");
        let home = root.path().join("home");
        let cwd = root.path().join("cwd");
        let mirror = root.path().join("mirror");
        let bin = root.path().join("bin");
        for directory in [&home, &cwd, &mirror, &bin] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        crate::git_repo::seal_lookup_boundary(&cwd);
        let executable = super::install_test_binary(&bin);
        let before = fs::read(&executable).expect("installed bytes");
        let (private_key, public_key) = generate_keypair(root.path());
        let trust_file = root.path().join("trusted-release-keys.txt");
        write_trust(&trust_file, &public_key, revoked_at);
        let generation = home.join(".orbit").join(".generation.lock");
        fs::create_dir_all(generation.parent().expect("generation parent"))
            .expect("generation dir");
        let generation_before = format!("1:{}\n", "ab".repeat(32));
        fs::write(&generation, &generation_before).expect("seed generation");
        Self {
            _root: root,
            home,
            cwd,
            mirror,
            executable,
            private_key,
            trust_file,
            generation,
            before,
            generation_before: generation_before.into_bytes(),
        }
    }

    fn command(&self) -> assert_cmd::Command {
        let mut command = assert_cmd::Command::new(&self.executable);
        apply_env(&mut command, self);
        command.timeout(UPDATE_TIMEOUT);
        command
    }

    fn std_command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        apply_std_env(&mut command, self);
        command
    }

    /// Publish `version`. `archive` is omitted when the case must fail before download.
    fn publish(&self, version: &str, archive: Option<&[u8]>, sign_manifest: bool) {
        let directory = self.mirror.join(format!("v{version}"));
        fs::create_dir_all(&directory).expect("release directory");
        let asset = release_asset();
        let bytes = archive.unwrap_or_default();
        if let Some(archive) = archive {
            fs::write(directory.join(asset), archive).expect("write archive");
        }
        let digest = sha256_hex(bytes);
        let manifest = format!("{digest}  {asset}\n");
        let signed = if sign_manifest {
            manifest.clone()
        } else {
            format!("{manifest}# not what was published\n")
        };
        fs::write(directory.join("orbit-checksums.txt"), &manifest).expect("write manifest");
        fs::write(
            directory.join("orbit-checksums.txt.sig"),
            sign(&self.private_key, signed.as_bytes()),
        )
        .expect("write signature");
        fs::write(
            self.mirror.join("latest-version.txt"),
            format!("{version}\n"),
        )
        .expect("write latest");
    }

    fn tamper_archive(&self, version: &str) {
        let path = self
            .mirror
            .join(format!("v{version}"))
            .join(release_asset());
        let mut archive = fs::read(&path).expect("read archive");
        archive.extend_from_slice(b"tampered");
        fs::write(path, archive).expect("write tampered archive");
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command();
        command.args(args);
        output_of(&mut command).expect("run orbit update")
    }

    fn assert_untouched(&self, label: &str) {
        assert_eq!(
            fs::read(&self.executable).expect("installed bytes"),
            self.before,
            "{label} replaced the installed binary"
        );
        assert_eq!(
            fs::read(&self.generation).expect("generation record"),
            self.generation_before,
            "{label} rewrote the generation record"
        );
        assert!(
            !staging_remains(self.executable.parent().expect("install dir")),
            "{label} left a staging file behind"
        );
    }

    fn assert_no_backup(&self, label: &str) {
        let backup = backup_path(&self.executable);
        assert!(
            !backup.exists(),
            "{label} wrote a backup at {}",
            backup.display()
        );
    }
}

fn apply_env(command: &mut assert_cmd::Command, install: &Install) {
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&install.cwd)
        .env("HOME", &install.home)
        .env("USERPROFILE", &install.home)
        .env("ORBIT_UPDATE_RELEASE_DIR", &install.mirror)
        .env(
            "ORBIT_INSTALL_DIR",
            install.executable.parent().expect("install dir"),
        )
        .env("ORBIT_RELEASE_TRUSTED_KEYS_FILE", &install.trust_file)
        .env(
            "ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE",
            "1",
        )
        .env_remove("ORBIT_HOME")
        .env_remove("ORBIT_INSTALL_REPO")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE");
}

fn apply_std_env(command: &mut Command, install: &Install) {
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&install.cwd)
        .env("HOME", &install.home)
        .env("USERPROFILE", &install.home)
        .env("ORBIT_UPDATE_RELEASE_DIR", &install.mirror)
        .env(
            "ORBIT_INSTALL_DIR",
            install.executable.parent().expect("install dir"),
        )
        .env("ORBIT_RELEASE_TRUSTED_KEYS_FILE", &install.trust_file)
        .env(
            "ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE",
            "1",
        )
        .env_remove("ORBIT_HOME")
        .env_remove("ORBIT_INSTALL_REPO")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE");
}

fn assert_refused(output: &Output, label: &str, needle: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{label} should refuse\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle),
        "{label} stderr did not contain {needle:?}: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{label} wrote a payload on refusal: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn assert_not_downloaded(output: &Output, label: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(release_asset()) && !stderr.contains("failed to read"),
        "{label} reached the archive download: {stderr}"
    );
}

#[test]
fn a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned() {
    let install = Install::new(None);
    let archive = tar_gz(&candidate_script("99.0.0"));
    install.publish("99.0.0", Some(&archive), true);
    install.tamper_archive("99.0.0");

    let output = install.run(&["update"]);

    assert_refused(&output, "checksum mismatch", "checksum verification failed");
    install.assert_untouched("checksum mismatch");
    install.assert_no_backup("checksum mismatch");
}

#[test]
fn a_manifest_signed_over_different_bytes_is_rejected_before_download() {
    let install = Install::new(None);
    install.publish("99.0.0", None, false);

    let output = install.run(&["update"]);

    assert_refused(
        &output,
        "bad signature",
        "no trusted release signing key matched",
    );
    assert_not_downloaded(&output, "bad signature");
    install.assert_untouched("bad signature");
    install.assert_no_backup("bad signature");
}

#[test]
fn a_revoked_key_fails_closed_before_expiry() {
    let install = Install::new(Some("2020-01-01"));
    install.publish("99.0.0", None, true);

    let output = install.run(&["update"]);

    assert_refused(&output, "revoked key", "revoked 2020-01-01");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("expired"),
        "revoked key was reported as expired: {stderr}"
    );
    assert_not_downloaded(&output, "revoked key");
    install.assert_untouched("revoked key");
    install.assert_no_backup("revoked key");
}

#[test]
fn an_archive_with_extra_members_including_traversal_is_rejected() {
    let install = Install::new(None);
    let archive = tar_gz_named(&[
        ("orbit", candidate_script("99.0.0").as_slice()),
        ("../evil", b"payload".as_slice()),
    ]);
    install.publish("99.0.0", Some(&archive), true);

    let output = install.run(&["update"]);

    assert_refused(&output, "hostile archive", "must contain only");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("../evil"),
        "traversal member was normalized away: {stderr}"
    );
    install.assert_untouched("hostile archive");
    install.assert_no_backup("hostile archive");
}

#[test]
fn a_mislabeled_release_is_rejected_without_attempting_the_swap() {
    let install = Install::new(None);
    // Hold the original inode open so even a swap followed by rollback cannot
    // pass by reusing its inode number.
    let original = File::open(&install.executable).expect("installed executable");
    let archive = tar_gz(&candidate_script("0.0.1"));
    install.publish("99.0.0", Some(&archive), true);

    let output = install.run(&["update"]);

    assert_refused(&output, "version mismatch", "reports itself as 0.0.1");
    install.assert_untouched("version mismatch");
    install.assert_no_backup("version mismatch");
    assert_eq!(
        original.metadata().expect("original inode").ino(),
        fs::metadata(&install.executable)
            .expect("installed inode")
            .ino(),
        "a mislabeled release must never be swapped into the installed path, even temporarily"
    );
}

#[test]
fn a_downgrade_is_refused_unless_explicitly_requested() {
    let install = Install::new(None);
    let script = candidate_script("0.0.1");
    let archive = tar_gz(&script);
    install.publish("0.0.1", Some(&archive), true);

    let refused = install.run(&["update"]);
    assert_refused(&refused, "downgrade", "--allow-downgrade");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("refusing to replace"), "{stderr}");
    assert_not_downloaded(&refused, "downgrade");
    install.assert_untouched("downgrade");
    install.assert_no_backup("downgrade");

    let allowed = install.run(&["update", "--allow-downgrade", "--json"]);
    assert!(
        allowed.status.success(),
        "explicit downgrade failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );
    let report: Value = serde_json::from_slice(&allowed.stdout).expect("downgrade report");
    assert_eq!(report["outcome"], "updated");
    assert_eq!(report["replaced"], true);
    assert_eq!(report["target_version"], "0.0.1");
    assert_eq!(
        fs::read(&install.executable).expect("downgraded bytes"),
        script,
        "explicit downgrade did not install the requested release"
    );
}

#[test]
fn a_second_concurrent_update_is_refused() {
    let install = Install::new(None);
    let script = candidate_script("99.0.0");
    let archive = tar_gz(&script);
    install.publish("99.0.0", Some(&archive), true);
    let lock_path = install
        .executable
        .parent()
        .expect("install dir")
        .join(".orbit-update.lock");
    let held = File::create(&lock_path).expect("create update lock");
    let rc = unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "lock: {}", std::io::Error::last_os_error());

    let refused = install.run(&["update"]);
    assert_refused(
        &refused,
        "concurrent update",
        "another orbit update is already running",
    );
    install.assert_untouched("concurrent update");
    install.assert_no_backup("concurrent update");
    drop(held);

    let retried = install.run(&["update", "--json"]);
    assert!(
        retried.status.success(),
        "update after the lock was released failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&retried.stdout),
        String::from_utf8_lossy(&retried.stderr)
    );
    let report: Value = serde_json::from_slice(&retried.stdout).expect("retry report");
    assert_eq!(report["outcome"], "updated");
    assert_eq!(
        fs::read(&install.executable).expect("updated bytes"),
        script
    );
}

/// The running binary discovered `1.0.0` while blocked on the mirror. The
/// process has already recorded the install path (`current_exe` runs before
/// that read). Before the read completes, that directory entry is replaced
/// with a binary that reports `99.0.0` and the generation record already names
/// a newer digest. The locked re-probe must follow the live path and refuse
/// to put `1.0.0` in its place.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_stale_writer_and_an_older_binary_never_displace_a_newer_install() {
    let install = Install::new(None);
    let older = candidate_script("1.0.0");
    let newer = candidate_script("99.0.0");
    install.publish("1.0.0", Some(&tar_gz(&older)), true);
    fs::remove_file(install.mirror.join("latest-version.txt")).expect("remove regular latest");
    let latest = install.mirror.join("latest-version.txt");
    mkfifo(&latest);

    let stdout_path = install._root.path().join("stale.stdout");
    let stderr_path = install._root.path().join("stale.stderr");
    let child = install
        .std_command()
        .arg("update")
        .stdout(File::create(&stdout_path).expect("stdout"))
        .stderr(File::create(&stderr_path).expect("stderr"))
        .spawn()
        .expect("spawn stale update");
    let mut child = ReapedChild { child: Some(child) };

    // Opening the write end is the rendezvous with `latest_version`: the
    // child has finished `current_exe` and is blocked in the mirror read.
    // Only then is it safe to replace the directory entry that path names.
    // Rename, rather than truncating, so the still-mapped image is untouched.
    let mut writer = open_fifo_writer(
        &latest,
        child.child.as_mut().expect("child"),
        Instant::now() + STALE_SLICE_DEADLINE,
    );
    let preserved = install.executable.with_file_name("orbit.running");
    fs::rename(&install.executable, &preserved).expect("unlink running image");
    fs::write(&install.executable, &newer).expect("plant newer install");
    fs::set_permissions(&install.executable, fs::Permissions::from_mode(0o755))
        .expect("chmod newer install");
    writer
        .write_all(b"1.0.0\n")
        .expect("publish the stale latest version");
    drop(writer);

    let status = wait_exit(
        child.child.as_mut().expect("child"),
        Instant::now() + STALE_SLICE_DEADLINE,
    );
    child.child = None;
    let stderr = fs::read_to_string(&stderr_path).expect("stale stderr");
    let stdout = fs::read_to_string(&stdout_path).expect("stale stdout");
    assert_eq!(
        status.code(),
        Some(1),
        "stale writer should refuse\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("--allow-downgrade")
            && stderr.contains("99.0.0")
            && stderr.contains("1.0.0"),
        "stale writer stderr: {stderr}"
    );
    assert!(
        !stderr.contains("failed to read"),
        "stale writer installed instead of refusing: {stderr}"
    );
    assert!(stdout.is_empty(), "stale writer wrote a payload: {stdout}");
    assert_eq!(
        fs::read(&install.executable).expect("live install"),
        newer,
        "stale writer displaced the newer install"
    );
    assert_eq!(
        fs::read(&install.generation).expect("generation record"),
        install.generation_before,
        "stale writer rewrote the newer generation"
    );
    assert!(
        !staging_remains(install.executable.parent().expect("install dir")),
        "stale writer left a staging file"
    );
    let _ = child;
}

pub(super) struct ReapedChild {
    pub(super) child: Option<Child>,
}

impl Drop for ReapedChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Rendezvous with the mirror reader without a blocking opener thread or
/// Linux's non-portable read/write FIFO open. ENXIO means no reader yet.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn open_fifo_writer(path: &Path, child: &mut Child, deadline: Instant) -> File {
    loop {
        assert_waiting(child, deadline, "open the mirror");
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => return file,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                std::thread::sleep(WAIT_SLICE);
            }
            Err(error) => panic!("open mirror fifo: {error}"),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_waiting(child: &mut Child, deadline: Instant, what: &str) {
    if Instant::now() >= deadline {
        let _ = child.kill();
        panic!("timed out waiting for the update to {what}");
    }
    if let Some(status) = child.try_wait().expect("poll update") {
        panic!("update exited before it could {what}: {status}");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn wait_exit(child: &mut Child, deadline: Instant) -> std::process::ExitStatus {
    loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("reap timed-out update");
            panic!("update did not finish before the deadline: {status}");
        }
        if let Some(status) = child.try_wait().expect("poll update") {
            return status;
        }
        std::thread::sleep(WAIT_SLICE);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn mkfifo(path: &Path) {
    let name = CString::new(path.as_os_str().as_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(name.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());
}

fn generate_keypair(dir: &Path) -> (PathBuf, PathBuf) {
    let private = dir.join("release-test.pem");
    let public = dir.join("release-test.pub");
    let generated = Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&private)
        .output()
        .expect("openssl genpkey");
    assert!(
        generated.status.success(),
        "openssl genpkey: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let exported = Command::new("openssl")
        .arg("pkey")
        .arg("-in")
        .arg(&private)
        .args(["-pubout", "-out"])
        .arg(&public)
        .output()
        .expect("openssl pkey");
    assert!(
        exported.status.success(),
        "openssl pkey: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    (private, public)
}

fn write_trust(path: &Path, public_key: &Path, revoked_at: Option<&str>) {
    let revoked = revoked_at.unwrap_or("");
    fs::write(
        path,
        format!(
            "orbit-test-key-1|2099-12-31|{revoked}|{}\n",
            public_key.display()
        ),
    )
    .expect("write trust file");
}

fn sign(private_key: &Path, message: &[u8]) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("signature dir");
    let message_path = dir.path().join("message");
    let signature = dir.path().join("signature");
    fs::write(&message_path, message).expect("write message");
    let signed = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(private_key)
        .arg("-out")
        .arg(&signature)
        .arg(&message_path)
        .output()
        .expect("openssl dgst");
    assert!(
        signed.status.success(),
        "openssl dgst: {}",
        String::from_utf8_lossy(&signed.stderr)
    );
    fs::read(signature).expect("read signature")
}

fn candidate_script(version: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then echo 'orbit {version}'; exit 0; fi\n\
         if [ \"$1\" = update ] && [ \"$2\" = --contract ]; then \
         echo '{{\"schema_version\":1,\"contract\":\"executable-generation-v1\"}}'; exit 0; fi\n\
         if [ \"$1\" = migrate ] && [ \"$2\" = --dry-run ]; then \
         echo '{{\"up_to_date\":true,\"schema\":{{\"current\":21,\"supported\":21}},\"layout\":{{\"current\":3,\"supported\":3}},\"forward_compatible\":{{\"read_only\":false}}}}'; \
         exit 0; fi\n\
         exit 0\n"
    )
    .into_bytes()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Asset name for this build. Matches `channel::release_target_triple`.
fn release_asset() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "orbit-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "orbit-aarch64-unknown-linux-gnu.tar.gz",
        ("macos", "aarch64") => "orbit-aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "orbit-x86_64-apple-darwin.tar.gz",
        (os, arch) => panic!("orbit publishes no release archive for {os}/{arch}"),
    }
}

fn tar_gz(body: &[u8]) -> Vec<u8> {
    tar_gz_named(&[("orbit", body)])
}

/// Member names are written into the ustar header directly. `set_path` refuses
/// `..`, and a hostile archive's traversal name is what the extractor must reject.
fn tar_gz_named(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, body) in members {
        let mut header = tar::Header::new_ustar();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        let raw = header.as_old_mut();
        let bytes = name.as_bytes();
        assert!(bytes.len() < raw.name.len(), "fixture member name too long");
        raw.name[..bytes.len()].copy_from_slice(bytes);
        header.set_cksum();
        builder.append(&header, *body).expect("append member");
    }
    let tar_bytes = builder.into_inner().expect("finish tar");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("gzip");
    encoder.finish().expect("finish gzip")
}

pub(super) fn staging_remains(directory: &Path) -> bool {
    fs::read_dir(directory)
        .expect("read install dir")
        .filter_map(Result::ok)
        .any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".orbit-update-staged") || name.starts_with(".orbit-update-restore")
        })
}

pub(super) fn backup_path(executable: &Path) -> PathBuf {
    let mut name = executable.as_os_str().to_os_string();
    name.push(".previous");
    PathBuf::from(name)
}
