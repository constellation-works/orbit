//! `orbit update --local-candidate`: install an operator-built executable
//! pinned to an exact source commit, through the same admission, staging,
//! rollback and convergence a published release uses.
//!
//! Every shift build of one release shares a semantic version, so the version
//! comparison that drives a release update cannot tell two builds apart. A
//! local candidate is identified by the SHA-256 of its exact bytes instead: a
//! different digest replaces the installation even at an equal version, and
//! an equal digest is the resume — it skips the swap and re-runs convergence.
//!
//! The trust model is deliberately narrow and reported literally as
//! `operator_attested`. The updater verifies what it can observe itself: the
//! digest of the bytes it accepted, and the object format and CPU architecture
//! their executable header names. The source commit is the operator's
//! assertion, carried by a v1 manifest and confirmed only against the commit
//! the operator expects on the command line. Nothing here proves how a binary
//! was built, and a local candidate is never treated as a signed release.
//!
//! Ordering is the release pipeline's: admission on every authority and the
//! install-directory lock are taken first and held while the candidate is
//! read, verified, staged, probed and swapped in, then the candidate is pinned
//! through convergence. The candidate's bytes are read once; everything after
//! that runs and installs the staged copy, so replacing the candidate's
//! pathname mid-update changes neither what runs nor what is installed.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::generation::executable_generation;
use orbit_common::security::release::normalize_sha256;
use serde::{Deserialize, Serialize};

use super::admission::{acquire_admissions, pin_candidate};
use super::channel::InstallChannel;
use super::converge::{probe_version, require_admission_contract};
use super::environment::UpdateEnvironment;
use super::flow::{assert_downgrade_is_compatible, backup_path, locked_installed_version};
use super::lock::UpdateLock;
use super::report::{UpdateOutcome, UpdateReport, finish};
use super::source::{MAX_METADATA_BYTES, read_bounded};
use super::stage::{restore_backup, stage_executable};
use super::version::ReleaseVersion;

/// `kind` every local-candidate manifest carries.
pub const MANIFEST_KIND: &str = "orbit-local-candidate";

/// The only manifest schema this updater reads.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The trust classification of a local candidate, in manifests and reports.
pub const OPERATOR_ATTESTED: &str = "operator_attested";

/// The only executable name a local candidate may replace.
const INSTALLED_NAME: &str = "orbit";

/// A v1 local-candidate manifest: which source commit the operator says the
/// executable was built from, the platform it targets, and its exact digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateManifest {
    /// Always [`MANIFEST_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Always [`MANIFEST_KIND`].
    pub kind: String,
    /// Always [`OPERATOR_ATTESTED`].
    pub trust: String,
    /// Full Git commit the executable was built from, as the operator attests.
    pub source_commit: String,
    /// Release target triple the executable header names.
    pub target: String,
    /// SHA-256 of the exact executable bytes.
    pub executable_sha256: String,
}

impl CandidateManifest {
    /// Parse and validate a manifest, refusing any other schema or trust class.
    pub fn parse(bytes: &[u8]) -> Result<Self, OrbitError> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "the local-candidate manifest is not a valid {MANIFEST_KIND} v1 document: {error}"
            ))
        })?;
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION || manifest.kind != MANIFEST_KIND {
            return Err(OrbitError::InvalidInput(format!(
                "unsupported local-candidate manifest {} v{}; this updater reads only \
                 {MANIFEST_KIND} v{MANIFEST_SCHEMA_VERSION}",
                manifest.kind, manifest.schema_version
            )));
        }
        if manifest.trust != OPERATOR_ATTESTED {
            return Err(OrbitError::InvalidInput(format!(
                "the local-candidate manifest claims trust '{}'; a local candidate is only ever \
                 {OPERATOR_ATTESTED}, never a signed release",
                manifest.trust
            )));
        }
        Ok(Self {
            source_commit: normalize_source_commit(&manifest.source_commit)?,
            executable_sha256: normalize_sha256(
                &manifest.executable_sha256,
                "the manifest's executable_sha256",
            )?,
            ..manifest
        })
    }
}

/// Lowercase a full Git commit id, refusing abbreviations and anything else.
///
/// SHA-1 repositories name commits with 40 hex digits and SHA-256 ones with
/// 64. A prefix is ambiguous by construction, so it never identifies a build.
pub fn normalize_source_commit(value: &str) -> Result<String, OrbitError> {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.len(), 40 | 64) || !normalized.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(OrbitError::InvalidInput(format!(
            "source commit '{value}' is not a full Git commit id; pass all 40 (SHA-1) or 64 \
             (SHA-256) hex digits, e.g. `git rev-parse HEAD`"
        )));
    }
    Ok(normalized)
}

/// The release target triple an executable header names.
///
/// This establishes the object format and CPU architecture only. It does not
/// establish the C library, so it is reported as header evidence rather than
/// as proof the binary runs on this host.
pub fn executable_target(bytes: &[u8]) -> Result<&'static str, OrbitError> {
    let field = |offset: usize, width: usize| bytes.get(offset..offset + width);
    match bytes {
        [0x7f, b'E', b'L', b'F', ..] => {
            if field(4, 2) != Some(&[2, 1]) {
                return Err(OrbitError::InvalidInput(
                    "the local candidate is not a 64-bit little-endian ELF executable".into(),
                ));
            }
            match field(18, 2).map(|machine| u16::from_le_bytes([machine[0], machine[1]])) {
                Some(62) => Ok("x86_64-unknown-linux-gnu"),
                Some(183) => Ok("aarch64-unknown-linux-gnu"),
                machine => Err(OrbitError::InvalidInput(format!(
                    "the local candidate is an ELF executable for machine {machine:?}, which \
                     orbit does not build for"
                ))),
            }
        }
        [0xcf, 0xfa, 0xed, 0xfe, ..] => {
            match field(4, 4).map(|cpu| u32::from_le_bytes([cpu[0], cpu[1], cpu[2], cpu[3]])) {
                Some(0x0100_0007) => Ok("x86_64-apple-darwin"),
                Some(0x0100_000c) => Ok("aarch64-apple-darwin"),
                cpu => Err(OrbitError::InvalidInput(format!(
                    "the local candidate is a Mach-O executable for CPU type {cpu:?}, which \
                     orbit does not build for"
                ))),
            }
        }
        [0xca, 0xfe, 0xba, 0xbe, ..] | [0xbe, 0xba, 0xfe, 0xca, ..] => {
            Err(OrbitError::InvalidInput(
                "the local candidate is a universal Mach-O binary; build and install a \
                 single-architecture executable for this host"
                    .into(),
            ))
        }
        _ => Err(OrbitError::InvalidInput(
            "the local candidate is not a native executable (no 64-bit ELF or Mach-O header)"
                .into(),
        )),
    }
}

/// What `--write-candidate-manifest` records.
#[derive(Debug, Clone)]
pub struct CandidateManifestRequest {
    /// The built executable to describe.
    pub candidate: PathBuf,
    /// The full commit the operator built it from.
    pub source_commit: String,
    /// Where to write the manifest. Never overwritten.
    pub output: PathBuf,
}

/// Describe a built executable in a new v1 manifest.
///
/// The digest and target come from the bytes; the commit is the operator's.
/// The manifest is created fresh, so it can neither clobber an existing file
/// (the candidate itself, say) nor silently replace an earlier attestation.
pub fn write_candidate_manifest(
    request: &CandidateManifestRequest,
) -> Result<CandidateManifest, OrbitError> {
    let source_commit = normalize_source_commit(&request.source_commit)?;
    drop(open_candidate(&request.candidate)?);
    let candidate = AcceptedCandidate::describe(&request.candidate)?;
    let manifest = CandidateManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        kind: MANIFEST_KIND.to_string(),
        trust: OPERATOR_ATTESTED.to_string(),
        source_commit,
        target: candidate.target.to_string(),
        executable_sha256: candidate.sha256,
    };
    let mut document = serde_json::to_string_pretty(&manifest)
        .map_err(|error| OrbitError::Execution(format!("serialize manifest: {error}")))?;
    document.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request.output)
        .map_err(|error| {
            OrbitError::Io(format!(
                "cannot create the manifest '{}': {error}; manifests are never overwritten, so \
                 choose a new path",
                request.output.display()
            ))
        })?;
    std::io::Write::write_all(&mut file, document.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            OrbitError::Io(format!(
                "failed to write the manifest '{}': {error}",
                request.output.display()
            ))
        })?;
    Ok(manifest)
}

/// What an operator asked a local-candidate update to install.
#[derive(Debug, Clone)]
pub struct LocalCandidateRequest {
    /// The built executable whose bytes are installed.
    pub candidate: PathBuf,
    /// Its v1 manifest.
    pub manifest: PathBuf,
    /// The full commit the operator expects the manifest to attest.
    pub source_commit: String,
    /// Permit a candidate reporting an older version than the installed one.
    pub allow_downgrade: bool,
}

/// One attested value and how the updater came to accept it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// The accepted value.
    pub value: String,
    /// `operator_attested` for an assertion the updater cannot check, or the
    /// check it ran.
    pub evidence: &'static str,
}

/// The local-candidate section of an update report.
#[derive(Debug, Clone, Serialize)]
pub struct LocalCandidateEvidence {
    /// Always [`OPERATOR_ATTESTED`].
    pub trust: &'static str,
    /// Always false: a local candidate is never a signed release.
    pub signed_release: bool,
    /// The candidate path the accepted bytes were read from.
    pub candidate: PathBuf,
    /// The manifest that attested them.
    pub manifest: PathBuf,
    /// The full source commit, as expected on the command line and attested
    /// by the manifest. Never verified against the bytes.
    pub source_commit: Evidence,
    /// SHA-256 of the accepted bytes, computed by the updater.
    pub executable_sha256: Evidence,
    /// Target triple, read from the accepted bytes' executable header.
    pub target: Evidence,
    /// Digest of the installed executable before this run.
    pub installed_sha256_before: String,
    /// Digest of the installed executable after this run, once verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_sha256_after: Option<String>,
    /// The exact command that retries this candidate.
    pub retry_command: String,
}

/// Largest local candidate accepted. Debug builds run to hundreds of
/// megabytes, well past the release archive limit.
const MAX_CANDIDATE_BYTES: u64 = 1024 * 1024 * 1024;

/// What the updater observed about one executable's bytes.
struct AcceptedCandidate {
    sha256: String,
    target: &'static str,
}

impl AcceptedCandidate {
    /// SHA-256 and header target of the executable at `path`.
    fn describe(path: &Path) -> Result<Self, OrbitError> {
        let mut header = Vec::with_capacity(64);
        File::open(path)
            .and_then(|file| file.take(64).read_to_end(&mut header))
            .map_err(|error| {
                OrbitError::Io(format!("cannot read '{}': {error}", path.display()))
            })?;
        let target = executable_target(&header)?;
        Ok(Self {
            sha256: executable_generation(path)?,
            target,
        })
    }

    /// Hold the bytes to the manifest and the manifest to the operator.
    fn verify(
        &self,
        manifest: &CandidateManifest,
        expected_commit: &str,
        host_target: &str,
    ) -> Result<(), OrbitError> {
        if manifest.source_commit != expected_commit {
            return Err(OrbitError::InvalidInput(format!(
                "the manifest attests source commit {}, but this update expects {expected_commit}; \
                 nothing was replaced. Build and describe the expected commit, or pass the commit \
                 the manifest names",
                manifest.source_commit
            )));
        }
        if manifest.executable_sha256 != self.sha256 {
            return Err(OrbitError::InvalidInput(format!(
                "the local candidate's SHA-256 is {}, but the manifest attests {}; the candidate \
                 changed after its manifest was written, so nothing was replaced. Rebuild and \
                 rewrite the manifest",
                self.sha256, manifest.executable_sha256
            )));
        }
        if manifest.target != self.target {
            return Err(OrbitError::InvalidInput(format!(
                "the manifest attests target {}, but the candidate's executable header names {}; \
                 nothing was replaced",
                manifest.target, self.target
            )));
        }
        if self.target != host_target {
            return Err(OrbitError::InvalidInput(format!(
                "the local candidate is built for {}, but this installation needs {host_target}; \
                 nothing was replaced. Build the same commit on this host",
                self.target
            )));
        }
        Ok(())
    }
}

/// Open a candidate path, which must name a regular file.
fn open_candidate(path: &Path) -> Result<File, OrbitError> {
    let unreadable = |error: std::io::Error| {
        OrbitError::InvalidInput(format!(
            "cannot read the local candidate '{}': {error}",
            path.display()
        ))
    };
    if !std::fs::metadata(path).map_err(unreadable)?.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "the local candidate '{}' is not a regular file",
            path.display()
        )));
    }
    File::open(path).map_err(unreadable)
}

/// The installed executable's file identity, compared again before the swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TargetIdentity {
    device: u64,
    inode: u64,
}

/// Confirm the explicit install target is an executable Orbit's installer owns
/// and this user may replace, without following a symlink at that path.
fn inspect_install_target(environment: &UpdateEnvironment) -> Result<TargetIdentity, OrbitError> {
    let target = &environment.executable;
    let refuse = |reason: String| {
        OrbitError::InvalidInput(format!(
            "cannot install a local candidate over '{}': {reason}",
            target.display()
        ))
    };
    if target.file_name().is_none_or(|name| name != INSTALLED_NAME) {
        return Err(refuse(format!(
            "the install target must be the managed `{INSTALLED_NAME}` executable"
        )));
    }
    let metadata = std::fs::symlink_metadata(target).map_err(|error| {
        refuse(format!(
            "{error}; a local candidate replaces an existing managed installation, so install \
             one with install.sh first"
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(refuse(
            "it is a symbolic link; name the managed executable itself".to_string(),
        ));
    }
    if !metadata.is_file() {
        return Err(refuse("it is not a regular file".to_string()));
    }
    if !matches!(environment.install_channel, InstallChannel::Managed { .. }) {
        return Err(refuse(format!(
            "{} owns it; local candidates replace only the managed installation in \
             ORBIT_INSTALL_DIR (default ~/.orbit/bin)",
            environment.install_channel.as_str()
        )));
    }
    identity_of(&metadata).map_err(refuse)
}

#[cfg(unix)]
fn identity_of(metadata: &std::fs::Metadata) -> Result<TargetIdentity, String> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid has no preconditions and cannot fail.
    let user = unsafe { libc::geteuid() };
    if metadata.uid() != user {
        return Err(format!(
            "it is owned by uid {}, not this user (uid {user}); run the update as the \
             installation's owner",
            metadata.uid()
        ));
    }
    Ok(TargetIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn identity_of(_metadata: &std::fs::Metadata) -> Result<TargetIdentity, String> {
    Err("local candidates are supported on Linux and macOS only".to_string())
}

/// Install the local candidate `request` names over `environment.executable`.
pub fn run_local_candidate_update(
    environment: &UpdateEnvironment,
    request: &LocalCandidateRequest,
) -> Result<UpdateReport, OrbitError> {
    let expected_commit = normalize_source_commit(&request.source_commit)?;
    let target = environment.executable.as_path();
    // Refuse an unowned target before admission, which creates lock files.
    inspect_install_target(environment)?;

    let admissions = acquire_admissions(&environment.admission_roots)?;
    let install_dir = target.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!("'{}' has no parent directory", target.display()))
    })?;
    let _lock = UpdateLock::acquire(install_dir)?;
    let identity = inspect_install_target(environment)?;
    let current = locked_installed_version(target)?;
    let installed_before = executable_generation(target)?;

    // Read the candidate once, into the staging file beside the target: what
    // is digested below is exactly what a swap would install.
    let staged = stage_executable(
        target,
        open_candidate(&request.candidate)?,
        MAX_CANDIDATE_BYTES,
    )?;
    let candidate = AcceptedCandidate::describe(staged.path())?;
    let manifest = CandidateManifest::parse(&read_bounded(
        File::open(&request.manifest).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "cannot read the local-candidate manifest '{}': {error}",
                request.manifest.display()
            ))
        })?,
        "the local-candidate manifest",
        MAX_METADATA_BYTES,
    )?)?;
    candidate.verify(&manifest, &expected_commit, &environment.target_triple)?;

    let mut report = local_report(
        environment,
        request,
        &expected_commit,
        &candidate,
        &current,
        installed_before.clone(),
    );

    if installed_before == candidate.sha256 {
        // The accepted bytes are already installed: the resume of a run whose
        // convergence did not finish, or a replay. Converge, never re-swap.
        drop(staged);
        let compatibility = require_admission_contract(target)?;
        let _generations = pin_candidate(
            &environment.admission_roots,
            admissions,
            &candidate.sha256,
            compatibility.as_ref(),
        )?;
        set_installed_after(&mut report, installed_before);
        return Ok(finish(
            environment,
            target,
            report,
            UpdateOutcome::AlreadyCurrent,
        ));
    }

    let compatibility = require_admission_contract(staged.path())?;
    let reported = probe_version(staged.path())
        .and_then(|reported| ReleaseVersion::parse(&reported))
        .map_err(|error| {
            OrbitError::Execution(format!(
                "the staged local candidate could not be verified ({error}); nothing was replaced"
            ))
        })?;
    report.target_version = reported.to_string();
    if reported < current {
        if !request.allow_downgrade {
            return Err(OrbitError::InvalidInput(format!(
                "the local candidate reports orbit {reported}, older than the installed \
                 {current}; an older binary cannot open state a newer one has migrated. Pass \
                 --allow-downgrade to attempt it anyway (it is checked against this workspace \
                 before anything is replaced)"
            )));
        }
        assert_downgrade_is_compatible(environment, staged.path(), &current, &reported)?;
    }

    // The swap installs the staged file, so hold it — and the target it
    // replaces — to what was accepted, immediately before the rename.
    if executable_generation(staged.path())? != candidate.sha256 {
        return Err(OrbitError::Execution(
            "the staged local candidate changed after it was verified; nothing was replaced".into(),
        ));
    }
    if inspect_install_target(environment)? != identity {
        return Err(OrbitError::Execution(format!(
            "'{}' was replaced by another process during this update; nothing was replaced",
            target.display()
        )));
    }
    let backup = backup_path(target);
    staged.commit(target, &backup)?;
    report.replaced = true;
    report.backup_path = Some(backup.clone());

    // Before any workspace state: the installed path must hold the accepted
    // bytes and report the version the staged copy did.
    let installed = executable_generation(target).and_then(|digest| {
        let version = probe_version(target).and_then(|v| ReleaseVersion::parse(&v))?;
        Ok((digest, version))
    });
    match installed {
        Ok((digest, version)) if digest == candidate.sha256 && version == reported => {
            set_installed_after(&mut report, digest);
        }
        Ok((digest, version)) => {
            restore_backup(target, &backup)?;
            return Err(OrbitError::Execution(format!(
                "the installed executable is {digest} reporting orbit {version}, not the accepted \
                 candidate {} reporting {reported}; restored the previous executable and changed \
                 no workspace state",
                candidate.sha256
            )));
        }
        Err(error) => {
            restore_backup(target, &backup)?;
            return Err(OrbitError::Execution(format!(
                "the installed local candidate could not be verified ({error}); restored the \
                 previous executable and changed no workspace state"
            )));
        }
    }

    let _generations = match pin_candidate(
        &environment.admission_roots,
        admissions,
        &candidate.sha256,
        compatibility.as_ref(),
    ) {
        Ok(guards) => guards,
        Err(error) => {
            let retry = report
                .local_candidate
                .as_ref()
                .map(|local| local.retry_command.clone())
                .unwrap_or_default();
            report.outcome = UpdateOutcome::NeedsRecovery;
            report.recovery = Some(format!(
                "The local candidate was installed but candidate admission failed: {error}. No \
                 convergence was attempted. Re-run `{retry}` once the authority is free."
            ));
            return Ok(report);
        }
    };
    Ok(finish(environment, target, report, UpdateOutcome::Updated))
}

fn set_installed_after(report: &mut UpdateReport, digest: String) {
    if let Some(local) = report.local_candidate.as_mut() {
        local.installed_sha256_after = Some(digest);
    }
}

fn local_report(
    environment: &UpdateEnvironment,
    request: &LocalCandidateRequest,
    expected_commit: &str,
    candidate: &AcceptedCandidate,
    current: &ReleaseVersion,
    installed_before: String,
) -> UpdateReport {
    UpdateReport {
        install_channel: environment.install_channel.as_str(),
        updatable: true,
        remediation: None,
        executable: environment.executable.clone(),
        release_source: format!("local candidate ({OPERATOR_ATTESTED}, not a signed release)"),
        target: environment.target_triple.clone(),
        asset: request.candidate.display().to_string(),
        current_version: current.to_string(),
        target_version: current.to_string(),
        outcome: UpdateOutcome::AlreadyCurrent,
        replaced: false,
        archive_sha256: None,
        signing_key_id: None,
        backup_path: None,
        steps: Vec::new(),
        workspace_root: environment
            .workspace
            .as_ref()
            .map(|workspace| workspace.root.clone()),
        admission_roots: environment.admission_roots.clone(),
        local_candidate: Some(LocalCandidateEvidence {
            trust: OPERATOR_ATTESTED,
            signed_release: false,
            candidate: request.candidate.clone(),
            manifest: request.manifest.clone(),
            source_commit: Evidence {
                value: expected_commit.to_string(),
                evidence: OPERATOR_ATTESTED,
            },
            executable_sha256: Evidence {
                value: candidate.sha256.clone(),
                evidence: "computed_from_accepted_bytes",
            },
            target: Evidence {
                value: candidate.target.to_string(),
                evidence: "executable_header",
            },
            installed_sha256_before: installed_before,
            installed_sha256_after: None,
            retry_command: retry_command(environment, request, expected_commit),
        }),
        recovery: None,
    }
}

/// The exact invocation that retries this candidate. It runs the installed
/// executable, which holds the candidate's bytes once the swap has happened.
fn retry_command(
    environment: &UpdateEnvironment,
    request: &LocalCandidateRequest,
    expected_commit: &str,
) -> String {
    let mut words = vec![environment.executable.display().to_string()];
    if let Some(root) = environment
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.root_argument.as_deref())
    {
        words.extend(["--root".to_string(), root.display().to_string()]);
    }
    words.extend([
        "update".to_string(),
        "--local-candidate".to_string(),
        request.candidate.display().to_string(),
        "--candidate-manifest".to_string(),
        request.manifest.display().to_string(),
        "--source-commit".to_string(),
        expected_commit.to_string(),
        "--install-target".to_string(),
        environment.executable.display().to_string(),
    ]);
    if request.allow_downgrade {
        words.push("--allow-downgrade".to_string());
    }
    words
        .iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quote `word` for a POSIX shell when it holds anything beyond a safe set.
fn shell_word(word: &str) -> String {
    let safe = |byte: u8| byte.is_ascii_alphanumeric() || b"/._-+=:@,%".contains(&byte);
    if !word.is_empty() && word.bytes().all(safe) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}
