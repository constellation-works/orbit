//! The run-owned store for a script step's declared output files (script
//! steps §4 "Files out").
//!
//! After a successful attempt whose stdout the step accepts, the host
//! copies every top-level regular file of the sandbox's `out/` into
//! `<state>/scripts/outputs/<run>/<set_id>/files/` with its size, media type
//! and SHA-256, and writes the producer record (`producer.json`, the
//! [`ScriptOutputSet`] descriptor) beside them. The copy is staged under a
//! private name and renamed into place only once every file and the record
//! are written, so a set is complete or absent: a later step never sees
//! part of one. Bytes are copied verbatim; nothing here decodes, redacts or
//! rewrites them, which is why the audit blob store (UTF-8, redacting) is
//! not used.
//!
//! A descriptor in a run's pipeline state is a claim, not authority:
//! [`verify_script_output_set`] honours it only for the consuming run or a
//! run it resumes, and only while the producer record equals it and every
//! file still hashes to its recorded digest.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use orbit_engine::activity_job::script_stdout_output;
use orbit_engine::{ScriptStepEnd, ScriptStepOutputs, ScriptStepRan, ScriptStepRequest};
use orbit_types::workflow::activity_job::{
    SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES, SCRIPT_OUTPUT_HARD_MAX_FILES, ScriptFileOutputs,
    ScriptGrant, ScriptGrantLimits, ScriptOutputFile, ScriptOutputSet, is_valid_script_output_name,
    script_failure, script_output_media_type, script_output_set_id, script_output_total_cap,
};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;

const PRODUCER_RECORD: &str = "producer.json";
const FILES_DIR: &str = "files";
const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// The caps one attempt's output set is held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScriptOutputCaps {
    max_files: u32,
    max_file_bytes: u64,
    max_total_bytes: u64,
}

impl ScriptOutputCaps {
    /// The manifest's caps where it sets them, otherwise the grant's, never
    /// above the grant or the hard ceilings. The caller has already refused
    /// a manifest that asks for more than the grant holds.
    fn effective(declared: &ScriptFileOutputs, limits: &ScriptGrantLimits) -> Self {
        let max_files = declared
            .max_files
            .unwrap_or(limits.output_max_files)
            .min(limits.output_max_files)
            .min(SCRIPT_OUTPUT_HARD_MAX_FILES);
        let max_file_bytes = declared
            .max_file_bytes
            .unwrap_or(limits.output_max_file_bytes)
            .min(limits.output_max_file_bytes)
            .min(SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES);
        Self {
            max_files,
            max_file_bytes,
            max_total_bytes: script_output_total_cap(max_files, max_file_bytes),
        }
    }
}

/// Who produced one output set.
struct ScriptOutputProducer<'a> {
    workspace_id: &'a str,
    run_id: &'a str,
    step_id: &'a str,
}

/// Where every output set of this workspace lives.
fn outputs_root(runtime: &OrbitRuntime) -> PathBuf {
    runtime.paths().state_dir.join("scripts").join("outputs")
}

fn run_dir(runtime: &OrbitRuntime, run_id: &str) -> PathBuf {
    outputs_root(runtime).join(super::script_step::path_segment(run_id))
}

/// Whether `set_id` has the shape [`script_output_set_id`] gives, so a
/// forged descriptor cannot name a path outside its run directory.
fn is_set_id(set_id: &str) -> bool {
    set_id
        .strip_prefix("so-")
        .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Decide what becomes of one attempt's `out/`, before the scratch
/// directory goes. Only a manifest that declares outputs, and only an
/// attempt the step will accept (exit 0 with stdout that passes the step's
/// output check), keeps anything; every other attempt leaves
/// [`ScriptStepOutputs::None`] and its `out/` is discarded with the scratch.
pub(crate) fn keep_declared_outputs(
    runtime: &OrbitRuntime,
    declared: &ScriptFileOutputs,
    grant: &ScriptGrant,
    request: &ScriptStepRequest,
    out_dir: &Path,
    ran: &mut ScriptStepRan,
) {
    if !declared.declared()
        || ran.end != ScriptStepEnd::Exited(0)
        || script_stdout_output(&ran.stdout, ran.stdout_over_cap, &ran.output_schema).is_err()
    {
        return;
    }
    ran.outputs = keep_script_outputs(
        runtime,
        out_dir,
        &ScriptOutputProducer {
            workspace_id: &grant.workspace_id,
            run_id: &request.run_id,
            step_id: &request.step_id,
        },
        ScriptOutputCaps::effective(declared, &grant.limits),
    );
}

/// Keep the files of `out_dir` as one output set, or reject the whole set.
fn keep_script_outputs(
    runtime: &OrbitRuntime,
    out_dir: &Path,
    producer: &ScriptOutputProducer<'_>,
    caps: ScriptOutputCaps,
) -> ScriptStepOutputs {
    let entries = match admitted_entries(out_dir, caps) {
        Ok(entries) => entries,
        Err(rejected) => return rejected,
    };
    let run_dir = run_dir(runtime, producer.run_id);
    let nonce = getrandom::u64().unwrap_or_else(|_| u64::from(std::process::id()));
    let staging = run_dir.join(format!(".staging-{nonce:016x}"));
    let outcome = stage_and_publish(&run_dir, &staging, out_dir, &entries, producer, caps);
    if outcome.is_err() && staging.exists() {
        remove_tree(&staging);
    }
    match outcome {
        Ok(set) => ScriptStepOutputs::Kept(set),
        Err(rejected) => rejected,
    }
}

/// One `out/` entry that passed the listing checks.
struct AdmittedEntry {
    name: String,
    len: u64,
    #[cfg(unix)]
    identity: (u64, u64),
}

fn rejected(code: &'static str, message: impl Into<String>) -> ScriptStepOutputs {
    ScriptStepOutputs::Rejected {
        code,
        message: message.into(),
    }
}

fn unavailable(message: impl Into<String>) -> ScriptStepOutputs {
    rejected(script_failure::OUTPUTS_UNAVAILABLE, message)
}

/// List `out/`, refusing anything but safely named top-level regular files
/// within the caps. Symlinks are never followed.
fn admitted_entries(
    out_dir: &Path,
    caps: ScriptOutputCaps,
) -> Result<Vec<AdmittedEntry>, ScriptStepOutputs> {
    let invalid = |message: String| rejected(script_failure::OUTPUTS_INVALID, message);
    let over_cap = |message: String| rejected(script_failure::OUTPUTS_OVER_CAP, message);
    let listing = std::fs::read_dir(out_dir)
        .map_err(|error| unavailable(format!("its out/ directory is unreadable: {error}")))?;
    let mut entries = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|error| unavailable(format!("listing out/ failed: {error}")))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return Err(invalid(
                "out/ holds a file whose name is not UTF-8".to_string(),
            ));
        };
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| unavailable(format!("out/{name} is unreadable: {error}")))?;
        let kind = metadata.file_type();
        if kind.is_symlink() {
            return Err(invalid(format!(
                "out/{name} is a symlink; output files must be regular files"
            )));
        }
        if kind.is_dir() {
            return Err(invalid(format!(
                "out/{name} is a directory; output files must be top-level regular files"
            )));
        }
        if !kind.is_file() {
            return Err(invalid(format!("out/{name} is not a regular file")));
        }
        if !is_valid_script_output_name(&name) {
            return Err(invalid(format!(
                "out/{name} is not a safe output name (ASCII letters, digits, `.`, `_` and \
                 `-`, not starting with `.`, at most 128 bytes)"
            )));
        }
        entries.push(AdmittedEntry {
            name,
            len: metadata.len(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    if entries.len() > caps.max_files as usize {
        return Err(over_cap(format!(
            "out/ holds {} files and the step may keep at most {}",
            entries.len(),
            caps.max_files
        )));
    }
    if let Some(entry) = entries.iter().find(|entry| entry.len > caps.max_file_bytes) {
        return Err(over_cap(format!(
            "out/{} is {} bytes and an output file may be at most {}",
            entry.name, entry.len, caps.max_file_bytes
        )));
    }
    let total: u64 = entries.iter().map(|entry| entry.len).sum();
    if total > caps.max_total_bytes {
        return Err(over_cap(format!(
            "out/ holds {total} bytes and the step may keep at most {} in total",
            caps.max_total_bytes
        )));
    }
    Ok(entries)
}

fn stage_and_publish(
    run_dir: &Path,
    staging: &Path,
    out_dir: &Path,
    entries: &[AdmittedEntry],
    producer: &ScriptOutputProducer<'_>,
    caps: ScriptOutputCaps,
) -> Result<ScriptOutputSet, ScriptStepOutputs> {
    let staged_files = staging.join(FILES_DIR);
    orbit_common::fs::io::create_private_dir_all(&staged_files).map_err(|error| {
        unavailable(format!(
            "create output staging {}: {error}",
            staged_files.display()
        ))
    })?;
    let mut staged = Vec::with_capacity(entries.len());
    let mut total: u64 = 0;
    for entry in entries {
        let (bytes, sha256) = copy_entry(out_dir, entry, &staged_files, caps)?;
        total = total.saturating_add(bytes);
        if total > caps.max_total_bytes {
            return Err(rejected(
                script_failure::OUTPUTS_OVER_CAP,
                format!(
                    "out/ grew past the {} byte total while it was kept",
                    caps.max_total_bytes
                ),
            ));
        }
        staged.push((entry.name.clone(), bytes, sha256));
    }
    let mut files: Vec<ScriptOutputFile> = staged
        .into_iter()
        .map(|(name, bytes, sha256)| ScriptOutputFile {
            media_type: script_output_media_type(&name).to_string(),
            path: String::new(),
            name,
            bytes,
            sha256,
        })
        .collect();
    let set_id = script_output_set_id(
        producer.workspace_id,
        producer.run_id,
        producer.step_id,
        &files,
    );
    let final_dir = run_dir.join(&set_id);
    for file in &mut files {
        file.path = final_dir
            .join(FILES_DIR)
            .join(&file.name)
            .display()
            .to_string();
    }
    let set = ScriptOutputSet {
        workspace_id: producer.workspace_id.to_string(),
        run_id: producer.run_id.to_string(),
        step_id: producer.step_id.to_string(),
        set_id,
        files,
    };
    let record = serde_json::to_vec_pretty(&set)
        .map_err(|error| unavailable(format!("serialize the producer record: {error}")))?;
    orbit_common::fs::io::atomic_write_bytes(&staging.join(PRODUCER_RECORD), &record)
        .map_err(|error| unavailable(format!("write the producer record: {error}")))?;

    // The same producer and bytes give the same set id: a retry or a
    // re-run that produced identical files reuses the stored set when it
    // still verifies, and replaces it when it does not.
    if final_dir.exists() {
        if verify_stored_set(&final_dir, &set).is_ok() {
            remove_tree(staging);
            return Ok(set);
        }
        std::fs::remove_dir_all(&final_dir).map_err(|error| {
            unavailable(format!(
                "replace the damaged output set {}: {error}",
                final_dir.display()
            ))
        })?;
    }
    std::fs::rename(staging, &final_dir).map_err(|error| {
        unavailable(format!(
            "publish the output set {}: {error}",
            final_dir.display()
        ))
    })?;
    if let Ok(dir) = File::open(run_dir) {
        let _ = orbit_common::fs::io::sync_file(&dir);
    }
    Ok(set)
}

/// Copy one admitted entry byte for byte, hashing it on the way and never
/// following a link: the opened file must be the very inode the listing
/// admitted, and may not grow past its cap.
fn copy_entry(
    out_dir: &Path,
    entry: &AdmittedEntry,
    staged_files: &Path,
    caps: ScriptOutputCaps,
) -> Result<(u64, String), ScriptStepOutputs> {
    let source_path = out_dir.join(&entry.name);
    let unreadable =
        |error: std::io::Error| unavailable(format!("read out/{}: {error}", entry.name));
    let mut source = open_no_follow(&source_path).map_err(unreadable)?;
    let metadata = source.metadata().map_err(unreadable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_file() || (metadata.dev(), metadata.ino()) != entry.identity {
            return Err(rejected(
                script_failure::OUTPUTS_INVALID,
                format!("out/{} changed while it was kept", entry.name),
            ));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_file() {
        return Err(rejected(
            script_failure::OUTPUTS_INVALID,
            format!("out/{} is not a regular file", entry.name),
        ));
    }
    let target_path = staged_files.join(&entry.name);
    let mut target = create_private_file(&target_path)
        .map_err(|error| unavailable(format!("stage out/{}: {error}", entry.name)))?;
    let mut hasher = Sha256::new();
    let mut copied: u64 = 0;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = source.read(&mut buffer).map_err(unreadable)?;
        if read == 0 {
            break;
        }
        copied = copied.saturating_add(read as u64);
        if copied > caps.max_file_bytes {
            return Err(rejected(
                script_failure::OUTPUTS_OVER_CAP,
                format!(
                    "out/{} grew past the {} byte cap while it was kept",
                    entry.name, caps.max_file_bytes
                ),
            ));
        }
        hasher.update(&buffer[..read]);
        target
            .write_all(&buffer[..read])
            .map_err(|error| unavailable(format!("stage out/{}: {error}", entry.name)))?;
    }
    orbit_common::fs::io::sync_file(&target)
        .map_err(|error| unavailable(format!("sync out/{}: {error}", entry.name)))?;
    Ok((copied, orbit_types::identity::lower_hex(hasher.finalize())))
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn remove_tree(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path) {
        tracing::warn!(
            target: "orbit.core.job_run",
            path = %path.display(),
            error = %error,
            "script output staging directory was not removed",
        );
    }
}

/// How many `retry_source_run_id` hops a lineage check walks up from the
/// consuming run. Matches the resume planner's bound; a deeper chain is
/// treated as unrelated rather than scanned without limit.
const LINEAGE_MAX_HOPS: usize = 64;

/// Honour a descriptor only when `consumer_run_id` or one of its
/// `retry_source_run_id` ancestors produced it, this workspace's producer
/// record for it equals it, and every file still has its recorded size and
/// digest.
pub(crate) fn verify_script_output_set(
    runtime: &OrbitRuntime,
    consumer_run_id: &str,
    set: &ScriptOutputSet,
) -> Result<(), String> {
    let workspace_id = runtime
        .workspace_id()
        .map_err(|error| format!("cannot be checked: {error}"))?;
    if set.workspace_id != workspace_id {
        return Err(format!(
            "belongs to workspace {}, not this one",
            set.workspace_id
        ));
    }
    if !is_set_id(&set.set_id) || !set.identity_matches() {
        return Err("has no valid set identity".to_string());
    }
    if !run_in_lineage(runtime, consumer_run_id, &set.run_id)? {
        return Err(format!(
            "was produced by run {}, which is not run {consumer_run_id} or one of the runs \
             it resumes",
            set.run_id
        ));
    }
    let dir = run_dir(runtime, &set.run_id).join(&set.set_id);
    verify_stored_set(&dir, set)
}

/// Whether `producer_run_id` is `consumer_run_id` or one of its
/// `retry_source_run_id` ancestors.
fn run_in_lineage(
    runtime: &OrbitRuntime,
    consumer_run_id: &str,
    producer_run_id: &str,
) -> Result<bool, String> {
    let mut cursor = Some(consumer_run_id.to_string());
    for _ in 0..=LINEAGE_MAX_HOPS {
        let Some(run_id) = cursor.take() else {
            return Ok(false);
        };
        if run_id == producer_run_id {
            return Ok(true);
        }
        cursor = runtime
            .get_job_run_backend(&run_id)
            .map_err(|error| format!("cannot be checked: run {run_id}: {error}"))?
            .and_then(|run| run.retry_source_run_id);
    }
    Ok(false)
}

fn verify_stored_set(dir: &Path, set: &ScriptOutputSet) -> Result<(), String> {
    let record_path = dir.join(PRODUCER_RECORD);
    let raw = std::fs::read(&record_path).map_err(|error| {
        format!(
            "has no producer record at {}: {error}",
            record_path.display()
        )
    })?;
    let recorded: ScriptOutputSet = serde_json::from_slice(&raw).map_err(|error| {
        format!(
            "has an unreadable producer record at {}: {error}",
            record_path.display()
        )
    })?;
    if recorded != *set {
        return Err(format!(
            "does not match its producer record at {}",
            record_path.display()
        ));
    }
    for file in &set.files {
        let expected = dir.join(FILES_DIR).join(&file.name);
        if !is_valid_script_output_name(&file.name) || Path::new(&file.path) != expected {
            return Err(format!("names file `{}` outside its set", file.name));
        }
        let (bytes, sha256) = hash_stored_file(&expected)
            .map_err(|error| format!("lost file `{}`: {error}", file.name))?;
        if bytes != file.bytes || sha256 != file.sha256 {
            return Err(format!(
                "file `{}` changed: recorded {} bytes sha256 {}, found {bytes} bytes sha256 \
                 {sha256}",
                file.name, file.bytes, file.sha256
            ));
        }
    }
    Ok(())
}

fn hash_stored_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut file = open_no_follow(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((total, orbit_types::identity::lower_hex(hasher.finalize())))
}
