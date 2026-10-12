//! Durable, workspace-wide flaky-signature reporting.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_store::contracts::FrictionAddParams;
use orbit_types::record::FrictionStatus;
use orbit_types::workflow::FlakyValidation;

use crate::OrbitRuntime;

/// The friction store's atomic add-or-reuse deduplicates each command/test
/// signature across runs, tasks, and local/claimed delivery in this workspace.
pub(crate) fn record_frictions(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &FlakyValidation,
) -> Result<(), OrbitError> {
    if !record.valid() {
        return Err(OrbitError::InvalidInput(
            "invalid flaky validation evidence".into(),
        ));
    }
    let store = crate::runtime::friction::store_for(runtime)?;
    for failure in &record.failures {
        let key = format!(
            "flaky-test:{}",
            sha256_hex(format!("{}\0{failure}", record.command.trim()).as_bytes())
        );
        store.add_or_reuse(&key, FrictionAddParams {
            model: "system".into(),
            title: Some(format!("Flaky test: {}", failure.strip_prefix("test ").unwrap_or(failure))),
            body: format!("dedupe-key: {key}\n\nRequired command `{}` failed `{failure}` on candidate `{}`, passed on base `{}`, then passed one serial isolated candidate replay ({} executed tests).\n\nEvidence: {}\n", record.command, record.tested_head, record.base_sha, record.rerun_tests_run, record.text()),
            tags: vec![], status: FrictionStatus::Open,
            during_task: Some(task_id.into()), created_at: Utc::now(),
        })?;
    }
    Ok(())
}
