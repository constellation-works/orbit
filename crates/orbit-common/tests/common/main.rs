//! Orbit-common integration coverage in one area binary.
//!
//! Filesystem, process, generation and test-environment cases share this
//! target. None of them installs a signal handler or mutates the process
//! environment outside `orbit_common::test_env`, so a separate binary is
//! not warranted. One load-time scrub covers the process.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

orbit_common::isolate_test_process!();

use chrono::DateTime;
use chrono::Utc;
use fs2::FileExt;
use orbit_common::ClaimRefusalKind;
use orbit_common::OrbitError;
use orbit_common::StorageLayer;
use orbit_common::derive_cost_usd;
use orbit_common::fs::directory_lock::DirectoryLockOptions;
use orbit_common::fs::directory_lock::with_directory_lock_options;
use orbit_common::fs::generation::Access;
use orbit_common::fs::generation::CompatibilityIdentity;
use orbit_common::fs::generation::FeatureFloor;
use orbit_common::fs::generation::GenerationGuard;
use orbit_common::fs::generation::GenerationUpdate;
use orbit_common::fs::generation::HandoverCandidate;
use orbit_common::fs::generation::LedgerCompatibility;
use orbit_common::fs::generation::Participant;
use orbit_common::fs::generation::ParticipantRole;
use orbit_common::fs::generation::RESUME_MCP_STDIO;
use orbit_common::fs::generation::pending_switch;
use orbit_common::fs::git::run_git;
#[cfg(unix)]
use orbit_common::fs::git::run_git_within;
use orbit_common::fs::io::FileLockOptions;
use orbit_common::fs::io::with_exclusive_file_lock_options;
use orbit_common::fs::io::with_shared_file_lock_options;
use orbit_common::fs::io::write_new_private_text;
use orbit_common::fs::lock_wait::measure;
use orbit_common::fs::lock_wait::record;
use orbit_common::fs::reverse_lines::REVERSE_READ_BLOCK;
use orbit_common::fs::reverse_lines::ReverseLines;
use orbit_common::observability::logging;
#[cfg(unix)]
use orbit_common::process::OwnedChild;
use orbit_common::process::build_budget::read_waits;
#[cfg(unix)]
use orbit_common::process::identity::KernelLiveness;
#[cfg(unix)]
use orbit_common::process::identity::probe_process_group_liveness;
#[cfg(unix)]
use orbit_common::process::run_bounded;
#[cfg(unix)]
use orbit_common::process::shell::quote_posix_arg;
#[cfg(unix)]
use orbit_common::process::supervisor::CaptureSink;
#[cfg(unix)]
use orbit_common::process::supervisor::Periodic;
#[cfg(unix)]
use orbit_common::process::supervisor::ProcessStopCause;
#[cfg(unix)]
use orbit_common::process::supervisor::Stdin;
#[cfg(unix)]
use orbit_common::process::supervisor::Stream;
#[cfg(unix)]
use orbit_common::process::supervisor::SupervisedChild;
#[cfg(unix)]
use orbit_common::process::supervisor::Supervision;
#[cfg(unix)]
use orbit_common::process::supervisor::Termination;
#[cfg(unix)]
use orbit_common::storage::blob_store::BlobStore;
#[cfg(unix)]
use orbit_common::storage::blob_sweep;
#[cfg(unix)]
use orbit_common::storage::blob_sweep::AuditBlobRoot;
#[cfg(unix)]
use orbit_common::storage::blob_sweep::SweepEntry;
#[cfg(unix)]
use orbit_common::storage::blob_sweep::SweepOutcome;
use orbit_common::test_env;
use orbit_common::test_env::FixtureProgress;
use orbit_common::test_env::INHERITED_AUTHORITY_ENV;
use orbit_common::test_env::MANAGED_RUN_ENV;
use orbit_common::test_env::SCRUBBED_MARKER_ENV;
use orbit_common::test_env::assert_child_test_exists;
use orbit_common::test_env::assert_child_test_passed;
use orbit_common::test_env::clear_inherited_authority;
use orbit_common::test_env::run_child_test;
use orbit_types::task::ArtifactManifestV2;
use orbit_types::task::TaskCommentRowV2;
use orbit_types::task::TaskEnvelopeV2;
use orbit_types::task::TaskEventRowV2;
use orbit_types::telemetry::TokenUsage;
use serde_json::Value;
use serde_json::json;
use std::alloc::GlobalAlloc;
use std::alloc::Layout;
use std::alloc::System;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs as std_fs;
use std::fs::File;
use std::fs::FileTimes;
use std::fs::OpenOptions;
use std::io;
#[cfg(unix)]
use std::io::BufRead;
use std::io::Cursor;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Child;
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc::sync_channel;
#[cfg(unix)]
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

#[path = "mechanisms/fs.rs"]
mod fs;
#[path = "mechanisms/governance.rs"]
mod governance;
#[path = "mechanisms/migration.rs"]
mod migration;
#[path = "mechanisms/observability.rs"]
mod observability;
#[cfg(unix)]
#[path = "mechanisms/process.rs"]
mod process;
#[path = "mechanisms/security.rs"]
mod security;
#[cfg(feature = "sqlite")]
#[path = "mechanisms/sqlite.rs"]
mod sqlite;
#[path = "mechanisms/storage.rs"]
mod storage;

#[cfg(unix)]
include!("audit_blob_sweep.rs");
include!("build_budget.rs");
include!("directory_lock.rs");
include!("error_classification.rs");
include!("file_lock_priority.rs");
include!("generation_admission.rs");
include!("generation_participation.rs");
include!("git_argv.rs");
#[cfg(unix)]
include!("git_deadline.rs");
include!("jsonl_exit_flush.rs");
include!("lock_wait.rs");
include!("new_private_file.rs");
include!("pricing.rs");
#[cfg(unix)]
include!("process_group_liveness.rs");
include!("process_supervisor.rs");
#[cfg(unix)]
include!("process_supervisor_periodic.rs");
include!("reverse_lines.rs");
include!("task_artifacts_v2.rs");
include!("test_env.rs");

fn json_string_body(value: &str) -> String {
    let encoded = serde_json::to_string(value).expect("json string");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("json quotes")
        .to_string()
}

/// `value` as the body of a Rust `Debug` string, without its quotes.
fn debug_string_body(value: &str) -> String {
    let encoded = format!("{value:?}");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("debug quotes")
        .to_string()
}
