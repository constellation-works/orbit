use orbit_cmd::{OrphanTaskStoreRemoval, RetiredConfigKeyRepair};

/// Render `--fix-orphan-task-stores --confirm`'s outcome, naming the empty
/// and populated partition counts separately so a run that deleted task
/// bundles is never described as removing only empty partitions [ORB-12144].
pub(super) fn orphan_task_store_removal_message(removed: &OrphanTaskStoreRemoval) -> String {
    format!(
        "Removed {} empty orphaned task-store partition(s) and {} populated partition(s) \
         ({} task bundle(s)).",
        removed.empty_partitions, removed.populated_partitions, removed.task_bundles
    )
}

/// Render `--fix-retired-config-keys`: which replacement keys were written,
/// which keys were removed, and the backup written beside each edited file.
/// An empty repair changed nothing.
pub(super) fn retired_config_key_repair_message(repair: &RetiredConfigKeyRepair) -> String {
    if repair.removed.is_empty() && repair.written.is_empty() {
        return "No retired, deprecated or unknown config keys were present.".to_string();
    }
    let mut groups: Vec<(String, String, Vec<&str>)> = Vec::new();
    for entry in &repair.removed {
        let config = entry.config_path.display().to_string();
        let backup = entry.backup_path.display().to_string();
        if let Some(group) = groups.iter_mut().find(|group| group.0 == config) {
            group.2.push(entry.key.as_str());
        } else {
            groups.push((config, backup, vec![entry.key.as_str()]));
        }
    }
    let detail = groups
        .iter()
        .map(|(config, backup, keys)| {
            format!("{} from {config} (backup {backup})", keys.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let count = repair.removed.len();
    let removed = format!(
        "Removed {count} retired, deprecated or unknown config key{}: {detail}",
        if count == 1 { "" } else { "s" }
    );
    let written = repair
        .written
        .iter()
        .map(|entry| format!("{} in {}", entry.key, entry.config_path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    match (count, written.is_empty()) {
        (_, true) => format!("{removed}."),
        (0, false) => format!("Rewrote deprecated config values: {written}."),
        (_, false) => format!("{removed}; wrote their replacements first: {written}."),
    }
}

/// Render `--fix-automation-pins`'s outcome: what it reclaimed, why the rest
/// of its own namespace stayed, and every pin it retained without proof of
/// ownership.
pub(super) fn automation_pin_cleanup_message(
    cleanup: &orbit_core::application::automation::AttemptPinCleanup,
) -> String {
    let mut message = match &cleanup.refused {
        Some(reason) => format!(
            "Reclaimed no automation attempt pins: ownership of {} is not proved ({reason}); \
             retained {} owned pin(s).",
            cleanup.namespace, cleanup.retained_unproven,
        ),
        None => format!(
            "Reclaimed {} unreferenced automation attempt pin(s) under {}; kept {} in flight, \
             {} live-run, {} assessed.",
            cleanup.released.len(),
            cleanup.namespace,
            cleanup.retained_active,
            cleanup.retained_live_run,
            cleanup.retained_assessed,
        ),
    };
    message.push_str(&format!(
        " Retained {} legacy shared pin(s) under refs/orbit/automation/ with no recorded owner \
         and {} pin namespace(s) owned by other Orbit roots or workspaces.",
        cleanup.retained_legacy, cleanup.foreign_owners,
    ));
    if !cleanup.kept.is_empty() {
        message.push_str(&format!(
            " Could not delete (moved or locked): {}.",
            cleanup.kept.join(", ")
        ));
    }
    if !cleanup.unrecognized.is_empty() {
        message.push_str(&format!(
            " Left unrecognized refs untouched: {}.",
            cleanup.unrecognized.join(", ")
        ));
    }
    message
}
