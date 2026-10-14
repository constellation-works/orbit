//! Generic command-reported diagnostic bytes, captured before temporary storage
//! ends and retained through the existing trusted validation artifact boundary.
//! These bytes never decide coverage, a command result or baseline equivalence.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path};

use orbit_common::OrbitError;
use orbit_common::security::{redaction::redact_all, release::sha256_hex};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

pub(super) const DIRECTORY_ENV: &str = "ORBIT_COMMAND_EVIDENCE_DIR";
pub(super) const INVOCATION_ENV: &str = "ORBIT_COMMAND_EVIDENCE_INVOCATION";
// Use the existing inline read ceiling for the entire invocation, not per file.
const MAX_BYTES: usize = orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES as usize;
const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MAX_FILES: usize = 8;
// Worst-case JSON escaping and byte-array claim transport stay below the
// existing artifact/transport ceilings. No command can choose these limits.
const CHUNK_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    invocation: String,
    files: Vec<ReportedFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportedFile {
    name: String,
    source: String,
    bytes: usize,
    sha256: String,
    complete: bool,
}

fn flat_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && name != "."
        && name != ".."
}

fn source_path(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 512
        && Path::new(source)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && !source.contains(['\\', '\0', '\n', '\r'])
}

#[cfg(unix)]
fn read_at(anchor: &File, name: &str, limit: usize) -> io::Result<Vec<u8>> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = CString::new(name).map_err(io::Error::other)?;
    // SAFETY: the anchor remains live and name is NUL terminated. Flat names
    // cannot traverse parents; NOFOLLOW and NONBLOCK refuse symlinks and FIFOs.
    let fd = unsafe {
        libc::openat(
            anchor.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor exclusively owned here.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("command evidence is not a regular file"));
    }
    if file.metadata()?.len() > limit as u64 {
        return Err(io::Error::other(
            "command evidence exceeds its fixed byte ceiling",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other(
            "command evidence grew beyond its fixed byte ceiling",
        ));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_at(_anchor: &File, _name: &str, _limit: usize) -> io::Result<Vec<u8>> {
    Err(io::Error::other(
        "anchored command evidence reads unavailable on this platform",
    ))
}

pub(super) fn capture(anchor: &File, invocation: &str, workspace: &Path) -> Value {
    let result = (|| -> Result<Value, String> {
        let bytes = match read_at(anchor, "manifest.json", MAX_MANIFEST_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(json!({"status": "not_reported", "complete": false}));
            }
            Err(error) => return Err(error.to_string()),
        };
        let report: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| "command evidence manifest is invalid")?;
        if report.schema_version != 1 || report.invocation != invocation {
            return Err("command evidence schema or invocation does not match".into());
        }
        if report.files.is_empty() || report.files.len() > MAX_FILES {
            return Err("command evidence file count is outside its fixed ceiling".into());
        }
        let mut remaining = MAX_BYTES;
        let mut names = BTreeSet::new();
        let mut files = Vec::new();
        for report in report.files {
            if !flat_name(&report.name)
                || report.name == "manifest.json"
                || !source_path(&report.source)
                || !names.insert(report.name.clone())
            {
                return Err("command evidence name/source is not confined or is repeated".into());
            }
            if report.bytes > remaining {
                return Err("command evidence exceeds its fixed total byte ceiling".into());
            }
            let bytes =
                read_at(anchor, &report.name, remaining).map_err(|error| error.to_string())?;
            if bytes.len() != report.bytes || sha256_hex(&bytes) != report.sha256 {
                return Err("command evidence bytes/digest do not match the report".into());
            }
            remaining -= bytes.len();
            // A reported prefix can end before a credential pattern's minimum
            // length or closing delimiter, including in a multiline value or
            // UTF-8 scalar. No suffix/window proves the earlier bytes safe.
            // Withhold the entire partial payload before memory/cache retention;
            // keep its verified source identity and honest incompleteness.
            let retained = if report.complete {
                let text =
                    std::str::from_utf8(&bytes).map_err(|_| "command evidence is not UTF-8")?;
                redact_all(text)
            } else {
                String::new()
            };
            let redacted = retained.as_bytes() != bytes;
            if retained.len() > MAX_BYTES {
                return Err("redacted command evidence exceeds its fixed byte ceiling".into());
            }
            files.push(json!({
                "name": redact_all(&report.name), "source": redact_all(&report.source),
                "source_authority": "command_reported",
                "source_bytes": bytes.len(), "source_sha256": report.sha256,
                "complete": report.complete,
                "raw_complete": report.complete && !redacted,
                "redacted": redacted,
                "partial_source_withheld": !report.complete,
                "retained_bytes": retained.len(), "retained_sha256": sha256_hex(retained.as_bytes()),
                // Private in-memory/cache payload, removed before artifact attachment.
                "payload": retained,
            }));
        }
        // Redaction can expand short credentials. The aggregate retention
        // ceiling applies to the stored bytes as well as to the source reads.
        if files
            .iter()
            .filter_map(|file| file["retained_bytes"].as_u64())
            .sum::<u64>()
            > MAX_BYTES as u64
        {
            return Err("redacted command evidence exceeds its fixed total byte ceiling".into());
        }
        Ok(json!({"status": "captured", "invocation": invocation,
            "source_workspace": redact_all(&workspace.to_string_lossy()), "files": files,
            "complete": files.iter().all(|file| file["complete"] == true)}))
    })();
    result.unwrap_or_else(
        |reason| json!({"status": "refused", "complete": false, "reason": redact_all(&reason)}),
    )
}

/// Attach every chunk before publishing a manifest that names it. Any failed
/// or unknown write is recorded as incomplete; there is no retry or successful completeness
/// claim when attachment did not conclusively complete.
pub(super) fn retain<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    run_id: &str,
    log_path: &str,
    content: &[u8],
    claimed: bool,
) -> Result<Vec<u8>, OrbitError> {
    let started = std::time::Instant::now();
    let mut had_evidence = false;
    let mut log: Value = serde_json::from_slice(content)
        .map_err(|error| OrbitError::Execution(format!("decode validation evidence: {error}")))?;
    let provenance = json!({
        "run_id": run_id,
        "command": log["command"].as_str().map(redact_all),
        "command_sha256": log["command"].as_str().map(|command| sha256_hex(command.as_bytes())),
        "role": log.get("role").cloned().unwrap_or_else(|| json!("candidate")),
        "tested_head": log.get("tested_head").or_else(|| log.get("base_sha")),
        "validation_log": log_path,
    });
    if let Some(attempts) = log["timing"]["attempts"].as_array_mut() {
        for (attempt, value) in attempts.iter_mut().enumerate() {
            let evidence = &mut value["command_evidence"];
            if evidence["status"] != "captured" {
                continue;
            }
            had_evidence = true;
            let invocation = evidence["invocation"].clone();
            let source_workspace = evidence["source_workspace"].clone();
            let mut captured_files = evidence["files"].clone();
            if let Some(files) = captured_files.as_array_mut() {
                for file in files {
                    if let Some(fields) = file.as_object_mut() {
                        fields.remove("payload");
                        fields.remove("chunks");
                        fields.remove("manifests");
                    }
                    file["reported_complete"] = file["complete"].clone();
                    file["complete"] = json!(false);
                    file["raw_complete"] = json!(false);
                }
            }
            let retained = (|| -> Result<(), OrbitError> {
                if let Some(files) = evidence["files"].as_array_mut() {
                    if files.is_empty() || files.len() > MAX_FILES {
                        return Err(OrbitError::Execution(
                            "cached command evidence file count refused".into(),
                        ));
                    }
                    let mut remaining = MAX_BYTES;
                    for file in files {
                        let payload = file
                            .as_object_mut()
                            .and_then(|file| file.remove("payload"))
                            .ok_or_else(|| {
                                OrbitError::Execution("command evidence payload unavailable".into())
                            })?;
                        let text = payload.as_str().ok_or_else(|| {
                            OrbitError::Execution("command evidence payload is not text".into())
                        })?;
                        if text.len() > remaining
                            || file["retained_bytes"].as_u64() != Some(text.len() as u64)
                            || file["retained_sha256"] != sha256_hex(text.as_bytes())
                        {
                            return Err(OrbitError::Execution(
                                "cached command evidence bytes/digest refused".into(),
                            ));
                        }
                        remaining -= text.len();
                        let mut offset = 0;
                        let mut chunks = Vec::new();
                        // At least one chunk also represents an empty, complete file.
                        loop {
                            let end = orbit_common::text::floor_char_boundary(
                                text,
                                (offset + CHUNK_BYTES).min(text.len()),
                            );
                            let data = &text[offset..end];
                            let chunk = json!({"schema_version": 1, "encoding": "utf-8", "offset": offset,
                            "bytes": data.len(), "sha256": sha256_hex(data.as_bytes()), "content": data});
                            let bytes = serde_json::to_vec(&chunk)
                                .map_err(|error| OrbitError::Execution(error.to_string()))?;
                            let path =
                                format!("validation/{run_id}/evidence/{}.json", sha256_hex(&bytes));
                            attach(host, task_ids, run_id, &path, &bytes, claimed)?;
                            chunks.push(json!({"path": path, "offset": offset, "bytes": data.len(),
                            "sha256": sha256_hex(data.as_bytes()), "artifact_sha256": sha256_hex(&bytes)}));
                            offset = end;
                            if offset == text.len() {
                                break;
                            }
                        }
                        file["chunks"] = json!(chunks);
                        let mut manifests = Vec::new();
                        for task_id in task_ids {
                            let manifest = json!({"schema_version": 1, "task_id": task_id,
                            "provenance": provenance, "invocation": invocation, "source_workspace": source_workspace,
                            "attempt": attempt, "file": file});
                            let bytes = serde_json::to_vec(&manifest)
                                .map_err(|error| OrbitError::Execution(error.to_string()))?;
                            let path = format!(
                                "validation/{run_id}/evidence/{}.manifest.json",
                                sha256_hex(&bytes)
                            );
                            attach(
                                host,
                                std::slice::from_ref(task_id),
                                run_id,
                                &path,
                                &bytes,
                                claimed,
                            )?;
                            // Each task receives an exact task-bound manifest; shared
                            // canonical logs name the task and its durable path together.
                            manifests.push(json!({"task_id": task_id, "path": path, "sha256": sha256_hex(&bytes)}));
                        }
                        file["manifests"] = json!(manifests);
                    }
                }
                Ok(())
            })();
            if let Err(error) = retained {
                // Unknown writes may already have stored a chunk. Never retry,
                // publish a complete manifest, or alter the command verdict.
                *evidence = json!({"status": "attachment_unknown", "complete": false,
                    "invocation": invocation, "source_workspace": source_workspace,
                    "captured_files": captured_files, "reason": redact_all(&error.to_string())});
            }
        }
    }
    if had_evidence {
        log["timing"]["evidence_retention_ms"] = json!(started.elapsed().as_millis());
    }
    serde_json::to_vec(&log).map_err(|error| OrbitError::Execution(error.to_string()))
}

fn attach<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    run_id: &str,
    path: &str,
    bytes: &[u8],
    claimed: bool,
) -> Result<(), OrbitError> {
    orbit_types::task::validate_relative_artifact_path(path)?;
    if bytes.len() > MAX_BYTES {
        return Err(OrbitError::PolicyDenied(
            "command evidence artifact exceeds the fixed inline ceiling".into(),
        ));
    }
    if claimed {
        let claim = host.claim_execution_context()?;
        if task_ids != std::slice::from_ref(&claim.task_id) || run_id != claim.run_id {
            return Err(OrbitError::PolicyDenied(
                "command evidence does not belong to the bound claim".into(),
            ));
        }
        host.attach_claim_validation_log(path, bytes.to_vec())
    } else {
        for task_id in task_ids {
            host.attach_task_validation_log(task_id, run_id, path, bytes.to_vec())?;
        }
        Ok(())
    }
}
