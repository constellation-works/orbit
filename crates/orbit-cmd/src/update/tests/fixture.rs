//! A complete fake installation and release mirror on disk.
//!
//! Nothing here touches a real Orbit installation: the "executable" is a shell
//! script in a temp directory and the "release" is a signed archive in another
//! one. That is what makes the replacement, rollback, and convergence paths
//! testable at all — the flow runs the installed binary, so the installed
//! binary has to be something a test can write and observe.

use std::fmt::{self, Debug};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use chrono::NaiveDate;
use orbit_common::OrbitError;
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME, TrustedReleaseKey,
};
use rsa::RsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer};
use tempfile::TempDir;

use crate::update::channel::InstallChannel;
use crate::update::source::{DirectoryReleaseSource, MIRROR_LATEST_FILE, ReleaseSource};
use crate::update::{UpdateEnvironment, UpdateRequest};

/// A throwaway keypair generated for these tests. It is not the release
/// signing key and no published artifact will ever verify against it.
const TEST_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDMI5n7ZrLkVUBN
p2yBs07QGUg9WzN5k6kH4FGEoaN4nxmQEBpRfBIdt2OmWyOGKp7wsnwsSSwmoCgz
q1dc4WV5U3Vr93tKY/KhZB0wVvu886LrOy0pXK6FYxR80b572Cfz/Sy62lFFtsV/
VysYXxtHt1MRrubWs8bBLuMEwL5q9vsQ4+ZXJm4hGofDMMhNN4KlP5HVha7QPIUl
1nK8+8H6UZgPDwrnzI53miY3c/72EUpZzreRJLB02B/AUIwtqjoZh0OpQ6WnCzcS
u3pGYJ7beg7QMn+66ySV/N+M6y2gX/8J/1A59af918BLUM3sG2YKwBQyfu4fEBsl
fc7WAOvpAgMBAAECggEACDY6ayw4xV9POeXNQ0kU7OFyJX22DA2jlBHdqvSG9e2N
2Adod59XvzhHBZnO9lu1OcDZppJpo0LNZd6zofnd3MvoV6fZ9CZ3IS+SI7ABanAb
5/3hFYkz6rZkoZdyY9r7KHZ7xnx3ySfOuWOmnnwHmz1FqzAaUHK6QRUuGV0uNnFB
8nDt5Eu6tU5eoqrU6I6WtDWIMrUcINz5JEoFuuEaTRZRp2x+8oow2WvBWPWVazPH
l4xbalkRpcoQdoiNtKr8/aYAHuKBJ45NobWOvex8wIdh+t635ztczv+jwtFxSlb7
gCbQtU0ZnPJEHJ9k/3zJYowdcUnBs2JAcJbhYo0IXQKBgQDovKxKEDf1srsI5kFL
ZQcfi+3tPzxRXM/i5yKpgCMMrrNVn2Yfu9uYUxoglhHGV5sN2mmNcEM67Lik/XVf
mgRVhvqT5CCrJaYusc+THuhARPPcaZfJr+zKW3Rk+Olqj9lMNtmCarMzoRaC+RXo
5P3vlVzfkhpE8nd3x/cPI11HSwKBgQDgiyh2UdgSPcfjffyS0Jvr133RfagnsJ9r
Qh4Mimq8Zd+SP6z2AvMf72wbDbG+1pn1Wlaqycus/gDmQzoN71gtd/GZxVW6YNH3
QS6qxewulZrUq5w42P3EEeZesnIRrPet07tkFXPTCOaN7cZXXRscu96Hg8KovWI+
jcniB+vVGwKBgHCICaYmAWjDarv62UdjKfaO6hP0p22PutSzfYcHdesD7aJQ2Egv
xRX52IA5D48ffNFN8gt5ZIhxPTZJdx8qkT3pbe9kNoeKRLf/MaapIxMwQ9knFUVn
0s5lOfo4gGQN+btoKfNtNAiasw/Q8E8TqdTWG3neYuVDd5BrF4IyTz/RAoGAWb/l
fV17QtdE1S4fTUNqfxrT5G8YTjzvi3yS7CpLPWBuu1MOPAqzyNj22d1gZUn7obDp
ITylV1DzZRYL11QKZ6ogfHj+qg9W/UAlegbAP2J2z3iEach5rewFq2Yh5+S93tHZ
fciBUiGlnacjdvn1A0goSvwkSzPfV+dugRTvc28CgYANo3fEgAhbNRPxw7mwzOiM
wr4k9J6JlveGjqPgqsBkv6rPPIBH2zAMAWm+siroAhB2yaTmdhOWFI3AWJa8X5t0
WRP9OgmB7BQzr0pxxoNzNTq0+5Im6p4plpf7J/wQ7JhBTn1rUNsJh8kSZDtyrk4D
kfcgZmDk20JOE7uLH9Mx5g==
-----END PRIVATE KEY-----";

const TEST_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAzCOZ+2ay5FVATadsgbNO
0BlIPVszeZOpB+BRhKGjeJ8ZkBAaUXwSHbdjplsjhiqe8LJ8LEksJqAoM6tXXOFl
eVN1a/d7SmPyoWQdMFb7vPOi6zstKVyuhWMUfNG+e9gn8/0sutpRRbbFf1crGF8b
R7dTEa7m1rPGwS7jBMC+avb7EOPmVyZuIRqHwzDITTeCpT+R1YWu0DyFJdZyvPvB
+lGYDw8K58yOd5omN3P+9hFKWc63kSSwdNgfwFCMLao6GYdDqUOlpws3Ert6RmCe
23oO0DJ/uusklfzfjOstoF//Cf9QOfWn/dfAS1DN7BtmCsAUMn7uHxAbJX3O1gDr
6QIDAQAB
-----END PUBLIC KEY-----";

/// Test trust set carrying the throwaway key above.
pub fn test_trusted_keys() -> &'static [TrustedReleaseKey] {
    &[TrustedReleaseKey {
        id: "orbit-test-key-1",
        not_after: "2099-12-31",
        revoked_at: None,
        public_key_pem: TEST_PUBLIC_KEY_PEM,
    }]
}

/// The release target the fixture publishes for.
pub const TEST_TARGET: &str = "x86_64-unknown-linux-gnu";

/// How the fake replacement binary behaves when it is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeBinary {
    /// Reports its version and succeeds at every subcommand.
    Healthy,
    /// Reports a version other than the one the release claims.
    VersionMismatch,
    /// Fails `migrate --confirm`, as a binary that cannot migrate would.
    MigrationFails,
    /// Fails `workspace sync` after migrating cleanly.
    SyncFails,
}

/// A fake installation plus the release mirror it updates from.
pub struct Fixture {
    _root: TempDir,
    /// The installed executable `orbit update` will replace.
    pub executable: PathBuf,
    /// The directory the fake binaries append their argv to.
    pub invocation_log: PathBuf,
    /// The workspace the convergence steps run in.
    pub workspace: PathBuf,
    mirror: PathBuf,
    installed_version: String,
}

impl Fixture {
    /// Build a fixture whose installed binary reports `installed_version`.
    pub fn new(installed_version: &str) -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let bin = root.path().join("bin");
        let mirror = root.path().join("mirror");
        let workspace = root.path().join("workspace");
        for directory in [&bin, &mirror, &workspace] {
            std::fs::create_dir_all(directory).expect("fixture directory");
        }
        let invocation_log = root.path().join("invocations.log");
        let executable = bin.join("orbit");
        write_script(
            &executable,
            &script(installed_version, &invocation_log, FakeBinary::Healthy),
        );
        Self {
            _root: root,
            executable,
            invocation_log,
            workspace,
            mirror,
            installed_version: installed_version.to_string(),
        }
    }

    /// Publish `version` to the mirror, and make it the mirror's latest.
    pub fn publish(&self, version: &str, behavior: FakeBinary) {
        let reported = match behavior {
            FakeBinary::VersionMismatch => "0.0.1",
            _ => version,
        };
        let archive = tar_gz(&script(reported, &self.invocation_log, behavior));
        self.publish_archive(version, &archive, true);
    }

    /// Publish raw archive bytes, signing a manifest that matches them.
    pub fn publish_archive(&self, version: &str, archive: &[u8], sign_correctly: bool) {
        let directory = self.mirror.join(format!("v{version}"));
        std::fs::create_dir_all(&directory).expect("release directory");
        let asset = crate::update::channel::release_archive_name(TEST_TARGET);
        std::fs::write(directory.join(&asset), archive).expect("write archive");

        let digest = orbit_common::security::release::sha256_hex(archive);
        let manifest = format!("{digest}  {asset}\n");
        std::fs::write(directory.join(RELEASE_CHECKSUMS_FILENAME), &manifest)
            .expect("write manifest");
        let signed = if sign_correctly {
            manifest.clone()
        } else {
            format!("{manifest}# not what was published\n")
        };
        std::fs::write(
            directory.join(RELEASE_CHECKSUMS_SIGNATURE_FILENAME),
            sign(signed.as_bytes()),
        )
        .expect("write signature");
        std::fs::write(
            self.mirror.join(MIRROR_LATEST_FILE),
            format!("v{version}\n"),
        )
        .expect("write latest");
    }

    /// Corrupt a published archive after its manifest was signed.
    pub fn tamper_with_archive(&self, version: &str) {
        let asset = crate::update::channel::release_archive_name(TEST_TARGET);
        let path = self.mirror.join(format!("v{version}")).join(asset);
        let mut archive = std::fs::read(&path).expect("read archive");
        archive.extend_from_slice(b"tampered");
        std::fs::write(&path, archive).expect("write tampered archive");
    }

    /// Build the update environment for this fixture.
    pub fn environment(&self) -> UpdateEnvironment {
        self.environment_with_workspace(Some(self.workspace.clone()))
    }

    /// Build an environment whose caller is not inside an Orbit workspace.
    pub fn environment_without_workspace(&self) -> UpdateEnvironment {
        self.environment_with_workspace(None)
    }

    fn environment_with_workspace(&self, workspace_cwd: Option<PathBuf>) -> UpdateEnvironment {
        UpdateEnvironment {
            install_channel: InstallChannel::Managed {
                install_dir: self.executable.parent().expect("bin dir").to_path_buf(),
            },
            executable: self.executable.clone(),
            current_version: self.installed_version.clone(),
            target_triple: TEST_TARGET.to_string(),
            source: Box::new(DirectoryReleaseSource::new(self.mirror.clone())),
            trusted_keys: test_trusted_keys(),
            today: NaiveDate::from_ymd_opt(2026, 9, 5).expect("valid date"),
            workspace_cwd,
        }
    }

    /// What the installed executable reports for `--version`.
    pub fn installed_reports(&self) -> String {
        format!(
            "orbit {}",
            crate::update::converge::probe_version(&self.executable).expect("run installed binary")
        )
    }

    /// Every argv the fake binaries have been invoked with, in order.
    pub fn invocations(&self) -> Vec<String> {
        std::fs::read_to_string(&self.invocation_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Path the outgoing executable is preserved at.
    pub fn backup_path(&self) -> PathBuf {
        let mut name = self.executable.as_os_str().to_os_string();
        name.push(".previous");
        PathBuf::from(name)
    }

    /// Entries the flow left behind in the install directory.
    pub fn install_dir_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.executable.parent().expect("bin dir"))
            .expect("read install dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

/// A default request: latest version, apply, no downgrade.
pub fn request() -> UpdateRequest {
    UpdateRequest::default()
}

/// Parks `latest_version` on `paused` until `resume`, then returns a frozen
/// version. Lets another update finish between snapshot and lock without
/// timing sleeps.
pub struct PausingLatestSource {
    inner: Box<dyn ReleaseSource>,
    latest: String,
    paused: Arc<Barrier>,
    resume: Arc<Barrier>,
}

impl PausingLatestSource {
    pub fn wrap(
        inner: Box<dyn ReleaseSource>,
        latest: impl Into<String>,
        paused: Arc<Barrier>,
        resume: Arc<Barrier>,
    ) -> Self {
        Self {
            inner,
            latest: latest.into(),
            paused,
            resume,
        }
    }
}

impl Debug for PausingLatestSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PausingLatestSource")
            .field("inner", &self.inner)
            .field("latest", &self.latest)
            .finish_non_exhaustive()
    }
}

impl ReleaseSource for PausingLatestSource {
    fn describe(&self) -> String {
        self.inner.describe()
    }

    fn latest_version(&self) -> Result<String, OrbitError> {
        self.paused.wait();
        self.resume.wait();
        Ok(self.latest.clone())
    }

    fn fetch(&self, version: &str, asset: &str) -> Result<Vec<u8>, OrbitError> {
        self.inner.fetch(version, asset)
    }
}

fn sign(message: &[u8]) -> Vec<u8> {
    let key = RsaPrivateKey::from_pkcs8_pem(TEST_PRIVATE_KEY_PEM).expect("test private key");
    SigningKey::<Sha256>::new(key).sign(message).to_vec()
}

/// A `sh` stand-in for the `orbit` binary: it answers `--version` and records
/// every other invocation so a test can assert the convergence order.
fn script(version: &str, log: &Path, behavior: FakeBinary) -> Vec<u8> {
    let failure = match behavior {
        FakeBinary::MigrationFails => {
            "if [ \"$1\" = migrate ]; then echo 'migration failed: pending layout v9' >&2; exit 1; fi\n"
        }
        FakeBinary::SyncFails => {
            "if [ \"$1\" = workspace ]; then echo 'managed asset sync failed' >&2; exit 1; fi\n"
        }
        FakeBinary::Healthy | FakeBinary::VersionMismatch => "",
    };
    format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then echo 'orbit {version}'; exit 0; fi\n\
         echo \"{version}: $*\" >> '{log}'\n\
         {failure}exit 0\n",
        log = log.display()
    )
    .into_bytes()
}

fn write_script(path: &Path, body: &[u8]) {
    std::fs::write(path, body).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
    }
}

/// Pack `body` as the single `orbit` member of a gzipped tar, the shape every
/// published release archive has.
pub fn tar_gz(body: &[u8]) -> Vec<u8> {
    tar_gz_named(&[("orbit", body)])
}

/// Pack arbitrary members, for the archive-shape rejection tests.
///
/// Member names are written into the header directly rather than through
/// `set_path`, which refuses `..` — the traversal names a hostile archive
/// would carry are exactly what the extractor has to be shown rejecting.
pub fn tar_gz_named(members: &[(&str, &[u8])]) -> Vec<u8> {
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
    let tar = builder.into_inner().expect("finish tar");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).expect("gzip");
    encoder.finish().expect("finish gzip")
}
